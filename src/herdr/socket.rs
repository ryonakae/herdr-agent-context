use super::MetadataReport;
use super::protocol::{self, AgentInfo, HerdrEvent, ProcessInfo, SessionSnapshot, TabInfo};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SocketError {
    #[error("Herdr socket I/O failed")]
    Io(#[from] std::io::Error),
    #[error("Herdr socket JSON was malformed")]
    Json(#[from] serde_json::Error),
    #[error("Herdr API returned {0}")]
    Api(String),
    #[error("Herdr socket closed")]
    Closed,
    #[error("Herdr API response did not match the request")]
    Protocol,
}

pub enum EventPoll {
    Event(HerdrEvent),
    Malformed,
    Timeout,
    Closed,
}

pub struct SocketTransport {
    rpc: RpcConnection,
    events: EventStream,
}

impl SocketTransport {
    pub fn connect(socket_path: &Path) -> Result<Self, SocketError> {
        let events = EventStream::subscribe(socket_path)?;
        let rpc = RpcConnection::connect(socket_path)?;
        Ok(Self { rpc, events })
    }

    pub fn list_agents(&mut self) -> Result<Vec<AgentInfo>, SocketError> {
        let result = self.rpc.call("agent.list", json!({}))?;
        protocol::parse_agents(result).map_err(SocketError::Json)
    }

    pub fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, SocketError> {
        let result = self
            .rpc
            .call("pane.process_info", protocol::process_info_params(pane_id))?;
        protocol::parse_process_info(result).map_err(SocketError::Json)
    }

    pub fn session_snapshot(&mut self) -> Result<SessionSnapshot, SocketError> {
        let result = self.rpc.call("session.snapshot", json!({}))?;
        protocol::parse_session_snapshot(result).map_err(SocketError::Json)
    }

    pub fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<TabInfo, SocketError> {
        let result = self
            .rpc
            .call("tab.rename", protocol::tab_rename_params(tab_id, label))?;
        protocol::parse_tab_info(result).map_err(SocketError::Json)
    }

    pub fn report_metadata(&mut self, report: MetadataReport<'_>) -> Result<(), SocketError> {
        let params = protocol::metadata_params(
            report.agent,
            report.pane_id,
            report.applies_to_source,
            report.seq,
            report.ttl_ms,
            report.session_name,
            report.last_message,
        );
        self.rpc.call("pane.report_metadata", params)?;
        Ok(())
    }

    pub fn poll_event(&self, timeout: Duration) -> EventPoll {
        self.events.poll(timeout)
    }
}

struct RpcConnection {
    socket_path: std::path::PathBuf,
    next_id: u64,
}

impl RpcConnection {
    fn connect(path: &Path) -> Result<Self, SocketError> {
        Ok(Self {
            socket_path: path.to_owned(),
            next_id: 1,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, SocketError> {
        let id = format!("agent-context:{}", self.next_id);
        self.next_id += 1;
        let stream = UnixStream::connect(&self.socket_path)?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        write_message(&mut writer, &protocol::request(&id, method, params))?;
        let value = read_message(&mut reader)?.ok_or(SocketError::Closed)?;
        if value.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(SocketError::Protocol);
        }
        protocol::parse_result(value).map_err(SocketError::Api)
    }
}

enum StreamItem {
    Value(Value),
    Malformed,
    Closed,
}

struct EventStream {
    receiver: Receiver<StreamItem>,
    control: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl EventStream {
    fn subscribe(path: &Path) -> Result<Self, SocketError> {
        let stream = UnixStream::connect(path)?;
        let control = stream.try_clone()?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let id = "agent-context:subscribe";
        write_message(
            &mut writer,
            &protocol::request(id, "events.subscribe", protocol::subscription_params()),
        )?;

        let mut buffered = Vec::new();
        loop {
            let value = read_message(&mut reader)?.ok_or(SocketError::Closed)?;
            if value.get("id").and_then(Value::as_str) == Some(id) {
                protocol::parse_result(value).map_err(SocketError::Api)?;
                break;
            }
            buffered.push(value);
        }

        let (sender, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            for value in buffered {
                if sender.send(StreamItem::Value(value)).is_err() {
                    return;
                }
            }
            loop {
                match read_message(&mut reader) {
                    Ok(Some(value)) => {
                        if sender.send(StreamItem::Value(value)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(StreamItem::Closed);
                        return;
                    }
                    Err(SocketError::Json(_)) => {
                        if sender.send(StreamItem::Malformed).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = sender.send(StreamItem::Closed);
                        return;
                    }
                }
            }
        });
        Ok(Self {
            receiver,
            control,
            thread: Some(thread),
        })
    }

    fn poll(&self, timeout: Duration) -> EventPoll {
        match self.receiver.recv_timeout(timeout) {
            Ok(StreamItem::Value(value)) => protocol::parse_event(&value)
                .map(EventPoll::Event)
                .unwrap_or(EventPoll::Malformed),
            Ok(StreamItem::Malformed) => EventPoll::Malformed,
            Ok(StreamItem::Closed) | Err(RecvTimeoutError::Disconnected) => EventPoll::Closed,
            Err(RecvTimeoutError::Timeout) => EventPoll::Timeout,
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        let _ = self.control.shutdown(Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn write_message(stream: &mut UnixStream, value: &Value) -> Result<(), SocketError> {
    serde_json::to_writer(&mut *stream, value)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn read_message(reader: &mut BufReader<UnixStream>) -> Result<Option<Value>, SocketError> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}
