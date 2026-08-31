pub mod resolver;
pub mod session;

use crate::backend::{
    BackendOutcome, Binding, BindingEvidence, OPENCODE_AGENT, PaneInput, PaneKey,
};
use crate::config::{Config, resolve_opencode_database_paths};
use resolver::{
    OpenCodeCandidate, OpenCodeCliState, OpenCodeResolveError, OpenCodeScanner,
    canonical_or_normalized, inspect_cli,
};
use session::SessionFingerprint;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct StickyBinding {
    binding: Binding,
    session_identity: String,
    pid: u32,
    cwd: PathBuf,
}

#[derive(Clone)]
struct PaneObservation {
    pid: u32,
    cwd: PathBuf,
    fork: bool,
    databases: HashMap<PathBuf, HashMap<String, SessionFingerprint>>,
}

#[derive(Default)]
pub struct OpenCodeBackend {
    scanner: OpenCodeScanner,
    sticky: HashMap<PaneKey, StickyBinding>,
    observations: HashMap<PaneKey, PaneObservation>,
}

impl OpenCodeBackend {
    pub(crate) fn binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.sticky.get(key).map(|sticky| &sticky.binding)
    }

    pub(crate) fn authoritative_binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.binding(key).filter(|binding| binding.is_official())
    }

    pub fn reconcile(
        &mut self,
        config: &Config,
        home: &Path,
        env: &HashMap<String, String>,
        panes: &[PaneInput],
    ) -> HashMap<PaneKey, BackendOutcome> {
        let databases = resolve_opencode_database_paths(env, home, &config.opencode_database_paths);
        let opencode_panes: Vec<_> = panes
            .iter()
            .filter(|pane| pane.agent.eq_ignore_ascii_case(OPENCODE_AGENT))
            .collect();
        let live_keys: HashSet<_> = opencode_panes.iter().map(|pane| pane.key.clone()).collect();
        self.sticky.retain(|key, _| live_keys.contains(key));
        self.observations.retain(|key, _| live_keys.contains(key));

        let mut outcomes = HashMap::new();
        let mut ordinary = Vec::new();
        let mut cwd_counts: HashMap<PathBuf, usize> = HashMap::new();

        for pane in opencode_panes {
            let cli = inspect_cli(&pane.processes);
            let OpenCodeCliState::Eligible {
                pid,
                exact_session,
                fork,
            } = cli
            else {
                self.retire(&pane.key);
                outcomes.insert(
                    pane.key.clone(),
                    if matches!(cli, OpenCodeCliState::Invalid) {
                        BackendOutcome::Failed
                    } else {
                        BackendOutcome::Unbound
                    },
                );
                continue;
            };
            let cwd = canonical_or_normalized(&pane.cwd);
            *cwd_counts.entry(cwd.clone()).or_default() += 1;

            let same_generation = self
                .observations
                .get(&pane.key)
                .is_some_and(|observation| observation.pid == pid && observation.cwd == cwd);
            let entered_fork = same_generation
                && fork
                && self
                    .observations
                    .get(&pane.key)
                    .is_some_and(|observation| !observation.fork);
            if !same_generation || entered_fork {
                self.retire(&pane.key);
            }

            if let Some(reference) = pane
                .authoritative_session
                .as_ref()
                .filter(|reference| reference.agent.eq_ignore_ascii_case(OPENCODE_AGENT))
            {
                self.sticky.remove(&pane.key);
                if reference.kind != "id" {
                    outcomes.insert(
                        pane.key.clone(),
                        BackendOutcome::FailedIdentity {
                            agent: OPENCODE_AGENT,
                            session_identity: reference.value.clone(),
                        },
                    );
                    self.note_generation(&pane.key, pid, cwd, fork);
                    continue;
                }
                match self
                    .scanner
                    .resolve_exact(&databases, &pane.cwd, &reference.value)
                {
                    Ok(Some(candidate)) => {
                        let binding = Binding {
                            path: candidate.database_path.clone(),
                            evidence: BindingEvidence::Official {
                                source: reference.source.clone(),
                            },
                        };
                        self.bind_candidate(pane, pid, cwd, fork, binding.clone(), &candidate);
                        outcomes.insert(
                            pane.key.clone(),
                            BackendOutcome::Resolved {
                                agent: OPENCODE_AGENT,
                                binding,
                                view: candidate.session.display,
                            },
                        );
                    }
                    Ok(None) | Err(_) => {
                        self.note_generation(&pane.key, pid, cwd, fork);
                        outcomes.insert(
                            pane.key.clone(),
                            BackendOutcome::FailedIdentity {
                                agent: OPENCODE_AGENT,
                                session_identity: reference.value.clone(),
                            },
                        );
                    }
                }
                continue;
            }

            if let Some(identity) = exact_session {
                self.sticky.remove(&pane.key);
                match self.scanner.resolve_exact(&databases, &pane.cwd, &identity) {
                    Ok(Some(candidate)) => {
                        let binding = Binding {
                            path: candidate.database_path.clone(),
                            evidence: BindingEvidence::ExactIdentityHint,
                        };
                        self.bind_candidate(pane, pid, cwd, fork, binding.clone(), &candidate);
                        outcomes.insert(
                            pane.key.clone(),
                            BackendOutcome::Resolved {
                                agent: OPENCODE_AGENT,
                                binding,
                                view: candidate.session.display,
                            },
                        );
                    }
                    Ok(None) | Err(_) => {
                        self.note_generation(&pane.key, pid, cwd, fork);
                        outcomes.insert(
                            pane.key.clone(),
                            BackendOutcome::FailedIdentity {
                                agent: OPENCODE_AGENT,
                                session_identity: identity,
                            },
                        );
                    }
                }
                continue;
            }
            ordinary.push((pane, pid, cwd, fork));
        }

        for (pane, pid, cwd, fork) in ordinary {
            if let Some(sticky) = self.sticky.get(&pane.key).cloned() {
                debug_assert_eq!(sticky.pid, pid);
                debug_assert_eq!(sticky.cwd, cwd);
                if std::fs::symlink_metadata(&sticky.binding.path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    self.retire(&pane.key);
                } else {
                    match self.scanner.resolve_exact(
                        &databases,
                        &pane.cwd,
                        &sticky.session_identity,
                    ) {
                        Ok(Some(candidate)) if candidate.database_path == sticky.binding.path => {
                            let binding = Binding {
                                path: candidate.database_path.clone(),
                                evidence: BindingEvidence::LocalFallback,
                            };
                            self.bind_candidate(
                                pane,
                                pid,
                                cwd.clone(),
                                fork,
                                binding.clone(),
                                &candidate,
                            );
                            outcomes.insert(
                                pane.key.clone(),
                                BackendOutcome::Resolved {
                                    agent: OPENCODE_AGENT,
                                    binding,
                                    view: candidate.session.display,
                                },
                            );
                            continue;
                        }
                        Err(OpenCodeResolveError::Read) => {
                            outcomes.insert(
                                pane.key.clone(),
                                BackendOutcome::FailedBinding {
                                    agent: OPENCODE_AGENT,
                                    binding: sticky.binding,
                                    session_identity: Some(sticky.session_identity),
                                },
                            );
                            continue;
                        }
                        Ok(Some(_)) | Ok(None) | Err(_) => self.retire(&pane.key),
                    }
                }
            }

            let previous = self.observations.get(&pane.key).cloned();
            let mut next_databases = previous
                .as_ref()
                .map(|observation| observation.databases.clone())
                .unwrap_or_default();
            let all_had_baseline = databases
                .iter()
                .all(|path| next_databases.contains_key(&canonical_or_normalized(path)));
            let mut all_candidates = Vec::new();
            let mut changed = Vec::new();
            let mut failed = false;
            let scan_now = unix_time_millis();

            for database in &databases {
                let database_key = canonical_or_normalized(database);
                match self.scanner.scan_database(database, &pane.cwd, scan_now) {
                    Ok(candidates) => {
                        let current: HashMap<_, _> = candidates
                            .iter()
                            .map(|candidate| {
                                (
                                    candidate.session.display.session_identity.clone(),
                                    candidate.session.fingerprint.clone(),
                                )
                            })
                            .collect();
                        if let Some(baseline) = next_databases.get(&database_key) {
                            changed.extend(
                                candidates
                                    .iter()
                                    .filter(|candidate| {
                                        baseline.get(&candidate.session.display.session_identity)
                                            != Some(&candidate.session.fingerprint)
                                    })
                                    .cloned(),
                            );
                        }
                        next_databases.insert(database_key, current);
                        all_candidates.extend(candidates);
                    }
                    Err(_) => failed = true,
                }
            }
            self.observations.insert(
                pane.key.clone(),
                PaneObservation {
                    pid,
                    cwd: cwd.clone(),
                    fork,
                    databases: next_databases,
                },
            );

            if failed {
                outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
                continue;
            }
            let duplicate_identity = {
                let mut identities = HashSet::new();
                all_candidates.iter().any(|candidate| {
                    !identities.insert(candidate.session.display.session_identity.as_str())
                })
            };
            if !all_had_baseline
                || cwd_counts.get(&cwd).copied().unwrap_or_default() != 1
                || duplicate_identity
                || changed.len() != 1
            {
                outcomes.insert(pane.key.clone(), BackendOutcome::Unbound);
                continue;
            }
            let candidate = changed.pop().unwrap();
            let binding = Binding {
                path: candidate.database_path.clone(),
                evidence: BindingEvidence::LocalFallback,
            };
            self.bind_candidate(pane, pid, cwd, fork, binding.clone(), &candidate);
            outcomes.insert(
                pane.key.clone(),
                BackendOutcome::Resolved {
                    agent: OPENCODE_AGENT,
                    binding,
                    view: candidate.session.display,
                },
            );
        }
        outcomes
    }

    fn note_generation(&mut self, key: &PaneKey, pid: u32, cwd: PathBuf, fork: bool) {
        self.observations
            .entry(key.clone())
            .and_modify(|observation| {
                observation.pid = pid;
                observation.cwd = cwd.clone();
                observation.fork = fork;
            })
            .or_insert(PaneObservation {
                pid,
                cwd,
                fork,
                databases: HashMap::new(),
            });
    }

    fn bind_candidate(
        &mut self,
        pane: &PaneInput,
        pid: u32,
        cwd: PathBuf,
        fork: bool,
        binding: Binding,
        candidate: &OpenCodeCandidate,
    ) {
        self.sticky.insert(
            pane.key.clone(),
            StickyBinding {
                binding,
                session_identity: candidate.session.display.session_identity.clone(),
                pid,
                cwd: cwd.clone(),
            },
        );
        self.note_generation(&pane.key, pid, cwd, fork);
    }

    fn retire(&mut self, key: &PaneKey) {
        self.sticky.remove(key);
        self.observations.remove(key);
    }
}

fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ProcessCommand, SessionReference};
    use rusqlite::{Connection, params};

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
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, ?4, ?4, NULL)",
                params![id, cwd.to_str().unwrap(), format!("Title {id}"), updated],
            )
            .unwrap();
    }

    fn update_session(connection: &Connection, id: &str, updated: i64) {
        connection
            .execute(
                "UPDATE session SET title = title || ' changed', time_updated = ?1 WHERE id = ?2",
                params![updated, id],
            )
            .unwrap();
    }

    fn pane(id: &str, cwd: &Path) -> PaneInput {
        PaneInput {
            key: PaneKey {
                pane_id: id.into(),
                terminal_id: format!("terminal-{id}"),
            },
            workspace_id: None,
            tab_id: None,
            agent: OPENCODE_AGENT.into(),
            cwd: cwd.to_owned(),
            terminal_title: None,
            authoritative_session: None,
            processes: vec![ProcessCommand {
                pid: 100,
                name: "opencode".into(),
                argv: Some(vec!["opencode".into()]),
                argv0: Some("opencode".into()),
                cmdline: None,
            }],
        }
    }

    fn config(database_paths: Vec<PathBuf>) -> Config {
        Config {
            opencode_database_paths: database_paths,
            ..Config::default()
        }
    }

    fn reconcile(
        backend: &mut OpenCodeBackend,
        config: &Config,
        panes: &[PaneInput],
    ) -> HashMap<PaneKey, BackendOutcome> {
        let env = HashMap::from([(
            "OPENCODE_DB".into(),
            config.opencode_database_paths[0]
                .to_string_lossy()
                .into_owned(),
        )]);
        let additional = Config {
            opencode_database_paths: config.opencode_database_paths[1..].to_vec(),
            ..config.clone()
        };
        backend.reconcile(&additional, Path::new("/no-home"), &env, panes)
    }

    #[test]
    fn official_and_nonfork_cli_exact_bind_with_their_distinct_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_official", &cwd, unix_time_millis());
        insert_session(&connection, "ses_exact", &cwd, unix_time_millis());
        let config = config(vec![database]);
        let mut official = pane("official", &cwd);
        official.authoritative_session = Some(SessionReference {
            source: "herdr:opencode".into(),
            agent: OPENCODE_AGENT.into(),
            kind: "id".into(),
            value: "ses_official".into(),
        });
        official.processes[0].argv = Some(vec![
            "opencode".into(),
            "--session".into(),
            "ses_exact".into(),
        ]);
        let mut exact = pane("exact", &cwd);
        exact.processes[0].argv = official.processes[0].argv.clone();

        let outcomes = reconcile(
            &mut OpenCodeBackend::default(),
            &config,
            &[official.clone(), exact.clone()],
        );
        let BackendOutcome::Resolved { binding, view, .. } = &outcomes[&official.key] else {
            panic!("official did not resolve");
        };
        assert_eq!(view.session_identity, "ses_official");
        assert_eq!(binding.applies_to_source(), Some("herdr:opencode"));
        let BackendOutcome::Resolved { binding, view, .. } = &outcomes[&exact.key] else {
            panic!("exact did not resolve");
        };
        assert_eq!(view.session_identity, "ses_exact");
        assert_eq!(binding.evidence, BindingEvidence::ExactIdentityHint);
        assert_eq!(binding.applies_to_source(), None);
    }

    #[test]
    fn every_authoritative_error_blocks_sticky_and_fallback() {
        for mode in ["missing", "malformed", "wrong-cwd", "nonroot", "duplicate"] {
            let temp = tempfile::tempdir().unwrap();
            let cwd = temp.path().join("project");
            let other = temp.path().join("other");
            std::fs::create_dir(&cwd).unwrap();
            std::fs::create_dir(&other).unwrap();
            let first = temp.path().join("first.db");
            let second = temp.path().join("second.db");
            let first_connection = create_database(&first);
            let second_connection = create_database(&second);
            insert_session(&first_connection, "ses_fallback", &cwd, unix_time_millis());
            let identity = if mode == "malformed" {
                "bad id"
            } else {
                "ses_target"
            };
            match mode {
                "wrong-cwd" => {
                    insert_session(&first_connection, identity, &other, unix_time_millis())
                }
                "nonroot" => {
                    first_connection.execute(
                        "INSERT INTO session VALUES (?1, 'ses_parent', ?2, 'Child', 1, ?3, NULL)",
                        params![identity, cwd.to_str().unwrap(), unix_time_millis()],
                    ).unwrap();
                }
                "duplicate" => {
                    insert_session(&first_connection, identity, &cwd, unix_time_millis());
                    insert_session(&second_connection, identity, &cwd, unix_time_millis());
                }
                _ => {}
            }
            let config = config(vec![first, second]);
            let mut input = pane("p1", &cwd);
            input.authoritative_session = Some(SessionReference {
                source: "herdr:opencode".into(),
                agent: OPENCODE_AGENT.into(),
                kind: "id".into(),
                value: identity.into(),
            });
            let outcomes = reconcile(
                &mut OpenCodeBackend::default(),
                &config,
                std::slice::from_ref(&input),
            );
            assert!(
                matches!(
                    outcomes.get(&input.key),
                    Some(BackendOutcome::FailedIdentity { .. })
                ),
                "{mode}"
            );
        }
    }

    #[test]
    fn exact_cli_errors_are_authoritative_but_foreign_references_are_not() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        insert_session(&connection, "ses_present", &cwd, unix_time_millis());
        let config = config(vec![database]);
        let mut missing = pane("missing", &cwd);
        missing.processes[0].argv = Some(vec![
            "opencode".into(),
            "--session".into(),
            "ses_missing".into(),
        ]);
        let mut foreign = pane("foreign", &cwd);
        foreign.authoritative_session = Some(SessionReference {
            source: "other".into(),
            agent: "pi".into(),
            kind: "path".into(),
            value: "/ignored".into(),
        });

        let outcomes = reconcile(
            &mut OpenCodeBackend::default(),
            &config,
            &[missing.clone(), foreign.clone()],
        );
        assert!(matches!(
            outcomes.get(&missing.key),
            Some(BackendOutcome::FailedIdentity { .. })
        ));
        assert_eq!(outcomes.get(&foreign.key), Some(&BackendOutcome::Unbound));
    }

    #[test]
    fn ordinary_binding_requires_baseline_then_exactly_one_session_change() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let now = unix_time_millis();
        insert_session(&connection, "ses_target", &cwd, now);
        let config = config(vec![database]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();

        let baseline = reconcile(&mut backend, &config, std::slice::from_ref(&input));
        assert_eq!(baseline.get(&input.key), Some(&BackendOutcome::Unbound));
        let unchanged = reconcile(&mut backend, &config, std::slice::from_ref(&input));
        assert_eq!(unchanged.get(&input.key), Some(&BackendOutcome::Unbound));
        update_session(&connection, "ses_target", now + 1);
        let changed = reconcile(&mut backend, &config, std::slice::from_ref(&input));
        let BackendOutcome::Resolved { binding, view, .. } = &changed[&input.key] else {
            panic!("changed candidate did not bind");
        };
        assert_eq!(binding.evidence, BindingEvidence::LocalFallback);
        assert_eq!(view.session_identity, "ses_target");
        let sticky = reconcile(&mut backend, &config, std::slice::from_ref(&input));
        assert!(matches!(
            sticky.get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
    }

    #[test]
    fn unrelated_session_writes_do_not_change_the_target_fingerprint() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let other = temp.path().join("other");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&other).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let now = unix_time_millis();
        insert_session(&connection, "ses_target", &cwd, now);
        insert_session(&connection, "ses_other", &other, now);
        let config = config(vec![database]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&input));
        update_session(&connection, "ses_other", now + 1);
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Unbound)
        );
    }

    #[test]
    fn multiple_changed_candidates_databases_or_same_cwd_panes_are_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        let first_connection = create_database(&first);
        let second_connection = create_database(&second);
        let now = unix_time_millis();
        insert_session(&first_connection, "ses_one", &cwd, now);
        insert_session(&second_connection, "ses_two", &cwd, now);
        let config = config(vec![first, second]);
        let one = pane("one", &cwd);
        let two = pane("two", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&one));
        update_session(&first_connection, "ses_one", now + 1);
        update_session(&second_connection, "ses_two", now + 1);
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&one)).get(&one.key),
            Some(&BackendOutcome::Unbound)
        );

        update_session(&first_connection, "ses_one", now + 2);
        let outcomes = reconcile(&mut backend, &config, &[one.clone(), two.clone()]);
        assert_eq!(outcomes.get(&one.key), Some(&BackendOutcome::Unbound));
        assert_eq!(outcomes.get(&two.key), Some(&BackendOutcome::Unbound));
    }

    #[test]
    fn duplicate_identity_across_databases_never_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        let first_connection = create_database(&first);
        let second_connection = create_database(&second);
        let now = unix_time_millis();
        insert_session(&first_connection, "ses_duplicate", &cwd, now);
        insert_session(&second_connection, "ses_duplicate", &cwd, now);
        let config = config(vec![first, second]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&input));
        update_session(&first_connection, "ses_duplicate", now + 1);
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Unbound)
        );
    }

    #[test]
    fn fork_never_adopts_parent_and_requires_child_post_observation_change() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let now = unix_time_millis();
        insert_session(&connection, "ses_parent", &cwd, now);
        let config = config(vec![database]);
        let mut input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&input));
        update_session(&connection, "ses_parent", now + 1);
        assert!(matches!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));

        input.processes[0].argv = Some(vec![
            "opencode".into(),
            "--session".into(),
            "ses_parent".into(),
            "--fork".into(),
        ]);
        insert_session(&connection, "ses_child", &cwd, now + 2);
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Unbound)
        );
        update_session(&connection, "ses_child", now + 3);
        let child = reconcile(&mut backend, &config, std::slice::from_ref(&input));
        let BackendOutcome::Resolved { view, .. } = &child[&input.key] else {
            panic!("child did not bind");
        };
        assert_eq!(view.session_identity, "ses_child");
    }

    #[test]
    fn pid_terminal_cwd_process_and_pane_lifecycle_retire_state() {
        for mode in ["pid", "terminal", "cwd", "process", "pane"] {
            let temp = tempfile::tempdir().unwrap();
            let cwd = temp.path().join("project");
            let other = temp.path().join("other");
            std::fs::create_dir(&cwd).unwrap();
            std::fs::create_dir(&other).unwrap();
            let database = temp.path().join("opencode.db");
            let connection = create_database(&database);
            let now = unix_time_millis();
            insert_session(&connection, "ses_target", &cwd, now);
            let config = config(vec![database]);
            let input = pane("p1", &cwd);
            let mut backend = OpenCodeBackend::default();
            reconcile(&mut backend, &config, std::slice::from_ref(&input));
            update_session(&connection, "ses_target", now + 1);
            reconcile(&mut backend, &config, std::slice::from_ref(&input));
            let mut current = input.clone();
            match mode {
                "pid" => current.processes[0].pid += 1,
                "terminal" => current.key.terminal_id = "replacement".into(),
                "cwd" => current.cwd = other,
                "process" => {
                    current.processes[0].argv = Some(vec!["opencode".into(), "run".into()])
                }
                "pane" => {
                    reconcile(&mut backend, &config, &[]);
                    current.key.pane_id = "replacement".into();
                }
                _ => unreachable!(),
            }
            let outcome = reconcile(&mut backend, &config, std::slice::from_ref(&current));
            assert!(
                !matches!(
                    outcome.get(&current.key),
                    Some(BackendOutcome::Resolved { .. })
                ),
                "{mode}"
            );
            assert!(backend.binding(&current.key).is_none(), "{mode}");
        }
    }

    #[test]
    fn failed_database_scan_blocks_partial_fallback_and_preserves_baselines() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        let first_connection = create_database(&first);
        let second_connection = create_database(&second);
        let now = unix_time_millis();
        insert_session(&first_connection, "ses_target", &cwd, now);
        let config = config(vec![first, second.clone()]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&input));
        update_session(&first_connection, "ses_target", now + 1);
        second_connection.execute("DROP TABLE part", []).unwrap();
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Failed)
        );
        second_connection.execute_batch("CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);").unwrap();
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Unbound)
        );
        update_session(&first_connection, "ses_target", now + 2);
        assert!(matches!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
    }

    #[test]
    fn busy_scan_preserves_baseline_and_blocks_fallback_until_a_later_change() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let now = unix_time_millis();
        insert_session(&connection, "ses_target", &cwd, now);
        let config = config(vec![database]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&input));

        connection.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Failed)
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        connection.execute_batch("ROLLBACK;").unwrap();
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Unbound)
        );
        update_session(&connection, "ses_target", now + 1);
        assert!(matches!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
    }

    #[test]
    fn incompatible_bound_session_row_retires_sticky_state() {
        for mode in ["nonroot", "wrong-cwd"] {
            let temp = tempfile::tempdir().unwrap();
            let cwd = temp.path().join("project");
            let other = temp.path().join("other");
            std::fs::create_dir(&cwd).unwrap();
            std::fs::create_dir(&other).unwrap();
            let database = temp.path().join("opencode.db");
            let connection = create_database(&database);
            let now = unix_time_millis();
            insert_session(&connection, "ses_target", &cwd, now);
            let config = config(vec![database]);
            let input = pane("p1", &cwd);
            let mut backend = OpenCodeBackend::default();
            reconcile(&mut backend, &config, std::slice::from_ref(&input));
            update_session(&connection, "ses_target", now + 1);
            reconcile(&mut backend, &config, std::slice::from_ref(&input));

            if mode == "nonroot" {
                connection
                    .execute(
                        "UPDATE session SET parent_id = 'ses_parent' WHERE id = 'ses_target'",
                        [],
                    )
                    .unwrap();
            } else {
                connection
                    .execute(
                        "UPDATE session SET directory = ?1 WHERE id = 'ses_target'",
                        [other.to_str().unwrap()],
                    )
                    .unwrap();
            }
            assert_eq!(
                reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
                Some(&BackendOutcome::Unbound),
                "{mode}"
            );
            assert!(backend.binding(&input.key).is_none(), "{mode}");
        }
    }

    #[test]
    fn disappearing_bound_database_retires_sticky_state() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let database = temp.path().join("opencode.db");
        let connection = create_database(&database);
        let now = unix_time_millis();
        insert_session(&connection, "ses_target", &cwd, now);
        let config = config(vec![database.clone()]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        reconcile(&mut backend, &config, std::slice::from_ref(&input));
        update_session(&connection, "ses_target", now + 1);
        assert!(matches!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
        drop(connection);
        std::fs::remove_file(database).unwrap();

        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Failed)
        );
        assert!(backend.binding(&input.key).is_none());
    }

    #[test]
    fn unobserved_database_recovery_is_baseline_only() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        let first_connection = create_database(&first);
        let now = unix_time_millis();
        insert_session(&first_connection, "ses_target", &cwd, now);
        let config = config(vec![first, second.clone()]);
        let input = pane("p1", &cwd);
        let mut backend = OpenCodeBackend::default();
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Failed)
        );
        update_session(&first_connection, "ses_target", now + 1);
        let _second_connection = create_database(&second);
        assert_eq!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(&BackendOutcome::Unbound)
        );
        update_session(&first_connection, "ses_target", now + 2);
        assert!(matches!(
            reconcile(&mut backend, &config, std::slice::from_ref(&input)).get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
    }
}
