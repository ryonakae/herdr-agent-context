use super::session::{SessionError, parse_session_header};
use crate::backend::{
    Binding, BindingEvidence, Candidate, DisplayView, PaneInput, PaneKey, ProcessCommand,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct Resolver {
    bindings: HashMap<PaneKey, Binding>,
    baseline: HashMap<PathBuf, Fingerprint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fingerprint {
    size: u64,
    modified_at: SystemTime,
}

impl From<&Candidate> for Fingerprint {
    fn from(candidate: &Candidate) -> Self {
        Self {
            size: candidate.size,
            modified_at: candidate.modified_at,
        }
    }
}

impl Resolver {
    pub fn bindings(&self) -> &HashMap<PaneKey, Binding> {
        &self.bindings
    }

    pub fn resolve(
        &mut self,
        panes: &[PaneInput],
        candidates: &[Candidate],
    ) -> HashMap<PaneKey, Binding> {
        let eligible: Vec<_> = panes
            .iter()
            .filter(|pane| pane.agent.eq_ignore_ascii_case("pi"))
            .filter(|pane| !has_no_session_process(&pane.processes))
            .collect();
        let live_keys: HashSet<_> = eligible.iter().map(|pane| pane.key.clone()).collect();
        self.bindings.retain(|key, _| live_keys.contains(key));

        let by_path: HashMap<_, _> = candidates
            .iter()
            .map(|candidate| (candidate.path.as_path(), candidate))
            .collect();

        for pane in &eligible {
            if let Some(reference) = &pane.authoritative_session {
                if reference.kind == "path" {
                    self.bindings.insert(
                        pane.key.clone(),
                        Binding {
                            path: PathBuf::from(&reference.value),
                            evidence: BindingEvidence::Official {
                                source: reference.source.clone(),
                            },
                        },
                    );
                    continue;
                }
            }

            let keep = self.bindings.get(&pane.key).is_some_and(|binding| {
                !binding.is_official()
                    && by_path
                        .get(binding.path.as_path())
                        .is_some_and(|candidate| {
                            canonical_or_original(&candidate.cwd)
                                == canonical_or_original(&pane.cwd)
                        })
            });
            if !keep {
                self.bindings.remove(&pane.key);
            }
        }

        let mut panes_by_cwd: HashMap<PathBuf, Vec<&PaneInput>> = HashMap::new();
        for pane in eligible.iter().copied().filter(|pane| {
            pane.authoritative_session
                .as_ref()
                .is_none_or(|reference| reference.kind != "path")
        }) {
            panes_by_cwd
                .entry(canonical_or_original(&pane.cwd))
                .or_default()
                .push(pane);
        }

        for (cwd, mut cwd_panes) in panes_by_cwd {
            cwd_panes.sort_by(|left, right| left.key.cmp(&right.key));
            let mut cwd_candidates: Vec<_> = candidates
                .iter()
                .filter(|candidate| canonical_or_original(&candidate.cwd) == cwd)
                .collect();
            cwd_candidates.sort_by(|left, right| {
                right
                    .modified_at
                    .cmp(&left.modified_at)
                    .then_with(|| left.path.cmp(&right.path))
            });

            if cwd_panes.len() == 1 {
                self.maybe_switch_single_pane(cwd_panes[0], &cwd_candidates);
            }

            let mut used: HashSet<PathBuf> = self
                .bindings
                .iter()
                .filter(|(key, _)| cwd_panes.iter().any(|pane| pane.key == **key))
                .map(|(_, binding)| binding.path.clone())
                .collect();
            for pane in cwd_panes {
                if self.bindings.contains_key(&pane.key) {
                    continue;
                }
                if let Some(candidate) = cwd_candidates
                    .iter()
                    .find(|candidate| !used.contains(&candidate.path))
                {
                    used.insert(candidate.path.clone());
                    self.bindings.insert(
                        pane.key.clone(),
                        Binding {
                            path: candidate.path.clone(),
                            evidence: BindingEvidence::LocalFallback,
                        },
                    );
                }
            }
        }

        self.baseline = candidates
            .iter()
            .map(|candidate| (candidate.path.clone(), Fingerprint::from(candidate)))
            .collect();
        self.bindings.clone()
    }

    fn maybe_switch_single_pane(&mut self, pane: &PaneInput, candidates: &[&Candidate]) {
        let Some(binding) = self.bindings.get(&pane.key).cloned() else {
            return;
        };
        if binding.is_official() {
            return;
        }
        let Some(bound) = candidates
            .iter()
            .find(|candidate| candidate.path == binding.path)
        else {
            return;
        };
        let Some(previous_bound) = self.baseline.get(&binding.path) else {
            return;
        };
        if *previous_bound != Fingerprint::from(*bound) {
            return;
        }

        let changed: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.path != binding.path)
            .filter(|candidate| {
                self.baseline
                    .get(&candidate.path)
                    .is_none_or(|previous| *previous != Fingerprint::from(**candidate))
            })
            .filter(|candidate| candidate.modified_at > bound.modified_at)
            .collect();
        if changed.len() == 1 {
            self.bindings.insert(
                pane.key.clone(),
                Binding {
                    path: changed[0].path.clone(),
                    evidence: BindingEvidence::LocalFallback,
                },
            );
        }
    }
}

pub fn has_no_session_process(processes: &[ProcessCommand]) -> bool {
    processes
        .iter()
        .flat_map(ProcessCommand::observable_args)
        .any(|argument| {
            argument == "--no-session"
                || argument
                    .split_whitespace()
                    .any(|token| token.trim_matches(['\'', '"']) == "--no-session")
        })
}

#[derive(Default)]
pub struct SessionScanner {
    cache: HashMap<PathBuf, CachedHeader>,
}

#[derive(Clone)]
struct CachedHeader {
    fingerprint: Fingerprint,
    cwd: PathBuf,
}

impl SessionScanner {
    pub fn scan(&mut self, roots: &[PathBuf]) -> Vec<Candidate> {
        let paths = discover_jsonl(roots);
        let live: HashSet<_> = paths.iter().cloned().collect();
        self.cache.retain(|path, _| live.contains(path));

        let mut candidates = Vec::new();
        for path in paths {
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let fingerprint = Fingerprint {
                size: metadata.len(),
                modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            };
            let cwd = match self.cache.get(&path) {
                Some(cached) if cached.fingerprint == fingerprint => cached.cwd.clone(),
                _ => match parse_session_header(&path) {
                    Ok((_, cwd)) => {
                        self.cache.insert(
                            path.clone(),
                            CachedHeader {
                                fingerprint,
                                cwd: cwd.clone(),
                            },
                        );
                        cwd
                    }
                    Err(_) => continue,
                },
            };
            candidates.push(Candidate {
                path,
                cwd,
                size: fingerprint.size,
                modified_at: fingerprint.modified_at,
            });
        }
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        candidates
    }
}

fn discover_jsonl(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = HashSet::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_jsonl(&path) {
                paths.insert(canonical_or_original(&path));
            } else if path.is_dir() {
                if let Ok(children) = fs::read_dir(path) {
                    for child in children.flatten() {
                        let path = child.path();
                        if path.is_file() && is_jsonl(&path) {
                            paths.insert(canonical_or_original(&path));
                        }
                    }
                }
            }
        }
    }
    paths.into_iter().collect()
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

pub fn parse_bound_session(path: &Path) -> Result<DisplayView, SessionError> {
    super::session::parse_session(path).map(|view| view.display_view())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SessionReference;
    use std::time::Duration;

    fn pane(id: &str) -> PaneInput {
        PaneInput {
            key: PaneKey {
                pane_id: id.into(),
                terminal_id: format!("term-{id}"),
            },
            agent: "pi".into(),
            cwd: "/work".into(),
            authoritative_session: None,
            processes: vec![ProcessCommand {
                name: "pi".into(),
                argv: Some(vec!["pi".into()]),
                argv0: Some("pi".into()),
                cmdline: None,
            }],
        }
    }

    fn candidate(path: &str, modified: u64, size: u64) -> Candidate {
        Candidate {
            path: path.into(),
            cwd: "/work".into(),
            size,
            modified_at: SystemTime::UNIX_EPOCH + Duration::from_secs(modified),
        }
    }

    #[test]
    fn authoritative_path_wins_even_when_not_a_candidate() {
        let mut resolver = Resolver::default();
        let mut input = pane("p1");
        input.authoritative_session = Some(SessionReference {
            source: "native-pi".into(),
            agent: "pi".into(),
            kind: "path".into(),
            value: "/missing/authoritative.jsonl".into(),
        });
        let result = resolver.resolve(&[input.clone()], &[candidate("/newer.jsonl", 20, 1)]);
        assert_eq!(
            result[&input.key],
            Binding {
                path: "/missing/authoritative.jsonl".into(),
                evidence: BindingEvidence::Official {
                    source: "native-pi".into()
                }
            }
        );
    }

    #[test]
    fn id_reference_uses_deterministic_fallback() {
        let mut resolver = Resolver::default();
        let mut input = pane("p1");
        input.authoritative_session = Some(SessionReference {
            source: "native-pi".into(),
            agent: "pi".into(),
            kind: "id".into(),
            value: "session-id".into(),
        });
        let result = resolver.resolve(
            &[input.clone()],
            &[
                candidate("/old.jsonl", 10, 1),
                candidate("/new.jsonl", 20, 1),
            ],
        );
        assert_eq!(result[&input.key].path, PathBuf::from("/new.jsonl"));
    }

    #[test]
    fn assigns_same_cwd_panes_by_mtime_then_keeps_sticky_bindings() {
        let mut resolver = Resolver::default();
        let p1 = pane("p1");
        let p2 = pane("p2");
        let first = resolver.resolve(
            &[p1.clone(), p2.clone()],
            &[
                candidate("/old.jsonl", 10, 1),
                candidate("/new.jsonl", 20, 1),
            ],
        );
        assert_eq!(first[&p1.key].path, PathBuf::from("/new.jsonl"));
        assert_eq!(first[&p2.key].path, PathBuf::from("/old.jsonl"));

        let second = resolver.resolve(
            &[p1.clone(), p2.clone()],
            &[
                candidate("/old.jsonl", 30, 2),
                candidate("/new.jsonl", 20, 1),
            ],
        );
        assert_eq!(second[&p1.key].path, PathBuf::from("/new.jsonl"));
        assert_eq!(second[&p2.key].path, PathBuf::from("/old.jsonl"));
    }

    #[test]
    fn single_pane_switches_only_for_one_newer_changed_alternative() {
        let mut resolver = Resolver::default();
        let input = pane("p1");
        resolver.resolve(
            std::slice::from_ref(&input),
            &[candidate("/bound.jsonl", 20, 1)],
        );
        let switched = resolver.resolve(
            std::slice::from_ref(&input),
            &[
                candidate("/bound.jsonl", 20, 1),
                candidate("/resumed.jsonl", 30, 2),
            ],
        );
        assert_eq!(switched[&input.key].path, PathBuf::from("/resumed.jsonl"));

        let stable = resolver.resolve(
            std::slice::from_ref(&input),
            &[
                candidate("/bound.jsonl", 20, 1),
                candidate("/resumed.jsonl", 30, 2),
            ],
        );
        assert_eq!(stable[&input.key].path, PathBuf::from("/resumed.jsonl"));
    }

    #[test]
    fn single_pane_does_not_switch_on_bound_or_ambiguous_activity() {
        let mut resolver = Resolver::default();
        let input = pane("p1");
        resolver.resolve(
            std::slice::from_ref(&input),
            &[candidate("/bound.jsonl", 20, 1)],
        );
        let bound_changed = resolver.resolve(
            std::slice::from_ref(&input),
            &[
                candidate("/bound.jsonl", 21, 2),
                candidate("/other.jsonl", 30, 2),
            ],
        );
        assert_eq!(
            bound_changed[&input.key].path,
            PathBuf::from("/bound.jsonl")
        );

        let ambiguous = resolver.resolve(
            std::slice::from_ref(&input),
            &[
                candidate("/bound.jsonl", 21, 2),
                candidate("/other.jsonl", 31, 3),
                candidate("/third.jsonl", 32, 3),
            ],
        );
        assert_eq!(ambiguous[&input.key].path, PathBuf::from("/bound.jsonl"));
    }

    #[test]
    fn no_session_and_non_pi_panes_are_unbound_and_cleanup_old_state() {
        let mut resolver = Resolver::default();
        let input = pane("p1");
        resolver.resolve(
            std::slice::from_ref(&input),
            &[candidate("/one.jsonl", 10, 1)],
        );
        let mut ephemeral = input.clone();
        ephemeral.processes = vec![ProcessCommand {
            name: "sh".into(),
            argv: Some(vec!["sh".into(), "-lc".into(), "pi --no-session".into()]),
            argv0: Some("sh".into()),
            cmdline: None,
        }];
        assert!(
            resolver
                .resolve(&[ephemeral], &[candidate("/one.jsonl", 10, 1)])
                .is_empty()
        );

        let mut other = input;
        other.agent = "codex".into();
        assert!(
            resolver
                .resolve(&[other], &[candidate("/one.jsonl", 10, 1)])
                .is_empty()
        );
    }

    #[test]
    fn scanner_reads_direct_and_cwd_directory_sessions_once() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("--work--");
        fs::create_dir(&nested).unwrap();
        let direct = temp.path().join("direct.jsonl");
        let nested_file = nested.join("nested.jsonl");
        let header = "{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/work\"}\n";
        fs::write(&direct, header).unwrap();
        fs::write(&nested_file, header).unwrap();
        fs::write(temp.path().join("ignored.txt"), header).unwrap();

        let mut scanner = SessionScanner::default();
        let roots = vec![temp.path().to_owned(), temp.path().to_owned()];
        let candidates = scanner.scan(&roots);
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.cwd == Path::new("/work"))
        );
    }

    #[test]
    fn changing_terminal_identity_invalidates_sticky_binding() {
        let mut resolver = Resolver::default();
        let input = pane("p1");
        resolver.resolve(
            std::slice::from_ref(&input),
            &[candidate("/one.jsonl", 10, 1)],
        );
        let mut replaced = input;
        replaced.key.terminal_id = "new-terminal".into();
        let result = resolver.resolve(
            std::slice::from_ref(&replaced),
            &[candidate("/two.jsonl", 20, 1)],
        );
        assert_eq!(result[&replaced.key].path, PathBuf::from("/two.jsonl"));
        assert_eq!(result.len(), 1);
    }
}
