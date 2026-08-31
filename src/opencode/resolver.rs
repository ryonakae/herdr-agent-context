use super::session::{OpenCodeSessionError, OpenCodeSessionView, read_session, scan_sessions};
use crate::backend::ProcessCommand;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenCodeCliState {
    Ineligible,
    Invalid,
    Eligible {
        pid: u32,
        exact_session: Option<String>,
        fork: bool,
    },
}

pub fn inspect_cli(processes: &[ProcessCommand]) -> OpenCodeCliState {
    let mut eligible = None;
    for command in processes
        .iter()
        .filter(|command| is_opencode_command(command))
    {
        let Some(tokens) = command.argv.as_deref().filter(|tokens| !tokens.is_empty()) else {
            return OpenCodeCliState::Ineligible;
        };
        let offset = usize::from(tokens.first().is_some_and(|token| is_opencode_name(token)));
        let state = inspect_args(command.pid, &tokens[offset..]);
        match state {
            OpenCodeCliState::Ineligible | OpenCodeCliState::Invalid => return state,
            OpenCodeCliState::Eligible { .. } if eligible.is_some() => {
                return OpenCodeCliState::Invalid;
            }
            OpenCodeCliState::Eligible { .. } => eligible = Some(state),
        }
    }
    eligible.unwrap_or(OpenCodeCliState::Ineligible)
}

fn inspect_args(pid: u32, args: &[String]) -> OpenCodeCliState {
    let mut positionals = Vec::new();
    let mut exact_session = None;
    let mut continue_count = 0;
    let mut fork = false;
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            positionals.extend(args[index + 1..].iter().map(String::as_str));
            break;
        }
        if matches!(token.as_str(), "-h" | "--help" | "-v" | "-V" | "--version") {
            return OpenCodeCliState::Ineligible;
        }
        if matches!(token.as_str(), "-c" | "--continue") {
            continue_count += 1;
            if continue_count > 1 {
                return OpenCodeCliState::Invalid;
            }
            index += 1;
            continue;
        }
        if matches!(token.as_str(), "-f" | "--fork") {
            if fork {
                return OpenCodeCliState::Invalid;
            }
            fork = true;
            index += 1;
            continue;
        }
        if matches!(token.as_str(), "-s" | "--session") {
            let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-')) else {
                return OpenCodeCliState::Invalid;
            };
            if exact_session.is_some() || !valid_session_identity(value) {
                return OpenCodeCliState::Invalid;
            }
            exact_session = Some(value.clone());
            index += 2;
            continue;
        }
        if let Some(value) = token.strip_prefix("--session=") {
            if exact_session.is_some() || !valid_session_identity(value) {
                return OpenCodeCliState::Invalid;
            }
            exact_session = Some(value.to_owned());
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            if option_takes_value(token) {
                if !token.contains('=') {
                    let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-'))
                    else {
                        return OpenCodeCliState::Invalid;
                    };
                    let _ = value;
                    index += 2;
                } else if token.ends_with('=') {
                    return OpenCodeCliState::Invalid;
                } else {
                    index += 1;
                }
            } else if known_boolean_option(token) {
                index += 1;
            } else {
                return OpenCodeCliState::Invalid;
            }
            continue;
        }
        positionals.push(token.as_str());
        index += 1;
    }

    if positionals
        .first()
        .is_some_and(|value| excluded_subcommand(value))
    {
        return OpenCodeCliState::Ineligible;
    }
    if positionals.len() > 1 || (continue_count > 0 && exact_session.is_some()) {
        return OpenCodeCliState::Invalid;
    }
    if fork {
        exact_session = None;
    }
    OpenCodeCliState::Eligible {
        pid,
        exact_session,
        fork,
    }
}

fn option_takes_value(token: &str) -> bool {
    matches!(
        token.split('=').next().unwrap_or(token),
        "--model"
            | "-m"
            | "--agent"
            | "--port"
            | "--hostname"
            | "--log-level"
            | "--mdns-domain"
            | "--cors"
            | "--prompt"
            | "--replay-limit"
    )
}

fn known_boolean_option(token: &str) -> bool {
    matches!(
        token,
        "--print-logs" | "--pure" | "--mdns" | "--auto" | "--mini" | "--no-replay"
    )
}

fn excluded_subcommand(value: &str) -> bool {
    matches!(
        value,
        "run"
            | "attach"
            | "serve"
            | "web"
            | "acp"
            | "mcp"
            | "github"
            | "auth"
            | "agent"
            | "upgrade"
            | "uninstall"
            | "generate"
            | "stats"
            | "export"
            | "import"
            | "db"
            | "debug"
            | "completion"
            | "help"
    )
}

fn is_opencode_command(command: &ProcessCommand) -> bool {
    is_opencode_name(&command.name)
        || command.argv0.as_deref().is_some_and(is_opencode_name)
        || command
            .argv
            .as_ref()
            .and_then(|argv| argv.first())
            .is_some_and(|value| is_opencode_name(value))
}

fn is_opencode_name(value: &str) -> bool {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("opencode"))
}

pub fn valid_session_identity(value: &str) -> bool {
    value.strip_prefix("ses_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeCandidate {
    pub database_path: PathBuf,
    pub session: OpenCodeSessionView,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenCodeResolveError {
    InvalidTarget,
    AmbiguousIdentity,
    Read,
}

#[derive(Default)]
pub struct OpenCodeScanner;

impl OpenCodeScanner {
    pub fn resolve_exact(
        &mut self,
        database_paths: &[PathBuf],
        cwd: &Path,
        identity: &str,
    ) -> Result<Option<OpenCodeCandidate>, OpenCodeResolveError> {
        if !valid_session_identity(identity) {
            return Err(OpenCodeResolveError::InvalidTarget);
        }
        let mut matches = Vec::new();
        for database_path in database_paths {
            match read_session(database_path, identity, cwd) {
                Ok(Some(session)) => matches.push(OpenCodeCandidate {
                    database_path: canonical_or_normalized(database_path),
                    session,
                }),
                Ok(None) => {}
                Err(OpenCodeSessionError::InvalidSession) => {
                    return Err(OpenCodeResolveError::InvalidTarget);
                }
                Err(_) => return Err(OpenCodeResolveError::Read),
            }
        }
        if matches.len() > 1 {
            return Err(OpenCodeResolveError::AmbiguousIdentity);
        }
        Ok(matches.pop())
    }

    pub fn scan_database(
        &mut self,
        database_path: &Path,
        cwd: &Path,
        now_millis: i64,
    ) -> Result<Vec<OpenCodeCandidate>, OpenCodeResolveError> {
        scan_sessions(database_path, cwd, now_millis)
            .map(|sessions| {
                let database_path = canonical_or_normalized(database_path);
                sessions
                    .into_iter()
                    .map(|session| OpenCodeCandidate {
                        database_path: database_path.clone(),
                        session,
                    })
                    .collect()
            })
            .map_err(map_session_error)
    }
}

fn map_session_error(_: OpenCodeSessionError) -> OpenCodeResolveError {
    OpenCodeResolveError::Read
}

pub fn canonical_or_normalized(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};

    fn command(pid: u32, args: Option<&[&str]>) -> ProcessCommand {
        ProcessCommand {
            pid,
            name: "opencode".into(),
            argv: args.map(|args| args.iter().map(|value| (*value).to_owned()).collect()),
            argv0: Some("opencode".into()),
            cmdline: None,
        }
    }

    fn create_database(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    directory TEXT NOT NULL,
                    title TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    time_archived INTEGER
                );
                CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );
                CREATE TABLE part (
                    id TEXT PRIMARY KEY,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );
                ",
            )
            .unwrap();
        connection
    }

    fn insert_session(connection: &Connection, id: &str, cwd: &Path, updated: i64) {
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, 'Synthetic', 1, ?3, NULL)",
                params![id, cwd.to_str().unwrap(), updated],
            )
            .unwrap();
    }

    #[test]
    fn classifies_supported_root_tui_forms_and_returns_the_root_pid() {
        for args in [
            &["opencode"][..],
            &["opencode", "/synthetic/project"][..],
            &["opencode", "--continue"][..],
            &["opencode", "--fork"][..],
            &["opencode", "--continue", "--fork"][..],
            &["opencode", "--session", "ses_exact"][..],
            &["opencode", "--session=ses_exact"][..],
            &[
                "opencode",
                "--pure",
                "--print-logs",
                "--log-level",
                "WARN",
                "--mdns",
                "--mdns-domain",
                "synthetic.local",
                "--cors",
                "https://synthetic.invalid",
                "--model",
                "synthetic/model",
                "--prompt",
                "Synthetic prompt",
                "--agent",
                "build",
                "--auto",
                "--mini",
                "--no-replay",
                "--replay-limit",
                "20",
                "/synthetic/project",
            ][..],
            &["opencode", "--session", "ses_parent", "--fork"][..],
        ] {
            let OpenCodeCliState::Eligible { pid, .. } = inspect_cli(&[command(42, Some(args))])
            else {
                panic!("expected eligible: {args:?}");
            };
            assert_eq!(pid, 42);
        }
        assert_eq!(
            inspect_cli(&[command(42, Some(&["opencode", "--session", "ses_exact"]))]),
            OpenCodeCliState::Eligible {
                pid: 42,
                exact_session: Some("ses_exact".into()),
                fork: false,
            }
        );
        assert_eq!(
            inspect_cli(&[command(
                42,
                Some(&["opencode", "--session", "ses_parent", "--fork"])
            )]),
            OpenCodeCliState::Eligible {
                pid: 42,
                exact_session: None,
                fork: true,
            }
        );
    }

    #[test]
    fn excludes_nonroot_help_version_and_malformed_visible_commands() {
        for subcommand in [
            "run",
            "attach",
            "serve",
            "web",
            "acp",
            "mcp",
            "github",
            "auth",
            "agent",
            "upgrade",
            "uninstall",
            "generate",
            "stats",
            "export",
            "import",
            "db",
            "debug",
            "completion",
            "help",
        ] {
            assert_eq!(
                inspect_cli(&[command(1, Some(&["opencode", subcommand]))]),
                OpenCodeCliState::Ineligible
            );
        }
        for args in [&["opencode", "--help"][..], &["opencode", "--version"][..]] {
            assert_eq!(
                inspect_cli(&[command(1, Some(args))]),
                OpenCodeCliState::Ineligible
            );
        }
        for args in [
            &["opencode", "--session"][..],
            &["opencode", "--session", "bad id"][..],
            &["opencode", "--session", "ses_one", "--session", "ses_two"][..],
            &["opencode", "--continue", "--session", "ses_one"][..],
            &["opencode", "one", "two"][..],
            &["opencode", "--unknown"][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(1, Some(args))]),
                OpenCodeCliState::Invalid,
                "{args:?}"
            );
        }
        assert_eq!(
            inspect_cli(&[command(1, None)]),
            OpenCodeCliState::Ineligible
        );
        assert_eq!(
            inspect_cli(&[
                command(1, Some(&["opencode"])),
                command(2, Some(&["opencode"])),
            ]),
            OpenCodeCliState::Invalid
        );
    }

    #[test]
    fn exact_identity_is_unique_across_every_database_and_bypasses_age() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        let first_connection = create_database(&first);
        let second_connection = create_database(&second);
        insert_session(&first_connection, "ses_exact", &cwd, 1);

        let candidate = OpenCodeScanner
            .resolve_exact(&[first.clone(), second.clone()], &cwd, "ses_exact")
            .unwrap()
            .unwrap();
        assert_eq!(candidate.session.display.session_identity, "ses_exact");
        assert_eq!(candidate.database_path, first.canonicalize().unwrap());

        insert_session(&second_connection, "ses_exact", &cwd, 1);
        assert_eq!(
            OpenCodeScanner.resolve_exact(&[first, second], &cwd, "ses_exact"),
            Err(OpenCodeResolveError::AmbiguousIdentity)
        );
    }

    #[test]
    fn exact_and_ordinary_reads_fail_closed_when_any_database_is_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let healthy = temp.path().join("healthy.db");
        let missing = temp.path().join("missing.db");
        let connection = create_database(&healthy);
        insert_session(&connection, "ses_exact", &cwd, 1);

        assert_eq!(
            OpenCodeScanner.resolve_exact(&[healthy.clone(), missing], &cwd, "ses_exact"),
            Err(OpenCodeResolveError::Read)
        );
        assert!(OpenCodeScanner.scan_database(&healthy, &cwd, 1).is_ok());
    }

    #[test]
    fn ordinary_scan_is_recent_root_cwd_bounded_and_uses_per_session_fingerprints() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let other = temp.path().join("other");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&other).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let now = 100 * 24 * 60 * 60 * 1_000_i64;
        insert_session(&connection, "ses_active", &cwd, now);
        insert_session(
            &connection,
            "ses_old",
            &cwd,
            now - 31 * 24 * 60 * 60 * 1_000,
        );
        insert_session(&connection, "ses_other", &other, now);
        connection
            .execute(
                "INSERT INTO session VALUES ('ses_child', 'ses_parent', ?1, 'Child', 1, ?2, NULL)",
                params![cwd.to_str().unwrap(), now],
            )
            .unwrap();

        let initial = OpenCodeScanner.scan_database(&database, &cwd, now).unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].session.display.session_identity, "ses_active");
        let fingerprint = initial[0].session.fingerprint.clone();

        connection
            .execute(
                "UPDATE session SET time_updated = ?1 WHERE id = 'ses_other'",
                [now + 1],
            )
            .unwrap();
        let unchanged = OpenCodeScanner
            .scan_database(&database, &cwd, now + 1)
            .unwrap();
        assert_eq!(unchanged[0].session.fingerprint, fingerprint);

        for index in 0..30 {
            insert_session(
                &connection,
                &format!("ses_many_{index:02}"),
                &cwd,
                now + index,
            );
        }
        let bounded = OpenCodeScanner
            .scan_database(&database, &cwd, now + 30)
            .unwrap();
        assert_eq!(bounded.len(), 25);
    }
}
