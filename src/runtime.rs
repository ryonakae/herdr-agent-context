use crate::backend::{
    BackendOutcome, BackendRegistry, Binding, DisplayView, PaneInput, PaneKey, ProcessCommand,
    SessionReference,
};
use crate::config::Config;
use crate::herdr::{HerdrApi, MetadataReport};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Runtime {
    config: Config,
    home: PathBuf,
    env: HashMap<String, String>,
    backends: BackendRegistry,
    panes: HashMap<String, PaneState>,
    sequences: HashMap<String, u64>,
    sequence_epoch: u64,
}

#[derive(Clone, Debug)]
struct PaneState {
    terminal_id: String,
    agent: String,
    binding: PathBuf,
    session_identity: String,
    session_name: Option<String>,
    last_message: Option<String>,
    reported: bool,
}

impl Runtime {
    pub fn new(config: Config, home: PathBuf, env: HashMap<String, String>) -> Self {
        Self {
            config,
            home,
            env,
            backends: BackendRegistry::default(),
            panes: HashMap::new(),
            sequences: HashMap::new(),
            sequence_epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
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

        for agent in agents
            .iter()
            .filter(|agent| self.backends.supports_agent(agent.agent.as_deref()))
        {
            let Some(cwd) = agent.foreground_cwd.as_ref().or(agent.cwd.as_ref()) else {
                continue;
            };
            let processes = api
                .process_info(&agent.pane_id)?
                .foreground_processes
                .into_iter()
                .map(|process| ProcessCommand {
                    name: process.name,
                    argv: process.argv,
                    argv0: process.argv0,
                    cmdline: process.cmdline,
                })
                .collect();
            let authoritative_session =
                agent
                    .agent_session
                    .as_ref()
                    .map(|session| SessionReference {
                        source: session.source.clone(),
                        agent: session.agent.clone(),
                        kind: session.kind.clone(),
                        value: session.value.clone(),
                    });
            pane_inputs.push(PaneInput {
                key: PaneKey {
                    pane_id: agent.pane_id.clone(),
                    terminal_id: agent.terminal_id.clone(),
                },
                agent: agent.agent.clone().unwrap_or_default(),
                cwd: PathBuf::from(cwd),
                authoritative_session,
                processes,
            });
        }

        self.clear_stale_panes(api, &pane_inputs)?;
        let outcomes = self
            .backends
            .reconcile(&self.config, &self.home, &self.env, &pane_inputs);

        for pane in &pane_inputs {
            match outcomes.get(&pane.key) {
                Some(BackendOutcome::Resolved {
                    agent,
                    binding,
                    view,
                }) => self.report_view(api, pane, agent, binding, view.clone())?,
                Some(BackendOutcome::Failed) => {
                    let binding = self.backends.authoritative_binding(&pane.key).cloned();
                    if let Some(binding) = binding {
                        self.clear_if_binding_changed(api, pane, &binding)?;
                    }
                }
                Some(BackendOutcome::Unbound) | None => {
                    self.clear_if_reported(api, &pane.key.pane_id)?;
                }
            }
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
        agent: &str,
        binding: &Binding,
        view: DisplayView,
    ) -> Result<(), A::Error> {
        let previous = self.panes.get(&pane.key.pane_id);
        let same_session = previous.is_some_and(|state| {
            state.terminal_id == pane.key.terminal_id
                && state.agent == agent
                && state.binding == binding.path
                && state.session_identity == view.session_identity
        });
        let last_message = view.last_message.or_else(|| {
            same_session
                .then(|| previous.and_then(|state| state.last_message.clone()))
                .flatten()
        });
        let session_name = view.session_name;
        let seq = self.next_sequence(&pane.key.pane_id);
        api.report_metadata(MetadataReport {
            agent,
            pane_id: &pane.key.pane_id,
            applies_to_source: binding.applies_to_source(),
            seq,
            ttl_ms: self.config.metadata_ttl_ms,
            session_name: session_name.as_deref(),
            last_message: last_message.as_deref(),
        })?;
        self.panes.insert(
            pane.key.pane_id.clone(),
            PaneState {
                terminal_id: pane.key.terminal_id.clone(),
                agent: agent.to_owned(),
                binding: binding.path.clone(),
                session_identity: view.session_identity,
                session_name,
                last_message,
                reported: true,
            },
        );
        Ok(())
    }

    fn clear_if_binding_changed<A: HerdrApi>(
        &mut self,
        api: &mut A,
        pane: &PaneInput,
        binding: &Binding,
    ) -> Result<(), A::Error> {
        let changed = self.panes.get(&pane.key.pane_id).is_some_and(|state| {
            state.terminal_id == pane.key.terminal_id && state.binding != binding.path
        });
        if changed {
            self.clear_if_reported(api, &pane.key.pane_id)?;
        }
        Ok(())
    }

    fn clear_if_reported<A: HerdrApi>(
        &mut self,
        api: &mut A,
        pane_id: &str,
    ) -> Result<(), A::Error> {
        let Some(agent) = self
            .panes
            .get(pane_id)
            .filter(|state| state.reported)
            .map(|state| state.agent.clone())
        else {
            return Ok(());
        };
        let seq = self.next_sequence(pane_id);
        api.report_metadata(MetadataReport {
            agent: &agent,
            pane_id,
            applies_to_source: None,
            seq,
            ttl_ms: self.config.metadata_ttl_ms,
            session_name: None,
            last_message: None,
        })?;
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
}
