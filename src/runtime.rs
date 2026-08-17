use crate::config::{Config, resolve_session_roots};
use crate::herdr::HerdrApi;
use crate::herdr::protocol::AgentInfo;
use crate::pi::resolver::{
    Binding, PaneInput, PaneKey, Resolver, SessionReference, SessionScanner, parse_bound_session,
};
use crate::pi::session::PiSessionView;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Runtime {
    config: Config,
    home: PathBuf,
    env: HashMap<String, String>,
    resolver: Resolver,
    scanner: SessionScanner,
    panes: HashMap<String, PaneState>,
    sequences: HashMap<String, u64>,
    sequence_epoch: u64,
    parsed: HashMap<PathBuf, ParsedCache>,
}

#[derive(Clone, Debug)]
struct PaneState {
    terminal_id: String,
    binding: PathBuf,
    session_name: Option<String>,
    last_message: Option<String>,
    reported: bool,
}

#[derive(Clone)]
struct ParsedCache {
    fingerprint: FileFingerprint,
    view: Option<PiSessionView>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileFingerprint {
    size: u64,
    modified_at: SystemTime,
}

impl Runtime {
    pub fn new(config: Config, home: PathBuf, env: HashMap<String, String>) -> Self {
        Self {
            config,
            home,
            env,
            resolver: Resolver::default(),
            scanner: SessionScanner::default(),
            panes: HashMap::new(),
            sequences: HashMap::new(),
            sequence_epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            parsed: HashMap::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    pub fn reconcile<A: HerdrApi>(&mut self, api: &mut A) -> Result<(), A::Error> {
        let agents = api.list_agents()?;
        let mut pane_inputs = Vec::new();
        let mut applies_to_sources = HashMap::new();

        for agent in agents.iter().filter(|agent| is_pi(agent)) {
            let Some(cwd) = agent.foreground_cwd.as_ref().or(agent.cwd.as_ref()) else {
                continue;
            };
            let process_args = api.process_info(&agent.pane_id)?.args();
            let authoritative_session =
                agent
                    .agent_session
                    .as_ref()
                    .map(|session| SessionReference {
                        kind: session.kind.clone(),
                        value: session.value.clone(),
                    });
            if let Some(session) = agent
                .agent_session
                .as_ref()
                .filter(|session| session.kind == "path")
            {
                applies_to_sources.insert(agent.pane_id.clone(), session.source.clone());
            }
            pane_inputs.push(PaneInput {
                key: PaneKey {
                    pane_id: agent.pane_id.clone(),
                    terminal_id: agent.terminal_id.clone(),
                },
                agent: agent.agent.clone().unwrap_or_default(),
                cwd: PathBuf::from(cwd),
                authoritative_session,
                process_args,
            });
        }

        self.clear_stale_panes(api, &pane_inputs)?;
        let roots = resolve_session_roots(&self.env, &self.home, &self.config.pi_session_dirs);
        let candidates = self.scanner.scan(&roots);
        let bindings = self.resolver.resolve(&pane_inputs, &candidates);

        for pane in &pane_inputs {
            let Some(binding) = bindings.get(&pane.key) else {
                self.clear_if_reported(api, &pane.key.pane_id)?;
                continue;
            };
            let Some(view) = self.load_view(&binding.path) else {
                continue;
            };
            self.report_view(
                api,
                pane,
                binding,
                view,
                applies_to_sources
                    .get(&pane.key.pane_id)
                    .map(String::as_str),
            )?;
        }
        Ok(())
    }

    fn clear_stale_panes<A: HerdrApi>(
        &mut self,
        api: &mut A,
        current: &[PaneInput],
    ) -> Result<(), A::Error> {
        let live: HashSet<_> = current
            .iter()
            .map(|pane| (pane.key.pane_id.as_str(), pane.key.terminal_id.as_str()))
            .collect();
        let stale: Vec<_> = self
            .panes
            .iter()
            .filter(|(pane_id, state)| {
                !live.contains(&(pane_id.as_str(), state.terminal_id.as_str()))
            })
            .map(|(pane_id, _)| pane_id.clone())
            .collect();
        for pane_id in stale {
            match self.clear_if_reported(api, &pane_id) {
                Ok(()) => {
                    self.panes.remove(&pane_id);
                }
                Err(error) if A::is_missing_pane_error(&error) => {
                    self.panes.remove(&pane_id);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn report_view<A: HerdrApi>(
        &mut self,
        api: &mut A,
        pane: &PaneInput,
        binding: &Binding,
        view: PiSessionView,
        applies_to_source: Option<&str>,
    ) -> Result<(), A::Error> {
        let previous = self.panes.get(&pane.key.pane_id);
        let same_session = previous.is_some_and(|state| {
            state.terminal_id == pane.key.terminal_id && state.binding == binding.path
        });
        let last_message = view.latest_turn_assistant_line.clone().or_else(|| {
            same_session
                .then(|| previous.and_then(|state| state.last_message.clone()))
                .flatten()
        });
        let session_name = view.session_name();
        let seq = self.next_sequence(&pane.key.pane_id);
        api.report_metadata(
            &pane.key.pane_id,
            applies_to_source,
            seq,
            self.config.metadata_ttl_ms,
            session_name.as_deref(),
            last_message.as_deref(),
        )?;
        self.panes.insert(
            pane.key.pane_id.clone(),
            PaneState {
                terminal_id: pane.key.terminal_id.clone(),
                binding: binding.path.clone(),
                session_name,
                last_message,
                reported: true,
            },
        );
        Ok(())
    }

    fn clear_if_reported<A: HerdrApi>(
        &mut self,
        api: &mut A,
        pane_id: &str,
    ) -> Result<(), A::Error> {
        if !self.panes.get(pane_id).is_some_and(|state| state.reported) {
            return Ok(());
        }
        let seq = self.next_sequence(pane_id);
        api.report_metadata(pane_id, None, seq, self.config.metadata_ttl_ms, None, None)?;
        if let Some(state) = self.panes.get_mut(pane_id) {
            state.reported = false;
            state.session_name = None;
            state.last_message = None;
        }
        Ok(())
    }

    fn next_sequence(&mut self, pane_id: &str) -> u64 {
        let sequence = self
            .sequences
            .entry(pane_id.to_owned())
            .or_insert(self.sequence_epoch);
        *sequence += 1;
        *sequence
    }

    fn load_view(&mut self, path: &Path) -> Option<PiSessionView> {
        let metadata = fs::metadata(path).ok()?;
        let fingerprint = FileFingerprint {
            size: metadata.len(),
            modified_at: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        };
        fs::File::open(path).ok()?;
        if let Some(cached) = self.parsed.get(path) {
            if cached.fingerprint == fingerprint {
                return cached.view.clone();
            }
        }
        let view = parse_bound_session(path).ok();
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

fn is_pi(agent: &AgentInfo) -> bool {
    agent
        .agent
        .as_deref()
        .is_some_and(|agent| agent.eq_ignore_ascii_case("pi"))
}
