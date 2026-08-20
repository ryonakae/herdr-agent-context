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
    let state_dir = env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let mut runtime = Runtime::new(initial_config, home, environment);
    if let Some(state_dir) = state_dir {
        if runtime
            .initialize_tab_names(&state_dir, &socket_path)
            .is_err()
        {
            eprintln!("herdr-agent-context: tab-name synchronization disabled: state unavailable");
        }
    } else if runtime.config().tab_name.enabled {
        eprintln!("herdr-agent-context: tab-name synchronization disabled: state unavailable");
    }
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
        runtime.reset_tab_event_expectations();
        match runtime.reconcile(&mut transport) {
            Ok(status) => report_tab_name_status(status),
            Err(error) => {
                eprintln!("herdr-agent-context: initial reconciliation failed: {error}");
                sleep_with_backoff(&mut backoff_ms);
                continue;
            }
        }

        let mut schedule = PollSchedule::new(
            Instant::now(),
            Duration::from_millis(runtime.config().poll_interval_ms),
        );
        loop {
            let mut immediate_reconcile = false;
            if let Some(watcher) = watcher.as_deref_mut() {
                match apply_config_reload(runtime, watcher.poll()) {
                    AppliedConfigReload::Updated => {
                        schedule.shorten(
                            Instant::now(),
                            Duration::from_millis(runtime.config().poll_interval_ms),
                        );
                        immediate_reconcile = true;
                        if runtime.config().tab_name.enabled && !runtime.tab_names_available() {
                            eprintln!(
                                "herdr-agent-context: tab-name synchronization disabled: state unavailable"
                            );
                        }
                    }
                    AppliedConfigReload::Invalid => {
                        eprintln!(
                            "herdr-agent-context: invalid plugin config; keeping previous values"
                        );
                    }
                    AppliedConfigReload::Unchanged => {}
                }
            }

            let wait_started = Instant::now();
            let event = transport.poll_event(next_wait(
                &schedule,
                runtime.next_tab_deadline(),
                wait_started,
                immediate_reconcile,
            ));
            let event_reconcile = match event {
                EventPoll::Event(event)
                    if event.kind == "pane_focused" || event.kind == "pane.focused" =>
                {
                    runtime.note_focus(
                        event.pane_id.as_deref(),
                        event.workspace_id.as_deref(),
                        Instant::now(),
                    );
                    false
                }
                EventPoll::Event(event)
                    if event.kind == "tab_renamed" || event.kind == "tab.renamed" =>
                {
                    runtime.note_tab_rename(event.tab_id.as_deref(), event.label.as_deref());
                    runtime.tab_event_reconcile_needed()
                }
                EventPoll::Event(event) => {
                    event_requires_reconcile(&event.kind, runtime.tab_event_reconcile_needed())
                }
                EventPoll::Malformed => {
                    eprintln!("herdr-agent-context: skipped malformed Herdr event");
                    false
                }
                EventPoll::Closed => break,
                EventPoll::Timeout => false,
            };
            let now = Instant::now();
            let poll_due = schedule.is_due(now);
            let tab_due = runtime
                .next_tab_deadline()
                .is_some_and(|deadline| now >= deadline);
            if !immediate_reconcile && !event_reconcile && !poll_due && !tab_due {
                continue;
            }
            match runtime.reconcile_at(&mut transport, now) {
                Ok(status) => report_tab_name_status(status),
                Err(error) => {
                    eprintln!("herdr-agent-context: reconciliation failed: {error}");
                    break;
                }
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

fn event_requires_reconcile(kind: &str, tab_ownership_active: bool) -> bool {
    match kind {
        "pane_updated" | "pane.updated" | "pane_focused" | "pane.focused" => false,
        "tab_created" | "tab.created" | "tab_closed" | "tab.closed" | "tab_renamed"
        | "tab.renamed" | "tab_moved" | "tab.moved" | "layout_updated" | "layout.updated"
        | "pane_moved" | "pane.moved" => tab_ownership_active,
        _ => true,
    }
}

fn report_tab_name_status(status: herdr_agent_context::runtime::ReconcileStatus) {
    if status.tab_name_disabled {
        eprintln!("herdr-agent-context: tab-name synchronization disabled: state unavailable");
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

fn next_wait(
    schedule: &PollSchedule,
    tab_deadline: Option<Instant>,
    now: Instant,
    immediate: bool,
) -> Duration {
    if immediate {
        return Duration::ZERO;
    }
    tab_deadline
        .map(|deadline| deadline.saturating_duration_since(now))
        .map_or_else(
            || schedule.remaining(now),
            |tab| tab.min(schedule.remaining(now)),
        )
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
    fn poll_deadline_is_not_extended_by_events_or_tab_debounce() {
        let start = Instant::now();
        let mut schedule = PollSchedule::new(start, Duration::from_secs(2));
        let original = schedule.deadline;
        assert_eq!(
            next_wait(
                &schedule,
                Some(start + Duration::from_millis(150)),
                start,
                false,
            ),
            Duration::from_millis(150)
        );
        assert_eq!(next_wait(&schedule, None, start, true), Duration::ZERO);
        assert!(!schedule.is_due(start + Duration::from_millis(150)));
        assert_eq!(schedule.deadline, original);
        assert!(schedule.is_due(start + Duration::from_secs(2)));
        schedule.reset(start + Duration::from_secs(2), Duration::from_secs(2));
        assert_eq!(schedule.deadline, start + Duration::from_secs(4));
    }

    #[test]
    fn tab_only_events_are_inert_without_active_tab_ownership() {
        for kind in [
            "tab_renamed",
            "tab.moved",
            "tab_closed",
            "layout.updated",
            "pane_moved",
        ] {
            assert!(!event_requires_reconcile(kind, false));
            assert!(event_requires_reconcile(kind, true));
        }
        assert!(event_requires_reconcile("pane_created", false));
        assert!(!event_requires_reconcile("pane_updated", true));
        assert!(!event_requires_reconcile("pane.focused", true));
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
