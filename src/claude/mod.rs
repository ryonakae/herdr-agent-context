pub mod resolver;
pub mod session;

use crate::backend::{BackendOutcome, Binding, BindingEvidence, CLAUDE_AGENT, PaneInput, PaneKey};
use crate::config::{Config, resolve_claude_project_roots};
use crate::text::{complete_line, display_line};
use resolver::{ClaudeCandidate, ClaudeCliState, ClaudeResolveError, ClaudeScanner, inspect_cli};
use session::{ClaudeSessionView, parse_session};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct ClaudeBackend {
    scanner: ClaudeScanner,
    bindings: HashMap<PaneKey, Binding>,
    baseline: HashMap<PathBuf, CandidateFingerprint>,
    parsed: HashMap<PathBuf, ParsedCache>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct CandidateFingerprint {
    size: u64,
    modified_at: SystemTime,
}

impl From<&ClaudeCandidate> for CandidateFingerprint {
    fn from(candidate: &ClaudeCandidate) -> Self {
        Self {
            size: candidate.size,
            modified_at: candidate.modified_at,
        }
    }
}

#[derive(Clone)]
struct ParsedCache {
    fingerprint: CandidateFingerprint,
    view: Option<ClaudeSessionView>,
}

impl ClaudeBackend {
    pub fn reconcile(
        &mut self,
        config: &Config,
        home: &Path,
        env: &HashMap<String, String>,
        panes: &[PaneInput],
    ) -> HashMap<PaneKey, BackendOutcome> {
        let roots = resolve_claude_project_roots(env, home, &config.claude_session_dirs);
        let claude_panes: Vec<_> = panes
            .iter()
            .filter(|pane| pane.agent.eq_ignore_ascii_case(CLAUDE_AGENT))
            .collect();
        let live_keys: std::collections::HashSet<_> =
            claude_panes.iter().map(|pane| pane.key.clone()).collect();
        self.bindings.retain(|key, _| live_keys.contains(key));

        let mut outcomes = HashMap::new();
        let mut candidates_by_pane = HashMap::new();
        let mut ordinary = Vec::new();
        let mut cwd_counts: HashMap<PathBuf, usize> = HashMap::new();
        let mut observed = HashMap::new();

        for pane in claude_panes {
            match inspect_cli(&pane.processes) {
                ClaudeCliState::Ineligible => {
                    self.bindings.remove(&pane.key);
                    outcomes.insert(pane.key.clone(), BackendOutcome::Unbound);
                    continue;
                }
                ClaudeCliState::Invalid => {
                    self.bindings.remove(&pane.key);
                    outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
                    continue;
                }
                ClaudeCliState::Eligible { exact_session } => {
                    *cwd_counts
                        .entry(canonical_or_original(&pane.cwd))
                        .or_default() += 1;
                    if let Some(reference) = &pane.authoritative_session {
                        self.bindings.remove(&pane.key);
                        if reference.kind != "id"
                            || !reference.agent.eq_ignore_ascii_case(CLAUDE_AGENT)
                        {
                            outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
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
                                        agent: CLAUDE_AGENT,
                                        session_identity: reference.value.clone(),
                                    },
                                );
                            }
                        }
                        continue;
                    }
                    if let Some(session_identity) = exact_session {
                        self.bindings.remove(&pane.key);
                        match self
                            .scanner
                            .resolve_exact(&roots, &pane.cwd, &session_identity)
                        {
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
                                        agent: CLAUDE_AGENT,
                                        session_identity,
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
                    Err(ClaudeResolveError::Retryable) => {
                        outcomes.insert(pane.key.clone(), BackendOutcome::Failed);
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
            let candidates = self.scanner.scan_ordinary(&roots, &cwd, SystemTime::now());
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
            if let Some(bound) = candidates_by_pane.get(&pane.key).cloned() {
                let bound_fingerprint = CandidateFingerprint::from(&bound);
                let bound_unchanged = self.baseline.get(&bound.path) == Some(&bound_fingerprint);
                if bound_unchanged {
                    let changed: Vec<_> = candidates
                        .iter()
                        .filter(|candidate| candidate.path != bound.path)
                        .filter(|candidate| candidate.modified_at > bound.modified_at)
                        .filter(|candidate| {
                            self.baseline.get(&candidate.path)
                                != Some(&CandidateFingerprint::from(*candidate))
                        })
                        .collect();
                    if changed.len() == 1 {
                        let candidate = changed[0].clone();
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
            } else if let Some(candidate) = candidates.first().cloned() {
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
            .filter(|pane| pane.agent.eq_ignore_ascii_case(CLAUDE_AGENT))
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
            let outcome = match self.load_view(&binding.path) {
                Some(view)
                    if view.header.session_identity == candidate.session_identity
                        && canonical_or_original(&view.header.cwd)
                            == canonical_or_original(&pane.cwd) =>
                {
                    BackendOutcome::Resolved {
                        agent: CLAUDE_AGENT,
                        binding,
                        view: verified_terminal_title(view.display, pane.terminal_title.as_deref()),
                    }
                }
                _ => BackendOutcome::FailedIdentity {
                    agent: CLAUDE_AGENT,
                    session_identity: candidate.session_identity.clone(),
                },
            };
            outcomes.insert(pane.key.clone(), outcome);
        }
        self.baseline = observed;
        outcomes
    }

    fn load_view(&mut self, path: &Path) -> Option<ClaudeSessionView> {
        let metadata = fs::metadata(path).ok()?;
        let fingerprint = CandidateFingerprint {
            size: metadata.len(),
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        };
        fs::File::open(path).ok()?;
        if let Some(cached) = self.parsed.get(path) {
            if cached.fingerprint == fingerprint {
                return cached.view.clone();
            }
        }
        let view = parse_session(path).ok();
        self.parsed.insert(
            path.to_owned(),
            ParsedCache {
                fingerprint,
                view: view.clone(),
            },
        );
        view
    }
}

fn verified_terminal_title(
    mut display: crate::backend::DisplayView,
    terminal_title: Option<&str>,
) -> crate::backend::DisplayView {
    let Some(jsonl_title) = display.tab_name_source.as_deref() else {
        return display;
    };
    let Some(terminal_title) = terminal_title.and_then(complete_line) else {
        return display;
    };
    if terminal_title != jsonl_title {
        return display;
    }

    display.session_name = display_line(&terminal_title);
    display.tab_name_source = Some(terminal_title);
    display
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendOutcome, PaneInput, PaneKey, ProcessCommand};
    use crate::config::Config;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn pane(id: &str) -> PaneInput {
        PaneInput {
            key: PaneKey {
                pane_id: id.into(),
                terminal_id: format!("term-{id}"),
            },
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
            agent: "claude".into(),
            cwd: PathBuf::from("/work/project"),
            terminal_title: None,
            authoritative_session: None,
            processes: vec![ProcessCommand {
                name: "claude".into(),
                argv: Some(vec!["claude".into()]),
                argv0: Some("claude".into()),
                cmdline: None,
            }],
        }
    }

    fn write_session(root: &Path) {
        let project = root.join("-work-project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("10000000-0000-4000-8000-000000000001.jsonl"),
            concat!(
                "{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
                "\"sessionId\":\"10000000-0000-4000-8000-000000000001\",\"cwd\":\"/work/project\",",
                "\"isSidechain\":false,\"message\":{\"role\":\"user\",\"content\":\"Task\"}}\n"
            ),
        )
        .unwrap();
    }

    fn write_named_session(root: &Path, session_id: &str, title: &str, modified: SystemTime) {
        let project = root.join("-work-project");
        fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session_id}.jsonl"));
        fs::write(
            &path,
            format!(
                concat!(
                    "{{\"type\":\"user\",\"uuid\":\"00000000-0000-4000-8000-000000000001\",\"parentUuid\":null,",
                    "\"sessionId\":\"{session_id}\",\"cwd\":\"/work/project\",\"isSidechain\":false,",
                    "\"message\":{{\"role\":\"user\",\"content\":\"Task\"}}}}\n",
                    "{{\"type\":\"custom-title\",\"title\":\"{title}\",\"sessionId\":\"{session_id}\"}}\n"
                ),
                session_id = session_id,
                title = title
            ),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn missing_exact_session_preserves_the_observed_identity() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = "10000000-0000-4000-8000-000000000099";
        let config = Config {
            claude_session_dirs: vec![temp.path().to_owned()],
            ..Config::default()
        };
        let mut input = pane("p1");
        input.processes[0].argv = Some(vec![
            "claude".into(),
            "--session-id".into(),
            session_id.into(),
        ]);

        let outcomes = ClaudeBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );

        assert!(matches!(
            &outcomes[&input.key],
            BackendOutcome::FailedIdentity { agent, session_identity }
                if *agent == CLAUDE_AGENT && session_identity == session_id
        ));
    }

    #[test]
    fn single_pane_sticky_binding_does_not_switch_for_ambiguous_activity() {
        let temp = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        write_named_session(
            temp.path(),
            "10000000-0000-4000-8000-000000000001",
            "Bound",
            now - Duration::from_secs(30),
        );
        let config = Config {
            claude_session_dirs: vec![temp.path().to_owned()],
            ..Config::default()
        };
        let input = pane("p1");
        let mut backend = ClaudeBackend::default();
        backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        write_named_session(
            temp.path(),
            "10000000-0000-4000-8000-000000000002",
            "Other",
            now - Duration::from_secs(10),
        );
        write_named_session(
            temp.path(),
            "10000000-0000-4000-8000-000000000003",
            "Third",
            now,
        );

        let outcomes = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );

        let BackendOutcome::Resolved { view, .. } = &outcomes[&input.key] else {
            panic!("expected resolved sticky binding");
        };
        assert_eq!(view.session_name.as_deref(), Some("Bound"));
    }

    #[test]
    fn terminal_title_requires_a_matching_jsonl_title() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = "10000000-0000-4000-8000-000000000001";
        write_named_session(temp.path(), session_id, "Verified", SystemTime::now());
        let config = Config {
            claude_session_dirs: vec![temp.path().to_owned()],
            ..Config::default()
        };
        let mut input = pane("p1");
        input.terminal_title = Some(" \n Verified \n ignored".into());

        let outcomes = ClaudeBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        let BackendOutcome::Resolved { view, .. } = &outcomes[&input.key] else {
            panic!("expected resolved session");
        };
        assert_eq!(view.session_name.as_deref(), Some("Verified"));
        assert_eq!(view.tab_name_source.as_deref(), Some("Verified"));

        let mut mismatched = pane("p-mismatch");
        mismatched.terminal_title = Some("Inherited shell title".into());
        let outcomes = ClaudeBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&mismatched),
        );
        let BackendOutcome::Resolved { view, .. } = &outcomes[&mismatched.key] else {
            panic!("expected resolved session with mismatched terminal title");
        };
        assert_eq!(view.session_name.as_deref(), Some("Verified"));
        assert_eq!(view.tab_name_source.as_deref(), Some("Verified"));

        write_session(temp.path());
        let mut input = pane("p2");
        input.terminal_title = Some("Inherited shell title".into());
        let outcomes = ClaudeBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&input),
        );
        let BackendOutcome::Resolved { view, .. } = &outcomes[&input.key] else {
            panic!("expected resolved session without title");
        };
        assert_eq!(view.session_name, None);
        assert_eq!(view.tab_name_source, None);
    }

    #[test]
    fn single_pane_falls_back_but_multi_pane_cold_start_stays_unbound() {
        let temp = tempfile::tempdir().unwrap();
        write_session(temp.path());
        let config = Config {
            claude_session_dirs: vec![temp.path().to_owned()],
            ..Config::default()
        };
        let one = pane("p1");
        let mut backend = ClaudeBackend::default();
        let outcomes = backend.reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&one),
        );
        assert!(matches!(
            outcomes.get(&one.key),
            Some(BackendOutcome::Resolved { .. })
        ));

        let two = pane("p2");
        let outcomes = ClaudeBackend::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            &[one.clone(), two.clone()],
        );
        assert_eq!(outcomes.get(&one.key), Some(&BackendOutcome::Unbound));
        assert_eq!(outcomes.get(&two.key), Some(&BackendOutcome::Unbound));
    }
}
