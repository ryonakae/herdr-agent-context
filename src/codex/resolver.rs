use super::session::{CodexSessionError, filename_identity, parse_session};
use crate::backend::ProcessCommand;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const ORDINARY_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const ORDINARY_MAX_CANDIDATES: usize = 25;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexCandidate {
    pub path: PathBuf,
    pub root: PathBuf,
    pub session_identity: String,
    pub cwd: PathBuf,
    pub size: u64,
    pub modified_at: SystemTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexResolveError {
    InvalidTarget,
    AmbiguousIdentity,
    Retryable,
}

#[derive(Default)]
pub struct CodexScanner;

impl CodexScanner {
    pub fn validate_path(
        &mut self,
        roots: &[PathBuf],
        cwd: &Path,
        path: &Path,
    ) -> Result<Option<CodexCandidate>, CodexResolveError> {
        let Some(root) = controlled_root(roots, path) else {
            return Err(CodexResolveError::InvalidTarget);
        };
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(CodexResolveError::Retryable),
        };
        if !metadata.file_type().is_file() {
            return Err(CodexResolveError::InvalidTarget);
        }
        let canonical = fs::canonicalize(path).map_err(|_| CodexResolveError::Retryable)?;
        if !canonical.starts_with(&root) {
            return Err(CodexResolveError::InvalidTarget);
        }
        let metadata = fs::metadata(&canonical).map_err(|_| CodexResolveError::Retryable)?;
        let index = root
            .parent()
            .map(|parent| parent.join("session_index.jsonl"));
        let view = parse_session(&canonical, index.as_deref()).map_err(|error| match error {
            CodexSessionError::Read | CodexSessionError::IncompleteTail => {
                CodexResolveError::Retryable
            }
            _ => CodexResolveError::InvalidTarget,
        })?;
        if canonical_or_original(&view.header.cwd) != canonical_or_original(cwd) {
            return Err(CodexResolveError::InvalidTarget);
        }
        Ok(Some(CodexCandidate {
            path: canonical,
            root,
            session_identity: view.header.session_identity,
            cwd: view.header.cwd,
            size: metadata.len(),
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        }))
    }

    pub fn resolve_exact(
        &mut self,
        roots: &[PathBuf],
        cwd: &Path,
        identity: &str,
    ) -> Result<Option<CodexCandidate>, CodexResolveError> {
        if !is_uuid(identity) {
            return Err(CodexResolveError::InvalidTarget);
        }
        let mut matches = Vec::new();
        for path in controlled_paths(roots, false) {
            if filename_identity(&path) != Some(identity) {
                continue;
            }
            match self.validate_path(roots, cwd, &path) {
                Ok(Some(candidate)) if candidate.session_identity == identity => {
                    matches.push(candidate)
                }
                Ok(Some(_)) | Ok(None) | Err(_) => return Err(CodexResolveError::InvalidTarget),
            }
        }
        if matches.len() > 1 {
            return Err(CodexResolveError::AmbiguousIdentity);
        }
        Ok(matches.pop())
    }

    pub fn scan_ordinary(
        &mut self,
        roots: &[PathBuf],
        cwd: &Path,
        now: SystemTime,
    ) -> Result<Vec<CodexCandidate>, CodexResolveError> {
        let mut paths = Vec::new();
        for path in controlled_paths(roots, true) {
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let modified_at = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if now.duration_since(modified_at).unwrap_or_default() > ORDINARY_MAX_AGE {
                continue;
            }
            paths.push((path, modified_at));
        }
        paths.sort_by(|(left_path, left_time), (right_path, right_time)| {
            right_time
                .cmp(left_time)
                .then_with(|| left_path.cmp(right_path))
        });
        let mut candidates = Vec::new();
        let mut identities = HashSet::new();
        for (path, _) in paths {
            let Ok(Some(candidate)) = self.validate_path(roots, cwd, &path) else {
                continue;
            };
            if !identities.insert(candidate.session_identity.clone()) {
                return Err(CodexResolveError::AmbiguousIdentity);
            }
            candidates.push(candidate);
            if candidates.len() == ORDINARY_MAX_CANDIDATES {
                break;
            }
        }
        Ok(candidates)
    }
}

fn controlled_paths(roots: &[PathBuf], regular_only: bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let Ok(years) = fs::read_dir(root) else {
            continue;
        };
        for year in years.flatten().filter(|entry| controlled_dir(entry, 4)) {
            let Ok(months) = fs::read_dir(year.path()) else {
                continue;
            };
            for month in months.flatten().filter(|entry| controlled_dir(entry, 2)) {
                let Ok(days) = fs::read_dir(month.path()) else {
                    continue;
                };
                for day in days.flatten().filter(|entry| controlled_dir(entry, 2)) {
                    let Ok(files) = fs::read_dir(day.path()) else {
                        continue;
                    };
                    for file in files.flatten() {
                        let Ok(file_type) = file.file_type() else {
                            continue;
                        };
                        if regular_only && !file_type.is_file() {
                            continue;
                        }
                        let path = file.path();
                        let dedupe_key = if regular_only {
                            canonical_or_original(&path)
                        } else {
                            path.clone()
                        };
                        if filename_identity(&path).is_some() && seen.insert(dedupe_key) {
                            paths.push(path);
                        }
                    }
                }
            }
        }
    }
    paths
}

fn controlled_dir(entry: &fs::DirEntry, width: usize) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_dir())
        && entry.file_name().to_str().is_some_and(|name| {
            name.len() == width && name.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn controlled_root(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    roots.iter().find_map(|root| {
        let root = canonical_or_original(root);
        let relative = canonical_or_original(path)
            .strip_prefix(&root)
            .ok()?
            .to_owned();
        let components: Vec<_> = relative.components().collect();
        (components.len() == 4
            && components[0]
                .as_os_str()
                .to_str()
                .is_some_and(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()))
            && components[1]
                .as_os_str()
                .to_str()
                .is_some_and(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_digit()))
            && components[2]
                .as_os_str()
                .to_str()
                .is_some_and(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_digit())))
        .then_some(root)
    })
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexCliState {
    Ineligible,
    Invalid,
    Eligible { exact_session: Option<String> },
}

pub fn inspect_cli(processes: &[ProcessCommand]) -> CodexCliState {
    let mut found = false;
    let mut exact_session = None;
    for command in processes.iter().filter(|command| is_codex_command(command)) {
        let Some(tokens) = command.argv.as_deref().filter(|tokens| !tokens.is_empty()) else {
            return CodexCliState::Ineligible;
        };
        found = true;
        let offset = usize::from(tokens.first().is_some_and(|token| is_codex_name(token)));
        match inspect_args(&tokens[offset..]) {
            CodexCliState::Ineligible => return CodexCliState::Ineligible,
            CodexCliState::Invalid => return CodexCliState::Invalid,
            CodexCliState::Eligible {
                exact_session: Some(identity),
            } => {
                if exact_session
                    .as_deref()
                    .is_some_and(|current| current != identity)
                {
                    return CodexCliState::Invalid;
                }
                exact_session = Some(identity);
            }
            CodexCliState::Eligible {
                exact_session: None,
            } => {}
        }
    }
    if found {
        CodexCliState::Eligible { exact_session }
    } else {
        CodexCliState::Ineligible
    }
}

fn inspect_args(args: &[String]) -> CodexCliState {
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            if positionals.first().copied() == Some("resume") {
                positionals.extend(args[index + 1..].iter().map(String::as_str));
            }
            break;
        }
        if matches!(token.as_str(), "-h" | "--help" | "-V" | "--version") {
            return CodexCliState::Ineligible;
        }
        if token == "--remote" || token.starts_with("--remote=") || token == "--ephemeral" {
            return CodexCliState::Ineligible;
        }
        if token.starts_with('-') {
            if option_takes_value(token) && !token.contains('=') {
                if args.get(index + 1).is_none() {
                    return CodexCliState::Ineligible;
                }
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        positionals.push(token.as_str());
        index += 1;
    }

    let Some(mode) = positionals.first().copied() else {
        return CodexCliState::Eligible {
            exact_session: None,
        };
    };
    if excluded_subcommand(mode) {
        return CodexCliState::Ineligible;
    }
    if mode != "resume" {
        return CodexCliState::Eligible {
            exact_session: None,
        };
    }
    let Some(target) = positionals.get(1).copied() else {
        return CodexCliState::Eligible {
            exact_session: None,
        };
    };
    if is_uuid(target) {
        CodexCliState::Eligible {
            exact_session: Some(target.to_owned()),
        }
    } else if looks_like_uuid(target) {
        CodexCliState::Invalid
    } else {
        CodexCliState::Eligible {
            exact_session: None,
        }
    }
}

fn option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "--config"
            | "-c"
            | "--enable"
            | "--disable"
            | "--image"
            | "-i"
            | "--model"
            | "-m"
            | "--local-provider"
            | "--profile"
            | "-p"
            | "--sandbox"
            | "-s"
            | "--ask-for-approval"
            | "-a"
            | "--cd"
            | "-C"
            | "--add-dir"
    )
}

fn excluded_subcommand(value: &str) -> bool {
    matches!(
        value,
        "exec"
            | "review"
            | "remote"
            | "cloud"
            | "mcp"
            | "mcp-server"
            | "app-server"
            | "login"
            | "logout"
            | "completion"
            | "debug"
            | "apply"
            | "sandbox"
            | "features"
            | "agents"
            | "remote-control"
            | "app"
            | "queue"
            | "archive"
            | "unarchive"
            | "migrate-rollouts"
            | "exec-server"
            | "help"
    )
}

fn is_codex_command(command: &ProcessCommand) -> bool {
    is_codex_name(&command.name)
        || command.argv0.as_deref().is_some_and(is_codex_name)
        || command
            .argv
            .as_ref()
            .and_then(|argv| argv.first())
            .is_some_and(|value| is_codex_name(value))
}

fn is_codex_name(value: &str) -> bool {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("codex"))
}

fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| !matches!(index, 8 | 13 | 18 | 23) || byte == b'-')
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ProcessCommand;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn command(args: Option<&[&str]>) -> ProcessCommand {
        ProcessCommand {
            name: "codex".into(),
            argv: args.map(|args| args.iter().map(|value| (*value).to_owned()).collect()),
            argv0: Some("codex".into()),
            cmdline: None,
        }
    }

    fn rollout_text(id: &str, cwd: &str) -> String {
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{cwd}\",\"source\":\"cli\"}}}}\n"
        )
    }

    fn write_rollout(root: &Path, id: &str, cwd: &str, modified: SystemTime) -> std::path::PathBuf {
        let day = root.join("2026/08/28");
        fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-2026-08-28T00-00-00-{id}.jsonl"));
        fs::write(&path, rollout_text(id, cwd)).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    }

    #[test]
    fn discovery_and_exact_resolution_share_the_official_filename_validator() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let day = root.join("2026/08/28");
        fs::create_dir_all(&day).unwrap();
        let valid_id = "10000000-0000-4000-8000-000000000001";
        let rollout_id = "20000000-0000-4000-8000-000000000002";
        let valid = day.join(format!(
            "rollout-2026-08-28T01-02-03-{valid_id}_{rollout_id}.jsonl"
        ));
        fs::write(&valid, rollout_text(valid_id, "/synthetic/project")).unwrap();

        for (name, id) in [
            (
                "rollout-copy-25000000-0000-4000-8000-000000000002.jsonl".to_owned(),
                "25000000-0000-4000-8000-000000000002".to_owned(),
            ),
            (
                "rollout-2026-02-29T01-02-03-30000000-0000-4000-8000-000000000003.jsonl".to_owned(),
                "30000000-0000-4000-8000-000000000003".to_owned(),
            ),
            (
                "rollout-2026-08-28T01-02-03-40000000-0000-4000-8000-000000000004_bad.jsonl"
                    .to_owned(),
                "40000000-0000-4000-8000-000000000004".to_owned(),
            ),
        ] {
            fs::write(day.join(name), rollout_text(&id, "/synthetic/project")).unwrap();
        }

        let mut scanner = CodexScanner;
        let candidates = scanner
            .scan_ordinary(
                std::slice::from_ref(&root),
                Path::new("/synthetic/project"),
                SystemTime::now(),
            )
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, fs::canonicalize(valid).unwrap());
        assert_eq!(candidates[0].session_identity, valid_id);

        assert_eq!(
            scanner
                .resolve_exact(
                    std::slice::from_ref(&root),
                    Path::new("/synthetic/project"),
                    valid_id,
                )
                .unwrap()
                .unwrap()
                .session_identity,
            valid_id
        );
        for invalid_id in [
            "25000000-0000-4000-8000-000000000002",
            "30000000-0000-4000-8000-000000000003",
            "40000000-0000-4000-8000-000000000004",
        ] {
            assert_eq!(
                scanner.resolve_exact(
                    std::slice::from_ref(&root),
                    Path::new("/synthetic/project"),
                    invalid_id,
                ),
                Ok(None)
            );
        }
    }

    #[test]
    fn scans_only_controlled_active_rollouts_and_exact_bypasses_age() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 24 * 60 * 60);
        let active_id = "10000000-0000-4000-8000-000000000001";
        let old_id = "10000000-0000-4000-8000-000000000002";
        let active = write_rollout(&root, active_id, "/synthetic/project", now);
        write_rollout(
            &root,
            old_id,
            "/synthetic/project",
            now - Duration::from_secs(31 * 24 * 60 * 60),
        );
        fs::write(
            root.join(format!("rollout-direct-{active_id}.jsonl")),
            rollout_text(active_id, "/synthetic/project"),
        )
        .unwrap();
        fs::write(
            root.join("2026/08/28/archive.jsonl.gz"),
            rollout_text(active_id, "/synthetic/project"),
        )
        .unwrap();
        fs::create_dir_all(root.join("2026/08/28/extra")).unwrap();
        fs::write(
            root.join(format!("2026/08/28/extra/rollout-{active_id}.jsonl")),
            rollout_text(active_id, "/synthetic/project"),
        )
        .unwrap();

        let mut scanner = CodexScanner;
        let candidates = scanner
            .scan_ordinary(
                std::slice::from_ref(&root),
                Path::new("/synthetic/project"),
                now,
            )
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, fs::canonicalize(active).unwrap());

        let exact = scanner
            .resolve_exact(&[root], Path::new("/synthetic/project"), old_id)
            .unwrap()
            .unwrap();
        assert_eq!(exact.session_identity, old_id);
    }

    #[test]
    fn ordinary_age_boundary_is_inclusive_and_symlink_or_non_cli_rollouts_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 24 * 60 * 60);
        let boundary_id = "10000000-0000-4000-8000-000000000010";
        let expired_id = "10000000-0000-4000-8000-000000000011";
        write_rollout(
            &root,
            boundary_id,
            "/synthetic/project",
            now - ORDINARY_MAX_AGE,
        );
        write_rollout(
            &root,
            expired_id,
            "/synthetic/project",
            now - ORDINARY_MAX_AGE - Duration::from_secs(1),
        );
        let invalid_source_id = "10000000-0000-4000-8000-000000000012";
        let invalid_source = write_rollout(&root, invalid_source_id, "/synthetic/project", now);
        fs::write(
            &invalid_source,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{invalid_source_id}\",\"cwd\":\"/synthetic/project\",\"source\":\"subagent\"}}}}\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = temp.path().join("outside.jsonl");
            fs::write(&outside, rollout_text(boundary_id, "/synthetic/project")).unwrap();
            symlink(
                outside,
                root.join(format!(
                    "2026/08/28/rollout-2026-08-28T01-00-00-{boundary_id}.jsonl"
                )),
            )
            .unwrap();
        }

        let mut scanner = CodexScanner;
        let candidates = scanner
            .scan_ordinary(
                std::slice::from_ref(&root),
                Path::new("/synthetic/project"),
                now,
            )
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].session_identity, boundary_id);
        #[cfg(unix)]
        assert_eq!(
            scanner.resolve_exact(
                std::slice::from_ref(&root),
                Path::new("/synthetic/project"),
                boundary_id,
            ),
            Err(CodexResolveError::InvalidTarget)
        );
        assert_eq!(
            scanner.resolve_exact(
                std::slice::from_ref(&root),
                Path::new("/synthetic/project"),
                invalid_source_id,
            ),
            Err(CodexResolveError::InvalidTarget)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entry_cannot_hide_its_regular_in_root_target_from_ordinary_scan() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let day = root.join("2026/08/28");
        fs::create_dir_all(&day).unwrap();
        let id = "10000000-0000-4000-8000-000000000020";
        let regular = day.join(format!("rollout-2026-08-28T02-00-00-{id}.jsonl"));
        let link = day.join(format!("rollout-2026-08-28T01-00-00-{id}.jsonl"));
        symlink(&regular, &link).unwrap();
        fs::write(&regular, rollout_text(id, "/synthetic/project")).unwrap();

        let candidates = CodexScanner
            .scan_ordinary(&[root], Path::new("/synthetic/project"), SystemTime::now())
            .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, fs::canonicalize(regular).unwrap());
    }

    #[test]
    fn ordinary_scan_limits_compatible_candidates_and_duplicate_exact_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let first_root = temp.path().join("one/sessions");
        let second_root = temp.path().join("two/sessions");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 24 * 60 * 60);
        for index in 0..26_u64 {
            let id = format!("10000000-0000-4000-8000-{index:012x}");
            write_rollout(
                &first_root,
                &id,
                "/synthetic/project",
                now - Duration::from_secs(index),
            );
        }
        let malformed = first_root.join("2026/08/28/rollout-newer-malformed.jsonl");
        fs::write(&malformed, "not-json\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&malformed)
            .unwrap()
            .set_modified(now + Duration::from_secs(1))
            .unwrap();

        let mut scanner = CodexScanner;
        let candidates = scanner
            .scan_ordinary(
                std::slice::from_ref(&first_root),
                Path::new("/synthetic/project"),
                now,
            )
            .unwrap();
        assert_eq!(candidates.len(), 25);
        assert_eq!(
            candidates.first().unwrap().session_identity,
            "10000000-0000-4000-8000-000000000000"
        );
        assert_eq!(
            candidates.last().unwrap().session_identity,
            "10000000-0000-4000-8000-000000000018"
        );

        let duplicate_id = "20000000-0000-4000-8000-000000000001";
        write_rollout(&first_root, duplicate_id, "/synthetic/project", now);
        write_rollout(&second_root, duplicate_id, "/synthetic/project", now);
        assert_eq!(
            scanner.resolve_exact(
                &[first_root, second_root],
                Path::new("/synthetic/project"),
                duplicate_id,
            ),
            Err(CodexResolveError::AmbiguousIdentity)
        );
    }

    #[test]
    fn rejects_conflicting_or_malformed_uuid_evidence_but_allows_picker_targets() {
        let first = "10000000-0000-4000-8000-000000000001";
        let second = "10000000-0000-4000-8000-000000000002";
        assert_eq!(
            inspect_cli(&[
                command(Some(&["codex", "resume", first])),
                command(Some(&["codex", "resume", second])),
            ]),
            CodexCliState::Invalid
        );
        assert_eq!(
            inspect_cli(&[command(Some(&[
                "codex",
                "resume",
                "10000000-0000-4000-8000-00000000000z",
            ]))]),
            CodexCliState::Invalid
        );
        for args in [&["codex", "resume"][..], &["codex", "fork"][..]] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Eligible {
                    exact_session: None
                }
            );
        }
    }

    #[test]
    fn top_level_separator_protects_prompt_text_while_resume_separator_keeps_its_target() {
        let id = "10000000-0000-4000-8000-000000000001";
        for args in [
            &["codex", "--", "resume", id][..],
            &["codex", "--", "exec"][..],
            &["codex", "--", "--help"][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Eligible {
                    exact_session: None
                }
            );
        }
        assert_eq!(
            inspect_cli(&[command(Some(&["codex", "resume", "--", id]))]),
            CodexCliState::Eligible {
                exact_session: Some(id.into())
            }
        );
        for args in [
            &["codex", "--local-provider", "synthetic", "resume", id][..],
            &["codex", "resume", "--local-provider", "synthetic", id][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Eligible {
                    exact_session: Some(id.into())
                }
            );
        }
        for args in [
            &["codex", "help"][..],
            &["codex", "-h"][..],
            &["codex", "--help"][..],
            &["codex", "-V"][..],
            &["codex", "--version"][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Ineligible
            );
        }
    }

    #[test]
    fn finds_modes_and_exact_hints_around_options_and_excludes_remote_or_known_subcommands() {
        let id = "10000000-0000-4000-8000-000000000001";
        for args in [
            &["codex", "--model", "synthetic", "resume", id][..],
            &["codex", "resume", "--model", "synthetic", id][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Eligible {
                    exact_session: Some(id.into())
                }
            );
        }
        assert_eq!(
            inspect_cli(&[command(Some(&[
                "codex",
                "fork",
                "--model",
                "synthetic",
                id,
            ]))]),
            CodexCliState::Eligible {
                exact_session: None
            }
        );
        for args in [
            &["codex", "--remote", "synthetic:1234"][..],
            &["codex", "resume", "--remote=synthetic:1234"][..],
            &["codex", "fork", "--remote", "synthetic:1234"][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Ineligible
            );
        }
        for subcommand in [
            "agents",
            "remote-control",
            "app",
            "queue",
            "archive",
            "unarchive",
            "migrate-rollouts",
            "exec-server",
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(&["codex", subcommand]))]),
                CodexCliState::Ineligible
            );
        }
    }

    #[test]
    fn classifies_interactive_exact_and_excluded_codex_commands() {
        let id = "10000000-0000-4000-8000-000000000001";
        assert_eq!(
            inspect_cli(&[command(Some(&["codex"]))]),
            CodexCliState::Eligible {
                exact_session: None
            }
        );
        assert_eq!(
            inspect_cli(&[command(Some(&["codex", "resume", id]))]),
            CodexCliState::Eligible {
                exact_session: Some(id.into())
            }
        );
        for args in [
            &["codex", "resume", "thread-name"][..],
            &["codex", "resume", "--last"][..],
            &["codex", "fork", id][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Eligible {
                    exact_session: None
                }
            );
        }
        for args in [
            &["codex", "exec"][..],
            &["codex", "review"][..],
            &["codex", "mcp"][..],
            &["codex", "app-server"][..],
            &["codex", "--ephemeral"][..],
        ] {
            assert_eq!(
                inspect_cli(&[command(Some(args))]),
                CodexCliState::Ineligible
            );
        }
        assert_eq!(inspect_cli(&[command(None)]), CodexCliState::Ineligible);
        assert_eq!(
            inspect_cli(&[command(Some(&["codex"])), command(None)]),
            CodexCliState::Ineligible
        );
    }
}
