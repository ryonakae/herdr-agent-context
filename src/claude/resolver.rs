use super::session::{ClaudeSessionHeader, parse_session_header};
use crate::backend::ProcessCommand;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudeCliState {
    Ineligible,
    Invalid,
    Eligible { exact_session: Option<String> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCandidate {
    pub path: PathBuf,
    pub session_identity: String,
    pub cwd: PathBuf,
    pub size: u64,
    pub modified_at: SystemTime,
}

const ORDINARY_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const ORDINARY_MAX_CANDIDATES: usize = 25;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fingerprint {
    size: u64,
    modified_at: SystemTime,
}

#[derive(Clone)]
struct CachedHeader {
    fingerprint: Fingerprint,
    header: Option<ClaudeSessionHeader>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeResolveError {
    InvalidTarget,
    AmbiguousIdentity,
    Retryable,
}

#[derive(Default)]
pub struct ClaudeScanner {
    cache: HashMap<PathBuf, CachedHeader>,
}

impl ClaudeScanner {
    pub fn validate_path(
        &mut self,
        roots: &[PathBuf],
        cwd: &Path,
        path: &Path,
    ) -> Result<Option<ClaudeCandidate>, ClaudeResolveError> {
        let project_hint = encode_project_hint(cwd).ok_or(ClaudeResolveError::InvalidTarget)?;
        let canonical_path = canonical_or_original(path);
        let allowed = roots.iter().any(|root| {
            canonical_path
                .parent()
                .is_some_and(|parent| parent == canonical_or_original(&root.join(&project_hint)))
        });
        if !allowed {
            return Err(ClaudeResolveError::InvalidTarget);
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ClaudeResolveError::Retryable),
        };
        if !metadata.file_type().is_file() {
            return Err(ClaudeResolveError::InvalidTarget);
        }
        let metadata = fs::metadata(&canonical_path).map_err(|_| ClaudeResolveError::Retryable)?;
        let fingerprint = Fingerprint {
            size: metadata.len(),
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        };
        let header = self
            .header(&canonical_path, fingerprint)
            .ok_or(ClaudeResolveError::Retryable)?;
        let filename_matches = canonical_path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == header.session_identity);
        if !filename_matches || canonical_or_original(&header.cwd) != canonical_or_original(cwd) {
            return Err(ClaudeResolveError::InvalidTarget);
        }
        Ok(Some(ClaudeCandidate {
            path: canonical_path,
            session_identity: header.session_identity,
            cwd: header.cwd,
            size: fingerprint.size,
            modified_at: fingerprint.modified_at,
        }))
    }

    pub fn resolve_exact(
        &mut self,
        roots: &[PathBuf],
        cwd: &Path,
        session_identity: &str,
    ) -> Result<Option<ClaudeCandidate>, ClaudeResolveError> {
        if !is_uuid(session_identity) {
            return Err(ClaudeResolveError::InvalidTarget);
        }
        let project_hint = encode_project_hint(cwd).ok_or(ClaudeResolveError::InvalidTarget)?;
        let expected_cwd = canonical_or_original(cwd);
        let mut paths = HashSet::new();
        for root in roots {
            let path = root
                .join(&project_hint)
                .join(format!("{session_identity}.jsonl"));
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(ClaudeResolveError::InvalidTarget),
            };
            if !metadata.file_type().is_file() {
                return Err(ClaudeResolveError::InvalidTarget);
            }
            paths.insert(canonical_or_original(&path));
        }
        if paths.len() > 1 {
            return Err(ClaudeResolveError::AmbiguousIdentity);
        }
        let Some(path) = paths.into_iter().next() else {
            return Ok(None);
        };
        let metadata = fs::metadata(&path).map_err(|_| ClaudeResolveError::InvalidTarget)?;
        let fingerprint = Fingerprint {
            size: metadata.len(),
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        };
        let header = self
            .header(&path, fingerprint)
            .ok_or(ClaudeResolveError::InvalidTarget)?;
        if header.session_identity != session_identity
            || canonical_or_original(&header.cwd) != expected_cwd
        {
            return Err(ClaudeResolveError::InvalidTarget);
        }
        Ok(Some(ClaudeCandidate {
            path,
            session_identity: header.session_identity,
            cwd: header.cwd,
            size: fingerprint.size,
            modified_at: fingerprint.modified_at,
        }))
    }

    pub fn scan_ordinary(
        &mut self,
        roots: &[PathBuf],
        cwd: &Path,
        now: SystemTime,
    ) -> Vec<ClaudeCandidate> {
        let Some(project_hint) = encode_project_hint(cwd) else {
            return Vec::new();
        };
        let expected_cwd = canonical_or_original(cwd);
        let mut seen = HashSet::new();
        let mut paths = Vec::new();
        for root in roots {
            let project = root.join(&project_hint);
            let Ok(entries) = fs::read_dir(project) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if !file_type.is_file() || !is_jsonl(&path) {
                    continue;
                }
                let path = canonical_or_original(&path);
                if !seen.insert(path.clone()) {
                    continue;
                }
                let Ok(metadata) = fs::metadata(&path) else {
                    continue;
                };
                let modified_at = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if now.duration_since(modified_at).unwrap_or_default() > ORDINARY_MAX_AGE {
                    continue;
                }
                paths.push((
                    path,
                    Fingerprint {
                        size: metadata.len(),
                        modified_at,
                    },
                ));
            }
        }
        paths.sort_by(|(left_path, left), (right_path, right)| {
            right
                .modified_at
                .cmp(&left.modified_at)
                .then_with(|| left_path.cmp(right_path))
        });

        let mut candidates = Vec::new();
        for (path, fingerprint) in paths {
            let header = self.header(&path, fingerprint);
            let Some(header) = header else {
                continue;
            };
            let filename_matches = path
                .file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value == header.session_identity);
            if !filename_matches || canonical_or_original(&header.cwd) != expected_cwd {
                continue;
            }
            candidates.push(ClaudeCandidate {
                path,
                session_identity: header.session_identity,
                cwd: header.cwd,
                size: fingerprint.size,
                modified_at: fingerprint.modified_at,
            });
            if candidates.len() == ORDINARY_MAX_CANDIDATES {
                break;
            }
        }
        candidates
    }

    fn header(&mut self, path: &Path, fingerprint: Fingerprint) -> Option<ClaudeSessionHeader> {
        if let Some(cached) = self.cache.get(path) {
            if cached.fingerprint == fingerprint {
                return cached.header.clone();
            }
        }
        let header = parse_session_header(path).ok();
        self.cache.insert(
            path.to_owned(),
            CachedHeader {
                fingerprint,
                header: header.clone(),
            },
        );
        header
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

pub fn encode_project_hint(cwd: &Path) -> Option<OsString> {
    cwd.is_absolute().then_some(())?;
    let value = cwd.to_str()?.replace(['/', '.', '_'], "-");
    (!value.is_empty()).then(|| OsString::from(value))
}

pub fn inspect_cli(processes: &[ProcessCommand]) -> ClaudeCliState {
    let mut found = false;
    let mut ineligible = false;
    let mut invalid = false;
    let mut exact_session = None;
    for command in processes
        .iter()
        .filter(|command| is_claude_command(command))
    {
        found = true;
        match inspect_command(command) {
            ClaudeCliState::Ineligible => ineligible = true,
            ClaudeCliState::Invalid => invalid = true,
            ClaudeCliState::Eligible {
                exact_session: Some(identity),
            } => {
                if set_exact_id(&mut exact_session, &identity).is_err() {
                    invalid = true;
                }
            }
            ClaudeCliState::Eligible {
                exact_session: None,
            } => {}
        }
    }
    if ineligible {
        ClaudeCliState::Ineligible
    } else if invalid {
        ClaudeCliState::Invalid
    } else if found {
        ClaudeCliState::Eligible { exact_session }
    } else {
        ClaudeCliState::Eligible {
            exact_session: None,
        }
    }
}

fn inspect_command(command: &ProcessCommand) -> ClaudeCliState {
    let owned;
    let tokens = if let Some(argv) = command.argv.as_deref().filter(|argv| !argv.is_empty()) {
        argv
    } else if let Some(cmdline) = command.cmdline.as_deref() {
        let Ok(parsed) = split_command_line(cmdline) else {
            return ClaudeCliState::Invalid;
        };
        owned = parsed;
        &owned
    } else {
        return ClaudeCliState::Eligible {
            exact_session: None,
        };
    };
    let mut session_id = None;
    let mut resume_id = None;
    let mut fork_session = false;
    let mut index = usize::from(tokens.first().is_some_and(|token| is_claude_name(token)));
    while index < tokens.len() {
        let token = &tokens[index];
        if token == "--" {
            break;
        }
        if matches!(
            token.as_str(),
            "--print" | "-p" | "--background" | "--bg" | "--no-session-persistence"
        ) {
            return ClaudeCliState::Ineligible;
        }
        if token == "--fork-session" {
            fork_session = true;
            index += 1;
            continue;
        }
        if token == "--session-id" {
            let Some(value) = tokens.get(index + 1).filter(|value| is_uuid(value)) else {
                return ClaudeCliState::Invalid;
            };
            if set_exact_id(&mut session_id, value).is_err() {
                return ClaudeCliState::Invalid;
            }
            index += 2;
            continue;
        }
        if let Some(value) = token.strip_prefix("--session-id=") {
            if !is_uuid(value) || set_exact_id(&mut session_id, value).is_err() {
                return ClaudeCliState::Invalid;
            }
            index += 1;
            continue;
        }
        if token == "--resume" || token == "-r" {
            let Some(value) = tokens.get(index + 1) else {
                return ClaudeCliState::Invalid;
            };
            if is_uuid(value) && set_exact_id(&mut resume_id, value).is_err() {
                return ClaudeCliState::Invalid;
            }
            index += 2;
            continue;
        }
        if let Some(value) = token.strip_prefix("--resume=") {
            if value.is_empty() {
                return ClaudeCliState::Invalid;
            }
            if is_uuid(value) && set_exact_id(&mut resume_id, value).is_err() {
                return ClaudeCliState::Invalid;
            }
            index += 1;
            continue;
        }
        if takes_required_value(token) {
            if tokens.get(index + 1).is_none() {
                return ClaudeCliState::Invalid;
            }
            index += 2;
            continue;
        }
        index += 1;
    }

    if fork_session {
        return ClaudeCliState::Eligible {
            exact_session: session_id,
        };
    }
    if session_id.is_some() && resume_id.is_some() && session_id != resume_id {
        return ClaudeCliState::Invalid;
    }
    ClaudeCliState::Eligible {
        exact_session: session_id.or(resume_id),
    }
}

fn takes_required_value(token: &str) -> bool {
    matches!(
        token,
        "--add-dir"
            | "--agent"
            | "--agents"
            | "--allowedTools"
            | "--allowed-tools"
            | "--append-system-prompt"
            | "--betas"
            | "--debug-file"
            | "--disallowedTools"
            | "--disallowed-tools"
            | "--effort"
            | "--fallback-model"
            | "--file"
            | "--input-format"
            | "--json-schema"
            | "--max-budget-usd"
            | "--mcp-config"
            | "--model"
            | "--name"
            | "-n"
            | "--output-format"
            | "--permission-mode"
            | "--plugin-dir"
            | "--plugin-url"
            | "--remote-control-session-name-prefix"
            | "--setting-sources"
            | "--settings"
            | "--system-prompt"
            | "--tools"
    )
}

fn split_command_line(input: &str) -> Result<Vec<String>, ()> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            started = true;
            continue;
        }
        match (quote, character) {
            (Some('\''), '\'') | (Some('"'), '"') => {
                quote = None;
                started = true;
            }
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (Some('\''), _) => {
                current.push(character);
                started = true;
            }
            (_, '\\') => escaped = true,
            (None, value) if value.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (_, value) => {
                current.push(value);
                started = true;
            }
        }
    }
    if quote.is_some() || escaped {
        return Err(());
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

fn set_exact_id(slot: &mut Option<String>, value: &str) -> Result<(), ()> {
    if slot.as_deref().is_some_and(|current| current != value) {
        return Err(());
    }
    *slot = Some(value.to_owned());
    Ok(())
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_claude_command(command: &ProcessCommand) -> bool {
    is_claude_name(&command.name)
        || command.argv0.as_deref().is_some_and(is_claude_name)
        || command
            .argv
            .as_ref()
            .and_then(|argv| argv.first())
            .is_some_and(|value| is_claude_name(value))
}

fn is_claude_name(value: &str) -> bool {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("claude"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn command(args: &[&str]) -> ProcessCommand {
        ProcessCommand {
            pid: 1,
            name: "claude".into(),
            argv: Some(args.iter().map(|value| (*value).to_owned()).collect()),
            argv0: Some("claude".into()),
            cmdline: None,
        }
    }

    fn session_text(session_id: &str, cwd: &str) -> String {
        format!(
            concat!(
                "{{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",",
                "\"parentUuid\":null,\"sessionId\":\"{session_id}\",\"cwd\":\"{cwd}\",",
                "\"isSidechain\":false,\"message\":{{\"role\":\"user\",\"content\":\"Task\"}}}}\n"
            ),
            session_id = session_id,
            cwd = cwd
        )
    }

    #[test]
    fn exact_identity_bypasses_the_ordinary_age_limit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let project = root.join("-work-project");
        fs::create_dir_all(&project).unwrap();
        let session_id = "10000000-0000-4000-8000-000000000001";
        let path = project.join(format!("{session_id}.jsonl"));
        fs::write(&path, session_text(session_id, "/work/project")).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();

        let candidate = ClaudeScanner::default()
            .resolve_exact(&[root], Path::new("/work/project"), session_id)
            .unwrap()
            .unwrap();

        assert_eq!(candidate.path, fs::canonicalize(path).unwrap());
        assert_eq!(candidate.session_identity, session_id);
    }

    #[test]
    fn duplicate_exact_identity_across_roots_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = "10000000-0000-4000-8000-000000000001";
        let roots = [temp.path().join("one"), temp.path().join("two")];
        for root in &roots {
            let project = root.join("-work-project");
            fs::create_dir_all(&project).unwrap();
            fs::write(
                project.join(format!("{session_id}.jsonl")),
                session_text(session_id, "/work/project"),
            )
            .unwrap();
        }

        assert_eq!(
            ClaudeScanner::default().resolve_exact(&roots, Path::new("/work/project"), session_id),
            Err(ClaudeResolveError::AmbiguousIdentity)
        );
    }

    #[test]
    fn ordinary_age_boundary_is_inclusive_at_thirty_days() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let project = root.join("-work-project");
        fs::create_dir_all(&project).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 24 * 60 * 60);
        for (session_id, age) in [
            ("10000000-0000-4000-8000-000000000001", ORDINARY_MAX_AGE),
            (
                "10000000-0000-4000-8000-000000000002",
                ORDINARY_MAX_AGE + Duration::from_secs(1),
            ),
        ] {
            let path = project.join(format!("{session_id}.jsonl"));
            fs::write(&path, session_text(session_id, "/work/project")).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - age)
                .unwrap();
        }

        let candidates =
            ClaudeScanner::default().scan_ordinary(&[root], Path::new("/work/project"), now);

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].session_identity,
            "10000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn invalid_candidates_do_not_consume_the_ordinary_quota() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let project = root.join("-work-project");
        fs::create_dir_all(&project).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 24 * 60 * 60);
        let malformed = project.join("malformed.jsonl");
        fs::write(&malformed, "not-json\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&malformed)
            .unwrap()
            .set_modified(now + Duration::from_secs(2))
            .unwrap();
        let collision_id = "20000000-0000-4000-8000-000000000000";
        let collision = project.join(format!("{collision_id}.jsonl"));
        fs::write(&collision, session_text(collision_id, "/other/project")).unwrap();
        fs::File::options()
            .write(true)
            .open(&collision)
            .unwrap()
            .set_modified(now + Duration::from_secs(1))
            .unwrap();

        for index in 0..26_u64 {
            let session_id = format!("10000000-0000-4000-8000-{index:012x}");
            let path = project.join(format!("{session_id}.jsonl"));
            fs::write(&path, session_text(&session_id, "/work/project")).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - Duration::from_secs(index))
                .unwrap();
        }

        let candidates =
            ClaudeScanner::default().scan_ordinary(&[root], Path::new("/work/project"), now);

        assert_eq!(candidates.len(), 25);
        assert_eq!(
            candidates.first().unwrap().session_identity,
            "10000000-0000-4000-8000-000000000000"
        );
        assert_eq!(
            candidates.last().unwrap().session_identity,
            "10000000-0000-4000-8000-000000000018"
        );
    }

    #[test]
    fn scans_only_direct_compatible_project_transcripts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("projects");
        let project = root.join("-work-project");
        let nested = project.join("subagents");
        fs::create_dir_all(&nested).unwrap();
        let direct_id = "10000000-0000-4000-8000-000000000001";
        let direct = project.join(format!("{direct_id}.jsonl"));
        fs::write(&direct, session_text(direct_id, "/work/project")).unwrap();
        let nested_id = "10000000-0000-4000-8000-000000000002";
        fs::write(
            nested.join(format!("{nested_id}.jsonl")),
            session_text(nested_id, "/work/project"),
        )
        .unwrap();
        let collision_id = "10000000-0000-4000-8000-000000000003";
        fs::write(
            project.join(format!("{collision_id}.jsonl")),
            session_text(collision_id, "/other/project"),
        )
        .unwrap();

        let candidates = ClaudeScanner::default().scan_ordinary(
            &[root],
            Path::new("/work/project"),
            SystemTime::now(),
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, fs::canonicalize(direct).unwrap());
        assert_eq!(candidates[0].session_identity, direct_id);
    }

    #[test]
    fn encodes_only_the_claude_project_hint_characters() {
        assert_eq!(
            encode_project_hint(Path::new("/work/my.project_dir")),
            Some(OsString::from("-work-my-project-dir"))
        );
    }

    #[test]
    fn excludes_headless_modes_and_accepts_interactive_invocations() {
        for args in [
            vec!["claude", "--print"],
            vec!["claude", "-p"],
            vec!["claude", "--background"],
            vec!["claude", "--bg"],
            vec!["claude", "--no-session-persistence"],
        ] {
            assert_eq!(inspect_cli(&[command(&args)]), ClaudeCliState::Ineligible);
        }
        assert_eq!(
            inspect_cli(&[command(&["claude", "--continue"])]),
            ClaudeCliState::Eligible {
                exact_session: None
            }
        );
        assert_eq!(
            inspect_cli(&[command(&["claude", "--", "--print"])]),
            ClaudeCliState::Eligible {
                exact_session: None
            }
        );
    }

    #[test]
    fn evaluates_all_claude_processes_and_falls_back_from_empty_argv() {
        assert_eq!(
            inspect_cli(&[command(&["claude"]), command(&["claude", "--background"]),]),
            ClaudeCliState::Ineligible
        );
        let empty_argv = ProcessCommand {
            pid: 1,
            name: "claude".into(),
            argv: Some(Vec::new()),
            argv0: Some("claude".into()),
            cmdline: Some("claude --print".into()),
        };
        assert_eq!(inspect_cli(&[empty_argv]), ClaudeCliState::Ineligible);
    }

    #[test]
    fn parses_fallback_cmdline_quotes_and_rejects_malformed_input() {
        let quoted = ProcessCommand {
            pid: 1,
            name: "claude".into(),
            argv: None,
            argv0: Some("claude".into()),
            cmdline: Some("claude --name '--print is text'".into()),
        };
        assert_eq!(
            inspect_cli(&[quoted]),
            ClaudeCliState::Eligible {
                exact_session: None
            }
        );
        assert_eq!(
            inspect_cli(&[command(&["claude", "--system-prompt", "-p"])]),
            ClaudeCliState::Eligible {
                exact_session: None
            }
        );
        let malformed = ProcessCommand {
            pid: 1,
            name: "claude".into(),
            argv: None,
            argv0: Some("claude".into()),
            cmdline: Some("claude --name 'unfinished".into()),
        };
        assert_eq!(inspect_cli(&[malformed]), ClaudeCliState::Invalid);
    }

    #[test]
    fn extracts_only_safe_exact_session_hints() {
        let id = "10000000-0000-4000-8000-000000000001";
        let other = "10000000-0000-4000-8000-000000000002";
        for args in [
            vec!["claude", "--session-id", id],
            vec![
                "claude",
                "--session-id=10000000-0000-4000-8000-000000000001",
            ],
            vec!["claude", "--resume", id],
            vec!["claude", "--resume=10000000-0000-4000-8000-000000000001"],
            vec!["claude", "-r", id],
        ] {
            assert_eq!(
                inspect_cli(&[command(&args)]),
                ClaudeCliState::Eligible {
                    exact_session: Some(id.into())
                }
            );
        }
        assert_eq!(
            inspect_cli(&[command(&["claude", "--resume", "named-session"])]),
            ClaudeCliState::Eligible {
                exact_session: None
            }
        );
        assert_eq!(
            inspect_cli(&[command(&["claude", "--fork-session", "--resume", id])]),
            ClaudeCliState::Eligible {
                exact_session: None
            }
        );
        assert_eq!(
            inspect_cli(&[command(&[
                "claude",
                "--fork-session",
                "--resume",
                id,
                "--session-id",
                other
            ])]),
            ClaudeCliState::Eligible {
                exact_session: Some(other.into())
            }
        );
        assert_eq!(
            inspect_cli(&[command(&["claude", "--session-id"])]),
            ClaudeCliState::Invalid
        );
        assert_eq!(
            inspect_cli(&[command(&["claude", "--session-id", id, "--resume", other])]),
            ClaudeCliState::Invalid
        );
    }
}
