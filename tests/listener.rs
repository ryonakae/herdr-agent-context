use herdr_agent_context::config::Config;
use herdr_agent_context::herdr::HerdrApi;
use herdr_agent_context::herdr::protocol::{
    AgentInfo, AgentSessionInfo, LAST_MESSAGE_TOKEN, ProcessInfo, SESSION_NAME_TOKEN,
};
use herdr_agent_context::herdr::socket::{EventPoll, SocketTransport};
use herdr_agent_context::runtime::Runtime;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
struct Report {
    pane_id: String,
    applies_to_source: Option<String>,
    seq: u64,
    ttl_ms: u64,
    session_name: Option<String>,
    last_message: Option<String>,
}

struct FakeApi {
    agents: Vec<AgentInfo>,
    process_args: HashMap<String, Vec<String>>,
    reports: Vec<Report>,
}

impl HerdrApi for FakeApi {
    type Error = ();

    fn list_agents(&mut self) -> Result<Vec<AgentInfo>, Self::Error> {
        Ok(self.agents.clone())
    }

    fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, Self::Error> {
        Ok(ProcessInfo {
            pane_id: pane_id.to_owned(),
            foreground_processes: vec![herdr_agent_context::herdr::protocol::Process {
                pid: 1,
                name: "pi".into(),
                argv: Some(self.process_args.get(pane_id).cloned().unwrap_or_default()),
            }],
        })
    }

    fn report_metadata(
        &mut self,
        pane_id: &str,
        applies_to_source: Option<&str>,
        seq: u64,
        ttl_ms: u64,
        session_name: Option<&str>,
        last_message: Option<&str>,
    ) -> Result<(), Self::Error> {
        self.reports.push(Report {
            pane_id: pane_id.into(),
            applies_to_source: applies_to_source.map(ToOwned::to_owned),
            seq,
            ttl_ms,
            session_name: session_name.map(ToOwned::to_owned),
            last_message: last_message.map(ToOwned::to_owned),
        });
        Ok(())
    }
}

fn agent() -> AgentInfo {
    AgentInfo {
        terminal_id: "term-1".into(),
        agent: Some("pi".into()),
        agent_status: "working".into(),
        cwd: Some("/work/project".into()),
        foreground_cwd: Some("/work/project".into()),
        pane_id: "w1:p1".into(),
        revision: 1,
        state_change_seq: 1,
        agent_session: None,
    }
}

fn fake_api() -> FakeApi {
    FakeApi {
        agents: vec![agent()],
        process_args: HashMap::from([("w1:p1".into(), vec!["pi".into()])]),
        reports: Vec::new(),
    }
}

fn session_text(last_entry: &str) -> String {
    format!(
        concat!(
            "{{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work/project\"}}\n",
            "{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"Build context\"}}}}\n",
            "{{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{{\"role\":\"assistant\",\"content\":\"Initial answer\"}}}}\n",
            "{last_entry}"
        ),
        last_entry = last_entry
    )
}

fn runtime_for(root: &Path) -> Runtime {
    Runtime::new(
        Config {
            pi_session_dirs: vec![root.to_owned()],
            ..Config::default()
        },
        PathBuf::from("/no-home"),
        HashMap::new(),
    )
}

#[test]
fn runtime_refreshes_ttl_and_retains_activity_until_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session.jsonl");
    fs::write(
        &session,
        session_text(
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"a1\",\"name\":\"Named session\"}\n",
        ),
    )
    .unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();

    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);
    assert_eq!(
        api.reports[0].session_name.as_deref(),
        Some("Named session")
    );
    assert_eq!(
        api.reports[0].last_message.as_deref(),
        Some("Initial answer")
    );
    assert_eq!(api.reports[0].ttl_ms, 10_000);

    fs::write(
        &session,
        session_text(
            concat!(
                "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"Next question\"}}\n",
                "{\"type\":\"session_info\",\"id\":\"n2\",\"parentId\":\"u2\",\"name\":\"Renamed\"}\n"
            ),
        ),
    )
    .unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(api.reports[1].session_name.as_deref(), Some("Renamed"));
    assert_eq!(
        api.reports[1].last_message.as_deref(),
        Some("Initial answer")
    );
    assert!(api.reports[1].seq > api.reports[0].seq);

    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 3);
    assert_eq!(
        api.reports[2].last_message.as_deref(),
        Some("Initial answer")
    );
}

#[test]
fn runtime_does_not_refresh_ttl_after_parse_failure_and_recovers() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("session.jsonl");
    fs::write(&session, session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);

    fs::write(&session, format!("{}{{", session_text(""))).unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);

    fs::write(
        &session,
        session_text(
            "{\"type\":\"message\",\"id\":\"a2\",\"parentId\":\"a1\",\"message\":{\"role\":\"assistant\",\"content\":\"Recovered answer\"}}\n",
        ),
    )
    .unwrap();
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 2);
    assert_eq!(
        api.reports[1].last_message.as_deref(),
        Some("Recovered answer")
    );
}

#[test]
fn runtime_clears_metadata_for_no_session_and_non_pi_panes() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    runtime.reconcile(&mut api).unwrap();

    api.process_args.insert(
        "w1:p1".into(),
        vec![
            "safehouse".into(),
            "--".into(),
            "pi".into(),
            "--no-session".into(),
        ],
    );
    runtime.reconcile(&mut api).unwrap();
    let clear = api.reports.last().unwrap();
    assert_eq!(clear.pane_id, "w1:p1");
    assert_eq!(clear.session_name, None);
    assert_eq!(clear.last_message, None);

    api.process_args.insert("w1:p1".into(), vec!["pi".into()]);
    runtime.reconcile(&mut api).unwrap();
    assert!(api.reports.last().unwrap().session_name.is_some());
    api.agents[0].agent = Some("codex".into());
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.last().unwrap().session_name, None);
}

#[test]
fn authoritative_unreadable_path_never_uses_valid_fallback() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("fallback.jsonl"), session_text("")).unwrap();
    let mut runtime = runtime_for(temp.path());
    let mut api = fake_api();
    api.agents[0].agent_session = Some(AgentSessionInfo {
        source: "native-pi".into(),
        agent: "pi".into(),
        kind: "path".into(),
        value: temp.path().join("missing.jsonl").display().to_string(),
    });
    runtime.reconcile(&mut api).unwrap();
    assert!(api.reports.is_empty());

    api.agents[0].agent_session = None;
    runtime.reconcile(&mut api).unwrap();
    assert_eq!(api.reports.len(), 1);
    assert_eq!(api.reports[0].applies_to_source, None);
}

#[test]
fn listener_restart_uses_a_fresh_monotonic_sequence_epoch() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), session_text("")).unwrap();
    let mut api = fake_api();
    runtime_for(temp.path()).reconcile(&mut api).unwrap();
    let first = api.reports.last().unwrap().seq;
    runtime_for(temp.path()).reconcile(&mut api).unwrap();
    let restarted = api.reports.last().unwrap().seq;
    assert!(restarted > first);
}

#[test]
fn socket_transport_subscribes_first_buffers_events_and_uses_exact_metadata_contract() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_seen = Arc::clone(&seen);

    let server = thread::spawn(move || {
        let (event_stream, _) = listener.accept().unwrap();
        let mut event_reader = BufReader::new(event_stream.try_clone().unwrap());
        let mut line = String::new();
        event_reader.read_line(&mut line).unwrap();
        let subscribe: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(subscribe["method"], "events.subscribe");
        assert_eq!(
            subscribe["params"]["subscriptions"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        server_seen.lock().unwrap().push(subscribe.clone());
        let mut event_writer = event_stream;
        writeln!(
            event_writer,
            "{}",
            json!({
                "event": "pane_created",
                "data": {"type": "pane_created", "pane": {"pane_id": "w1:p2"}}
            })
        )
        .unwrap();
        writeln!(
            event_writer,
            "{}",
            json!({"id": subscribe["id"], "result": {"type": "events_subscribed"}})
        )
        .unwrap();
        writeln!(event_writer, "not-json").unwrap();
        event_writer.flush().unwrap();

        for _ in 0..3 {
            let (rpc_stream, _) = listener.accept().unwrap();
            let mut rpc_reader = BufReader::new(rpc_stream.try_clone().unwrap());
            let mut rpc_writer = rpc_stream;
            line.clear();
            rpc_reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            server_seen.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "agent.list" => json!({
                    "type": "agent_list",
                    "agents": [{
                        "terminal_id": "term-1", "agent": "pi", "agent_status": "working",
                        "cwd": "/work", "foreground_cwd": "/work", "pane_id": "w1:p1",
                        "revision": 1, "state_change_seq": 2, "future": true
                    }]
                }),
                "pane.process_info" => json!({
                    "type": "pane_process_info",
                    "process_info": {"pane_id": "w1:p1", "foreground_processes": [
                        {"pid": 1, "name": "bash", "argv": ["pi", "--no-session"]}
                    ]}
                }),
                "pane.report_metadata" => json!({"type": "pane_metadata_reported"}),
                method => panic!("unexpected method {method}"),
            };
            writeln!(
                rpc_writer,
                "{}",
                json!({"id": request["id"], "result": result})
            )
            .unwrap();
            rpc_writer.flush().unwrap();
        }
    });

    let mut transport = SocketTransport::connect(&socket).unwrap();
    match transport.poll_event(Duration::from_secs(1)) {
        EventPoll::Event(event) => assert_eq!(event.pane_id.as_deref(), Some("w1:p2")),
        _ => panic!("expected buffered event"),
    }
    assert!(matches!(
        transport.poll_event(Duration::from_secs(1)),
        EventPoll::Malformed
    ));
    let agents = transport.list_agents().unwrap();
    assert_eq!(agents[0].agent.as_deref(), Some("pi"));
    assert_eq!(
        transport.process_info("w1:p1").unwrap().args(),
        vec!["pi", "--no-session"]
    );
    transport
        .report_metadata("w1:p1", Some("native"), 9, 10_000, Some("name"), None)
        .unwrap();
    drop(transport);
    server.join().unwrap();

    let requests = seen.lock().unwrap();
    let report = requests
        .iter()
        .find(|request| request["method"] == "pane.report_metadata")
        .unwrap();
    let params = &report["params"];
    assert_eq!(params["source"], "ryonakae.agent-context");
    assert_eq!(params["agent"], "pi");
    assert_eq!(params["applies_to_source"], "native");
    assert_eq!(params["tokens"][SESSION_NAME_TOKEN], "name");
    assert!(params["tokens"][LAST_MESSAGE_TOKEN].is_null());
    assert_eq!(params["seq"], 9);
    assert_eq!(params["ttl_ms"], 10_000);
    assert!(params.get("title").is_none());
}
