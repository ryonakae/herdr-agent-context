pub mod resolver;
pub mod session;

use crate::backend::{BackendOutcome, Binding, BindingEvidence, CODEX_AGENT, PaneInput, PaneKey};
use crate::config::{Config, resolve_codex_session_roots};
use resolver::{CodexCandidate, CodexCliState, CodexResolveError, CodexScanner, inspect_cli};
use session::parse_session;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct CodexBackend {
    scanner: CodexScanner,
    bindings: HashMap<PaneKey, Binding>,
    baseline: HashMap<PathBuf, CandidateFingerprint>,
    fallback_observations: HashMap<PaneKey, PathBuf>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct CandidateFingerprint {
    size: u64,
    modified_at: SystemTime,
}

impl From<&CodexCandidate> for CandidateFingerprint {
    fn from(candidate: &CodexCandidate) -> Self {
        Self {
            size: candidate.size,
            modified_at: candidate.modified_at,
        }
    }
}

impl CodexBackend {
    pub(crate) fn binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.bindings.get(key)
    }

    pub(crate) fn authoritative_binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.bindings
            .get(key)
            .filter(|binding| binding.is_official())
    }

    pub fn reconcile(
        &mut self,
        config: &Config,
        home: &Path,
        env: &HashMap<String, String>,
        panes: &[PaneInput],
    ) -> HashMap<PaneKey, BackendOutcome> {
        let roots = resolve_codex_session_roots(env, home, &config.codex_session_dirs);
        let codex_panes: Vec<_> = panes
            .iter()
            .filter(|pane| pane.agent.eq_ignore_ascii_case(CODEX_AGENT))
            .collect();
        let live_keys: HashSet<_> = codex_panes.iter().map(|pane| pane.key.clone()).collect();
        self.bindings.retain(|key, _| live_keys.contains(key));

        let mut outcomes = HashMap::new();
        let mut candidates_by_pane = HashMap::new();
        let mut ordinary = Vec::new();
        let mut cwd_counts: HashMap<PathBuf, usize> = HashMap::new();
        let mut observed = HashMap::new();
        let mut next_fallback_observations = HashMap::new();

        for pane in codex_panes {
            let mut cli_state = inspect_cli(&pane.processes);
            if matches!(cli_state, CodexCliState::Invalid)
                && pane
                    .authoritative_session
                    .as_ref()
                    .is_some_and(|reference| reference.agent.eq_ignore_ascii_case(CODEX_AGENT))
            {
                cli_state = CodexCliState::Eligible {
                    exact_session: None,
                };
            }
            match cli_state {
                CodexCliState::Ineligible => {
                    self.bindings.remove(&pane.key);
                    outcomes.insert(pane.key.clone(), BackendOutcome::Unbound);
                    continue;
                }
                CodexCliState::Invalid => {
                    self.bindings.remove(&pane.key);
                    outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
                    continue;
                }
                CodexCliState::Eligible { exact_session } => {
                    let cwd = canonical_or_original(&pane.cwd);
                    *cwd_counts.entry(cwd).or_default() += 1;
                    if let Some(reference) = pane
                        .authoritative_session
                        .as_ref()
                        .filter(|reference| reference.agent.eq_ignore_ascii_case(CODEX_AGENT))
                    {
                        self.bindings.remove(&pane.key);
                        if reference.kind != "id" {
                            outcomes.insert(
                                pane.key.clone(),
                                BackendOutcome::FailedIdentity {
                                    agent: CODEX_AGENT,
                                    session_identity: reference.value.clone(),
                                },
                            );
                            continue;
                        }
                        match self
                            .scanner
                            .resolve_exact(&roots, &pane.cwd, &reference.value)
                        {
                            Ok(Some(candidate)) => {
                                let binding = Binding {
                                    path: candidate.path.clone(),
                                    evidence: BindingEvidence::Official {
                                        source: reference.source.clone(),
                                    },
                                };
                                observed.insert(
                                    candidate.path.clone(),
                                    CandidateFingerprint::from(&candidate),
                                );
                                candidates_by_pane.insert(pane.key.clone(), candidate);
                                self.bindings.insert(pane.key.clone(), binding);
                            }
                            Ok(None) | Err(_) => {
                                outcomes.insert(
                                    pane.key.clone(),
                                    BackendOutcome::FailedIdentity {
                                        agent: CODEX_AGENT,
                                        session_identity: reference.value.clone(),
                                    },
                                );
                            }
                        }
                        continue;
                    }
                    if let Some(identity) = exact_session {
                        self.bindings.remove(&pane.key);
                        match self.scanner.resolve_exact(&roots, &pane.cwd, &identity) {
                            Ok(Some(candidate)) => {
                                let binding = Binding {
                                    path: candidate.path.clone(),
                                    evidence: BindingEvidence::ExactIdentityHint,
                                };
                                observed.insert(
                                    candidate.path.clone(),
                                    CandidateFingerprint::from(&candidate),
                                );
                                candidates_by_pane.insert(pane.key.clone(), candidate);
                                self.bindings.insert(pane.key.clone(), binding);
                            }
                            Ok(None) | Err(_) => {
                                outcomes.insert(
                                    pane.key.clone(),
                                    BackendOutcome::FailedIdentity {
                                        agent: CODEX_AGENT,
                                        session_identity: identity,
                                    },
                                );
                            }
                        }
                        continue;
                    }
                    ordinary.push(pane);
                }
            }
        }

        let mut ordinary_by_cwd: HashMap<PathBuf, Vec<&PaneInput>> = HashMap::new();
        for pane in ordinary {
            let cwd = canonical_or_original(&pane.cwd);
            if let Some(binding) = self.bindings.get(&pane.key).cloned() {
                match self.scanner.validate_path(&roots, &pane.cwd, &binding.path) {
                    Ok(Some(candidate)) => {
                        observed.insert(
                            candidate.path.clone(),
                            CandidateFingerprint::from(&candidate),
                        );
                        candidates_by_pane.insert(pane.key.clone(), candidate);
                        self.bindings.insert(
                            pane.key.clone(),
                            Binding {
                                path: binding.path,
                                evidence: BindingEvidence::LocalFallback,
                            },
                        );
                    }
                    Err(CodexResolveError::Retryable) => {
                        outcomes.insert(
                            pane.key.clone(),
                            BackendOutcome::FailedBinding {
                                agent: CODEX_AGENT,
                                binding,
                                session_identity: None,
                            },
                        );
                        continue;
                    }
                    Ok(None) | Err(_) => {
                        self.bindings.remove(&pane.key);
                    }
                }
            }
            ordinary_by_cwd.entry(cwd).or_default().push(pane);
        }

        for (cwd, cwd_panes) in ordinary_by_cwd {
            let candidates = match self.scanner.scan_ordinary(&roots, &cwd, SystemTime::now()) {
                Ok(candidates) => candidates,
                Err(_) => {
                    for pane in cwd_panes {
                        outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
                    }
                    continue;
                }
            };
            for candidate in &candidates {
                observed.insert(
                    candidate.path.clone(),
                    CandidateFingerprint::from(candidate),
                );
            }
            if cwd_counts.get(&cwd).copied().unwrap_or_default() != 1 {
                continue;
            }
            let pane = cwd_panes[0];
            let first_observation = self.fallback_observations.get(&pane.key) != Some(&cwd);
            next_fallback_observations.insert(pane.key.clone(), cwd.clone());
            if first_observation {
                continue;
            }
            if candidates_by_pane.contains_key(&pane.key) {
                continue;
            }
            let changed: Vec<_> = candidates
                .iter()
                .filter(|candidate| {
                    self.baseline.get(&candidate.path)
                        != Some(&CandidateFingerprint::from(*candidate))
                })
                .cloned()
                .collect();
            if changed.len() == 1 {
                let candidate = changed.into_iter().next().unwrap();
                self.bindings.insert(
                    pane.key.clone(),
                    Binding {
                        path: candidate.path.clone(),
                        evidence: BindingEvidence::LocalFallback,
                    },
                );
                candidates_by_pane.insert(pane.key.clone(), candidate);
            }
        }

        for pane in panes
            .iter()
            .filter(|pane| pane.agent.eq_ignore_ascii_case(CODEX_AGENT))
        {
            if outcomes.contains_key(&pane.key) {
                continue;
            }
            let Some(binding) = self.bindings.get(&pane.key).cloned() else {
                outcomes.insert(pane.key.clone(), BackendOutcome::Unbound);
                continue;
            };
            let Some(candidate) = candidates_by_pane.get(&pane.key) else {
                outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
                continue;
            };
            let index = candidate
                .root
                .parent()
                .map(|parent| parent.join("session_index.jsonl"));
            let outcome = match parse_session(&binding.path, index.as_deref()) {
                Ok(view)
                    if view.header.session_identity == candidate.session_identity
                        && canonical_or_original(&view.header.cwd)
                            == canonical_or_original(&pane.cwd) =>
                {
                    BackendOutcome::Resolved {
                        agent: CODEX_AGENT,
                        binding,
                        view: view.display,
                    }
                }
                _ => BackendOutcome::FailedIdentity {
                    agent: CODEX_AGENT,
                    session_identity: candidate.session_identity.clone(),
                },
            };
            outcomes.insert(pane.key.clone(), outcome);
        }
        self.baseline = observed;
        self.fallback_observations = next_fallback_observations;
        outcomes
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        BackendOutcome, BindingEvidence, PaneInput, PaneKey, ProcessCommand, SessionReference,
    };
    use crate::config::Config;
    use std::collections::HashMap;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    const FIRST_ID: &str = "10000000-0000-4000-8000-000000000001";
    const SECOND_ID: &str = "10000000-0000-4000-8000-000000000002";

    fn pane(id: &str) -> PaneInput {
        PaneInput {
            key: PaneKey {
                pane_id: id.into(),
                terminal_id: format!("term-{id}"),
            },
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
            agent: "codex".into(),
            cwd: PathBuf::from("/synthetic/project"),
            terminal_title: None,
            authoritative_session: None,
            processes: vec![ProcessCommand {
                pid: 1,
                name: "codex".into(),
                argv: Some(vec!["codex".into()]),
                argv0: Some("codex".into()),
                cmdline: None,
            }],
        }
    }

    fn write_rollout(root: &Path, id: &str, name: &str) -> PathBuf {
        let day = root.join("2026/08/28");
        fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-2026-08-28T00-00-00-{id}.jsonl"));
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"{name}\"}}}}\n"
            ),
        )
        .unwrap();
        path
    }

    fn establish_fallback(config: &Config, input: &PaneInput, path: &Path) -> CodexBackend {
        let mut backend = CodexBackend::default();
        backend.reconcile(
            config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(input),
        );
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{\"type\":\"future_unrelated\"}\n")
            .unwrap();
        assert!(matches!(
            backend
                .reconcile(
                    config,
                    Path::new("/no-home"),
                    &HashMap::new(),
                    std::slice::from_ref(input),
                )
                .get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
        backend
    }

    #[test]
    fn sticky_binding_invalidates_on_path_identity_cwd_source_or_terminal_change() {
        for mode in ["missing", "identity", "cwd", "source", "terminal"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("sessions");
            let path = write_rollout(&root, FIRST_ID, "Observed");
            let config = Config {
                codex_session_dirs: vec![root],
                ..Config::default()
            };
            let input = pane("p1");
            let mut backend = establish_fallback(&config, &input, &path);
            let mut current = input.clone();
            match mode {
                "missing" => fs::remove_file(&path).unwrap(),
                "identity" => fs::write(
                    &path,
                    format!(
                        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SECOND_ID}\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}}}\n"
                    ),
                )
                .unwrap(),
                "cwd" => fs::write(
                    &path,
                    format!(
                        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{FIRST_ID}\",\"cwd\":\"/synthetic/other\",\"source\":\"cli\"}}}}\n"
                    ),
                )
                .unwrap(),
                "source" => fs::write(
                    &path,
                    format!(
                        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{FIRST_ID}\",\"cwd\":\"/synthetic/project\",\"source\":\"subagent\"}}}}\n"
                    ),
                )
                .unwrap(),
                "terminal" => current.key.terminal_id = "replacement".into(),
                _ => unreachable!(),
            }

            let outcomes = backend.reconcile(
                &config,
                Path::new("/no-home"),
                &HashMap::new(),
                std::slice::from_ref(&current),
            );
            assert_eq!(outcomes.get(&current.key), Some(&BackendOutcome::Unbound));
            assert!(backend.binding(&current.key).is_none());
        }
    }

    #[test]
    fn retryable_sticky_parse_failure_keeps_binding_and_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let path = write_rollout(&root, FIRST_ID, "Observed");
        let config = Config {
            codex_session_dirs: vec![root.clone()],
            ..Config::default()
        };
        let input = pane("p1");
        let mut backend = establish_fallback(&config, &input, &path);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{")
            .unwrap();

        let failed = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        assert!(matches!(
            failed.get(&input.key),
            Some(BackendOutcome::FailedBinding { .. })
        ));
        assert!(backend.binding(&input.key).is_some());

        write_rollout(&root, FIRST_ID, "Recovered");
        let recovered = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        assert!(matches!(
            recovered.get(&input.key),
            Some(BackendOutcome::Resolved { .. })
        ));
    }

    #[test]
    fn fallback_requires_post_observation_change_and_then_stays_sticky() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let path = write_rollout(&root, FIRST_ID, "Observed");
        let config = Config {
            codex_session_dirs: vec![root],
            ..Config::default()
        };
        let mut input = pane("p1");
        input.processes[0].argv = Some(vec!["codex".into(), "fork".into(), SECOND_ID.into()]);
        let mut backend = CodexBackend::default();

        let cold = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        assert_eq!(cold.get(&input.key), Some(&BackendOutcome::Unbound));

        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"future_unrelated\"}\n")
            .unwrap();
        let changed = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        let BackendOutcome::Resolved { binding, .. } = &changed[&input.key] else {
            panic!("expected changed rollout binding");
        };
        assert_eq!(binding.evidence, BindingEvidence::LocalFallback);

        write_rollout(&config.codex_session_dirs[0], SECOND_ID, "Newer");
        let sticky = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        let BackendOutcome::Resolved { view, .. } = &sticky[&input.key] else {
            panic!("expected sticky binding");
        };
        assert_eq!(view.session_identity, FIRST_ID);
    }

    #[test]
    fn pane_absence_or_terminal_replacement_starts_a_new_fallback_baseline() {
        for mode in ["absence", "terminal"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("sessions");
            let path = write_rollout(&root, FIRST_ID, "Observed");
            let config = Config {
                codex_session_dirs: vec![root],
                ..Config::default()
            };
            let original = pane("p1");
            let mut backend = CodexBackend::default();
            let cold = backend.reconcile(
                &config,
                Path::new("/no-home"),
                &HashMap::new(),
                std::slice::from_ref(&original),
            );
            assert_eq!(cold.get(&original.key), Some(&BackendOutcome::Unbound));

            let mut current = original.clone();
            if mode == "absence" {
                backend.reconcile(&config, Path::new("/no-home"), &HashMap::new(), &[]);
                current = pane("new-pane");
            } else {
                current.key.terminal_id = "replacement-terminal".into();
            }
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"{\"type\":\"future_unrelated\"}\n")
                .unwrap();

            let first_new_generation = backend.reconcile(
                &config,
                Path::new("/no-home"),
                &HashMap::new(),
                std::slice::from_ref(&current),
            );
            assert_eq!(
                first_new_generation.get(&current.key),
                Some(&BackendOutcome::Unbound)
            );
        }
    }

    #[test]
    fn ambiguous_fallback_and_same_cwd_panes_remain_unbound() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rollout(&root, FIRST_ID, "First");
        let config = Config {
            codex_session_dirs: vec![root.clone()],
            ..Config::default()
        };
        let one = pane("p1");
        let mut backend = CodexBackend::default();
        backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&one),
        );
        write_rollout(&root, SECOND_ID, "Second");
        write_rollout(&root, "10000000-0000-4000-8000-000000000003", "Third");
        let ambiguous = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&one),
        );
        assert_eq!(ambiguous.get(&one.key), Some(&BackendOutcome::Unbound));

        let mut other = pane("p2");
        other.cwd = one.cwd.clone();
        let multi = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            &[one.clone(), other.clone()],
        );
        assert_eq!(multi.get(&one.key), Some(&BackendOutcome::Unbound));
        assert_eq!(multi.get(&other.key), Some(&BackendOutcome::Unbound));
    }

    #[test]
    fn missing_argv_or_excluded_process_invalidates_a_sticky_binding() {
        for mode in ["missing", "excluded"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("sessions");
            let path = write_rollout(&root, FIRST_ID, "Observed");
            let config = Config {
                codex_session_dirs: vec![root],
                ..Config::default()
            };
            let mut input = pane("p1");
            let mut backend = establish_fallback(&config, &input, &path);

            input.processes[0].argv = if mode == "missing" {
                None
            } else {
                Some(vec!["codex".into(), "exec".into()])
            };
            let outcomes = backend.reconcile(
                &config,
                Path::new("/no-home"),
                &HashMap::new(),
                std::slice::from_ref(&input),
            );
            assert_eq!(outcomes.get(&input.key), Some(&BackendOutcome::Unbound));
            assert!(backend.binding(&input.key).is_none());
        }
    }

    #[test]
    fn broken_official_identity_is_pane_local_and_foreign_reference_is_not_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rollout(&root, FIRST_ID, "Healthy");
        let config = Config {
            codex_session_dirs: vec![root],
            ..Config::default()
        };
        let mut broken = pane("broken");
        broken.authoritative_session = Some(SessionReference {
            source: "integration".into(),
            agent: "codex".into(),
            kind: "id".into(),
            value: SECOND_ID.into(),
        });
        let mut healthy = pane("healthy");
        healthy.processes[0].argv = Some(vec!["codex".into(), "resume".into(), FIRST_ID.into()]);
        let mut foreign = pane("foreign");
        foreign.authoritative_session = Some(SessionReference {
            source: "other".into(),
            agent: "pi".into(),
            kind: "path".into(),
            value: "/synthetic/foreign".into(),
        });

        let outcomes = CodexBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            &[broken.clone(), healthy.clone(), foreign.clone()],
        );
        assert!(matches!(
            &outcomes[&broken.key],
            BackendOutcome::FailedIdentity { session_identity, .. } if session_identity == SECOND_ID
        ));
        assert!(matches!(
            outcomes.get(&healthy.key),
            Some(BackendOutcome::Resolved { .. })
        ));
        assert_eq!(outcomes.get(&foreign.key), Some(&BackendOutcome::Unbound));
    }

    #[test]
    fn official_identity_precedes_candidates_and_exact_resume_is_visual_only() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rollout(&root, FIRST_ID, "Official");
        write_rollout(&root, SECOND_ID, "Exact");
        let config = Config {
            codex_session_dirs: vec![root],
            ..Config::default()
        };
        let mut official = pane("official");
        official.authoritative_session = Some(SessionReference {
            source: "integration".into(),
            agent: "codex".into(),
            kind: "id".into(),
            value: FIRST_ID.into(),
        });
        official.processes[0].argv = Some(vec!["codex".into(), "resume".into(), FIRST_ID.into()]);
        official.processes.push(ProcessCommand {
            pid: 2,
            name: "codex".into(),
            argv: Some(vec!["codex".into(), "resume".into(), SECOND_ID.into()]),
            argv0: Some("codex".into()),
            cmdline: None,
        });
        let mut exact = pane("exact");
        exact.processes[0].argv = Some(vec!["codex".into(), "resume".into(), SECOND_ID.into()]);

        let outcomes = CodexBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            &[official.clone(), exact.clone()],
        );

        let BackendOutcome::Resolved { binding, view, .. } = &outcomes[&official.key] else {
            panic!("expected official resolution");
        };
        assert_eq!(view.session_identity, FIRST_ID);
        assert_eq!(binding.applies_to_source(), Some("integration"));
        let BackendOutcome::Resolved { binding, view, .. } = &outcomes[&exact.key] else {
            panic!("expected exact resolution");
        };
        assert_eq!(view.session_identity, SECOND_ID);
        assert_eq!(binding.evidence, BindingEvidence::ExactIdentityHint);
        assert_eq!(binding.applies_to_source(), None);
    }
}
