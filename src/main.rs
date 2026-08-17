use herdr_agent_context::config::{Config, ConfigReload, ConfigWatcher};
use herdr_agent_context::herdr::socket::{EventPoll, SocketTransport};
use herdr_agent_context::runtime::Runtime;
use std::collections::HashMap;
use std::env;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-agent-context: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args();
    let _binary = args.next();
    if args.next().as_deref() != Some("listen") || args.next().is_some() {
        return Err("usage: herdr-agent-context listen".into());
    }
    if env::var("HERDR_ENV").as_deref() != Ok("1") {
        return Err("listen must run inside a Herdr plugin environment".into());
    }
    let socket_path = env::var_os("HERDR_SOCKET_PATH")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "HERDR_SOCKET_PATH is not set".to_owned())?;
    let Some(_lock) = ListenerLock::acquire(&socket_path).map_err(|_| "listener lock failed")?
    else {
        return Ok(());
    };

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())?;
    let environment: HashMap<String, String> = env::vars()
        .filter(|(key, _)| key == "PI_CODING_AGENT_SESSION_DIR" || key == "PI_CODING_AGENT_DIR")
        .collect();
    let mut watcher = env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .map(|directory| ConfigWatcher::new(&directory, &home));
    let initial_config = match watcher.as_mut().map(ConfigWatcher::poll) {
        Some(ConfigReload::Updated(config)) => config,
        Some(ConfigReload::Invalid) => {
            eprintln!("herdr-agent-context: invalid plugin config; using defaults");
            Config::default()
        }
        _ => Config::default(),
    };
    let mut runtime = Runtime::new(initial_config, home, environment);
    listen_forever(&socket_path, watcher.as_mut(), &mut runtime)
}

fn listen_forever(
    socket_path: &Path,
    mut watcher: Option<&mut ConfigWatcher>,
    runtime: &mut Runtime,
) -> Result<(), String> {
    let mut backoff_ms = 250;
    loop {
        let mut transport = match SocketTransport::connect(socket_path) {
            Ok(transport) => transport,
            Err(_) => {
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(5_000);
                continue;
            }
        };
        backoff_ms = 250;
        if let Err(error) = runtime.reconcile(&mut transport) {
            eprintln!("herdr-agent-context: initial reconciliation failed: {error}");
            continue;
        }

        loop {
            if let Some(watcher) = watcher.as_deref_mut() {
                match watcher.poll() {
                    ConfigReload::Updated(config) => runtime.set_config(config),
                    ConfigReload::Invalid => {
                        eprintln!(
                            "herdr-agent-context: invalid plugin config; keeping previous values"
                        );
                    }
                    ConfigReload::Unchanged => {}
                }
            }
            let timeout = Duration::from_millis(runtime.config().poll_interval_ms);
            match transport.poll_event(timeout) {
                EventPoll::Event(event) if event.kind == "pane_updated" => continue,
                EventPoll::Event(_) => {}
                EventPoll::Malformed => {
                    eprintln!("herdr-agent-context: skipped malformed Herdr event");
                    continue;
                }
                EventPoll::Closed => break,
                EventPoll::Timeout => {}
            }
            if let Err(error) = runtime.reconcile(&mut transport) {
                eprintln!("herdr-agent-context: reconciliation failed: {error}");
                break;
            }
        }
    }
}

struct ListenerLock {
    _file: File,
}

impl ListenerLock {
    fn acquire(socket_path: &Path) -> io::Result<Option<Self>> {
        let lock_path = socket_path.with_extension("agent-context.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }
}
