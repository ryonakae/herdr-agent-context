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
use std::time::{Duration, Instant};

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
        .filter(|(key, _)| {
            key == "PI_CODING_AGENT_SESSION_DIR"
                || key == "PI_CODING_AGENT_DIR"
                || key == "CLAUDE_CONFIG_DIR"
        })
        .collect();
    let mut watcher = env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .map(|directory| ConfigWatcher::new(&directory, &home));
    let (initial_config, initial_invalid) =
        initial_config(watcher.as_mut().map(ConfigWatcher::poll));
    if initial_invalid {
        eprintln!("herdr-agent-context: invalid plugin config; using defaults");
    }
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
                sleep_with_backoff(&mut backoff_ms);
                continue;
            }
        };
        if let Err(error) = runtime.reconcile(&mut transport) {
            eprintln!("herdr-agent-context: initial reconciliation failed: {error}");
            sleep_with_backoff(&mut backoff_ms);
            continue;
        }

        let mut schedule = PollSchedule::new(
            Instant::now(),
            Duration::from_millis(runtime.config().poll_interval_ms),
        );
        loop {
            if let Some(watcher) = watcher.as_deref_mut() {
                match apply_config_reload(runtime, watcher.poll()) {
                    AppliedConfigReload::Updated => {
                        schedule.shorten(
                            Instant::now(),
                            Duration::from_millis(runtime.config().poll_interval_ms),
                        );
                    }
                    AppliedConfigReload::Invalid => {
                        eprintln!(
                            "herdr-agent-context: invalid plugin config; keeping previous values"
                        );
                    }
                    AppliedConfigReload::Unchanged => {}
                }
            }

            let event = transport.poll_event(schedule.remaining(Instant::now()));
            let event_reconcile = match event {
                EventPoll::Event(event) => event.kind != "pane_updated",
                EventPoll::Malformed => {
                    eprintln!("herdr-agent-context: skipped malformed Herdr event");
                    false
                }
                EventPoll::Closed => break,
                EventPoll::Timeout => false,
            };
            let now = Instant::now();
            let poll_due = schedule.is_due(now);
            if !event_reconcile && !poll_due {
                continue;
            }
            if let Err(error) = runtime.reconcile(&mut transport) {
                eprintln!("herdr-agent-context: reconciliation failed: {error}");
                break;
            }
            if poll_due {
                schedule.reset(
                    now,
                    Duration::from_millis(runtime.config().poll_interval_ms),
                );
                backoff_ms = 250;
            }
        }
        sleep_with_backoff(&mut backoff_ms);
    }
}

fn initial_config(reload: Option<ConfigReload>) -> (Config, bool) {
    match reload {
        Some(ConfigReload::Updated(config)) => (config, false),
        Some(ConfigReload::Invalid) => (Config::default(), true),
        Some(ConfigReload::Unchanged) | None => (Config::default(), false),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppliedConfigReload {
    Unchanged,
    Updated,
    Invalid,
}

fn apply_config_reload(runtime: &mut Runtime, reload: ConfigReload) -> AppliedConfigReload {
    match reload {
        ConfigReload::Updated(config) => {
            runtime.set_config(config);
            AppliedConfigReload::Updated
        }
        ConfigReload::Invalid => AppliedConfigReload::Invalid,
        ConfigReload::Unchanged => AppliedConfigReload::Unchanged,
    }
}

fn sleep_with_backoff(backoff_ms: &mut u64) {
    thread::sleep(Duration::from_millis(*backoff_ms));
    *backoff_ms = (*backoff_ms * 2).min(5_000);
}

struct PollSchedule {
    deadline: Instant,
}

impl PollSchedule {
    fn new(now: Instant, interval: Duration) -> Self {
        Self {
            deadline: now + interval,
        }
    }

    fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    fn reset(&mut self, now: Instant, interval: Duration) {
        self.deadline = now + interval;
    }

    fn shorten(&mut self, now: Instant, interval: Duration) {
        self.deadline = self.deadline.min(now + interval);
    }
}

fn listener_lock_path(socket_path: &Path) -> PathBuf {
    let mut path = socket_path.as_os_str().to_os_string();
    path.push(".agent-context.lock");
    PathBuf::from(path)
}

struct ListenerLock {
    _file: File,
}

impl ListenerLock {
    fn acquire(socket_path: &Path) -> io::Result<Option<Self>> {
        let lock_path = listener_lock_path(socket_path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_initial_config_uses_complete_defaults() {
        let (config, invalid) = initial_config(Some(ConfigReload::Invalid));
        assert_eq!(config, Config::default());
        assert!(invalid);
    }

    #[test]
    fn invalid_reload_retains_the_complete_runtime_config() {
        let config = Config::from_toml(
            concat!(
                "poll_interval_ms = 3000\n",
                "metadata_ttl_ms = 12000\n",
                "[agents.pi]\nsession_dirs = [\"/pi\"]\n",
                "[agents.claude]\nsession_dirs = [\"/claude\"]\n",
                "[tab_name]\nenabled = true\n"
            ),
            Path::new("/home/me"),
        )
        .unwrap();
        let mut runtime = Runtime::new(config.clone(), PathBuf::from("/home/me"), HashMap::new());

        assert_eq!(
            apply_config_reload(&mut runtime, ConfigReload::Invalid),
            AppliedConfigReload::Invalid
        );
        assert_eq!(runtime.config(), &config);
        assert!(runtime.config().tab_name.enabled);
    }

    #[test]
    fn poll_deadline_is_not_extended_by_events() {
        let start = Instant::now();
        let mut schedule = PollSchedule::new(start, Duration::from_secs(2));
        let original = schedule.deadline;
        assert!(!schedule.is_due(start + Duration::from_secs(1)));
        assert_eq!(schedule.deadline, original);
        assert!(schedule.is_due(start + Duration::from_secs(2)));
        schedule.reset(start + Duration::from_secs(2), Duration::from_secs(2));
        assert_eq!(schedule.deadline, start + Duration::from_secs(4));
    }

    #[test]
    fn lock_path_preserves_the_complete_socket_name() {
        assert_ne!(
            listener_lock_path(Path::new("/tmp/herdr.sock")),
            listener_lock_path(Path::new("/tmp/herdr.api"))
        );
        assert_eq!(
            listener_lock_path(Path::new("/tmp/herdr.sock")),
            PathBuf::from("/tmp/herdr.sock.agent-context.lock")
        );
    }

    #[test]
    fn locks_are_scoped_to_the_full_socket_path() {
        let temp = tempfile::tempdir().unwrap();
        let first_path = temp.path().join("herdr.sock");
        let second_path = temp.path().join("herdr.api");
        let first = ListenerLock::acquire(&first_path).unwrap().unwrap();
        assert!(ListenerLock::acquire(&first_path).unwrap().is_none());
        assert!(ListenerLock::acquire(&second_path).unwrap().is_some());
        drop(first);
        assert!(ListenerLock::acquire(&first_path).unwrap().is_some());
    }
}
