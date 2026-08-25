pub mod resolver;
pub mod session;

use crate::backend::{BackendOutcome, DisplayView, PI_AGENT, PaneInput, PaneKey};
use crate::config::{Config, resolve_pi_agent_dir, resolve_session_roots};
use resolver::{Resolver, SessionScanner, parse_bound_session};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct PiBackend {
    resolver: Resolver,
    scanner: SessionScanner,
    parsed: HashMap<PathBuf, ParsedCache>,
}

#[derive(Clone)]
struct ParsedCache {
    fingerprint: FileFingerprint,
    view: DisplayView,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileFingerprint {
    size: u64,
    modified_at: SystemTime,
}

impl PiBackend {
    pub(crate) fn binding(&self, key: &PaneKey) -> Option<&crate::backend::Binding> {
        self.resolver.bindings().get(key)
    }

    pub(crate) fn authoritative_binding(&self, key: &PaneKey) -> Option<&crate::backend::Binding> {
        self.binding(key).filter(|binding| binding.is_official())
    }

    pub fn reconcile(
        &mut self,
        config: &Config,
        home: &Path,
        env: &HashMap<String, String>,
        panes: &[PaneInput],
    ) -> HashMap<PaneKey, BackendOutcome> {
        let roots = resolve_session_roots(env, home, &config.pi_session_dirs);
        let candidates = self.scanner.scan(&roots);
        let integration_installed = resolve_pi_agent_dir(env, home)
            .join("extensions/herdr-agent-state.ts")
            .is_file();
        let bindings = self.resolver.resolve_with_authority_requirement(
            panes,
            &candidates,
            integration_installed,
        );
        panes
            .iter()
            .filter(|pane| pane.agent.eq_ignore_ascii_case(PI_AGENT))
            .map(|pane| {
                let outcome = match bindings.get(&pane.key) {
                    None => BackendOutcome::Unbound,
                    Some(binding) => match self.load_view(&binding.path) {
                        Some(view) => BackendOutcome::Resolved {
                            agent: PI_AGENT,
                            binding: binding.clone(),
                            view,
                        },
                        None => BackendOutcome::FailedBinding {
                            agent: PI_AGENT,
                            binding: binding.clone(),
                        },
                    },
                };
                (pane.key.clone(), outcome)
            })
            .collect()
    }

    fn load_view(&mut self, path: &Path) -> Option<DisplayView> {
        let metadata = fs::metadata(path).ok()?;
        let fingerprint = FileFingerprint {
            size: metadata.len(),
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        };
        fs::File::open(path).ok()?;
        if let Some(cached) = self.parsed.get(path) {
            if cached.fingerprint == fingerprint {
                return Some(cached.view.clone());
            }
        }
        let view = parse_bound_session(path).ok()?;
        self.parsed.insert(
            path.to_owned(),
            ParsedCache {
                fingerprint,
                view: view.clone(),
            },
        );
        Some(view)
    }
}
