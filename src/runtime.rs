use crate::backend::{
    BackendOutcome, BackendRegistry, Binding, CLAUDE_AGENT, DisplayView, PaneInput, PaneKey,
    ProcessCommand, SessionReference,
};
use crate::config::Config;
use crate::herdr::{HerdrApi, MetadataReport};
use crate::tab_name::{
    DisplayState, PaneContext, PaneSnapshot as TabPaneSnapshot, RenameCompletion, TabNameError,
    TabNameManager, TabRenameObservation, TabSnapshot as ManagedTabSnapshot,
};
use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct Runtime {
    config: Config,
    home: PathBuf,
    env: HashMap<String, String>,
    backends: BackendRegistry,
    panes: HashMap<String, PaneState>,
    sequences: HashMap<String, u64>,
    sequence_epoch: u64,
    tab_names: Option<TabNameManager>,
    pending_tab_renames: Vec<TabRenameObservation>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReconcileStatus {
    pub tab_name_disabled: bool,
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
            tab_names: None,
            pending_tab_renames: Vec::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    pub fn initialize_tab_names(
        &mut self,
        state_dir: &Path,
        socket_path: &Path,
    ) -> Result<(), TabNameError> {
        self.tab_names = Some(TabNameManager::load(state_dir, socket_path)?);
        Ok(())
    }

    pub fn tab_names_available(&self) -> bool {
        self.tab_names.is_some()
    }

    pub fn reset_tab_event_expectations(&mut self) {
        self.pending_tab_renames.clear();
        if let Some(manager) = self.tab_names.as_mut() {
            manager.reset_event_expectations();
        }
    }

    pub fn note_tab_rename(&mut self, tab_id: Option<&str>, label: Option<&str>) {
        if !self.tab_event_reconcile_needed() {
            return;
        }
        let (Some(tab_id), Some(label)) = (tab_id, label) else {
            return;
        };
        self.pending_tab_renames.push(TabRenameObservation {
            tab_id: tab_id.to_owned(),
            label: label.to_owned(),
        });
    }

    pub fn tab_event_reconcile_needed(&self) -> bool {
        self.tab_names
            .as_ref()
            .is_some_and(|manager| manager.needs_snapshot(self.config.tab_name.enabled))
    }

    pub fn reconcile<A: HerdrApi>(&mut self, api: &mut A) -> Result<ReconcileStatus, A::Error> {
        self.reconcile_at(api, Instant::now())
    }

    pub fn reconcile_at<A: HerdrApi>(
        &mut self,
        api: &mut A,
        now: Instant,
    ) -> Result<ReconcileStatus, A::Error> {
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
                workspace_id: agent.workspace_id.clone(),
                tab_id: agent.tab_id.clone(),
                agent: agent.agent.clone().unwrap_or_default(),
                cwd: PathBuf::from(cwd),
                terminal_title: agent.terminal_title_stripped.clone(),
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
                Some(
                    BackendOutcome::Failed
                    | BackendOutcome::FailedBinding { .. }
                    | BackendOutcome::FailedIdentity { .. },
                ) => {
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
        self.reconcile_tab_names(api, &pane_inputs, &outcomes, now)
    }

    fn reconcile_tab_names<A: HerdrApi>(
        &mut self,
        api: &mut A,
        panes: &[PaneInput],
        outcomes: &HashMap<PaneKey, BackendOutcome>,
        now: Instant,
    ) -> Result<ReconcileStatus, A::Error> {
        let Some(manager) = self.tab_names.as_ref() else {
            return Ok(ReconcileStatus::default());
        };
        if !manager.needs_snapshot(self.config.tab_name.enabled) {
            return Ok(ReconcileStatus::default());
        }
        let snapshot = match api.session_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) if A::is_tab_topology_error(&error) => {
                self.tab_names = None;
                return Ok(ReconcileStatus {
                    tab_name_disabled: true,
                });
            }
            Err(error) => return Err(error),
        };
        let mut layouts = HashMap::new();
        for layout in &snapshot.layouts {
            if layout.tab_id.is_empty()
                || layout.workspace_id.is_empty()
                || layouts.insert(layout.tab_id.as_str(), layout).is_some()
            {
                self.tab_names = None;
                return Ok(ReconcileStatus {
                    tab_name_disabled: true,
                });
            }
        }
        let mut positions = HashMap::<&str, usize>::new();
        let mut tabs = Vec::with_capacity(snapshot.tabs.len());
        let mut seen_tabs = HashSet::new();
        let mut snapshot_tab_workspaces = HashMap::new();
        for tab in &snapshot.tabs {
            let Some(layout) = layouts.get(tab.tab_id.as_str()) else {
                self.tab_names = None;
                return Ok(ReconcileStatus {
                    tab_name_disabled: true,
                });
            };
            if tab.tab_id.is_empty()
                || tab.workspace_id.is_empty()
                || layout.workspace_id != tab.workspace_id
                || !seen_tabs.insert(tab.tab_id.as_str())
            {
                self.tab_names = None;
                return Ok(ReconcileStatus {
                    tab_name_disabled: true,
                });
            }
            snapshot_tab_workspaces.insert(tab.tab_id.as_str(), tab.workspace_id.as_str());
            let position = positions.entry(tab.workspace_id.as_str()).or_default();
            *position += 1;
            tabs.push(ManagedTabSnapshot {
                tab_id: tab.tab_id.clone(),
                workspace_id: tab.workspace_id.clone(),
                position: *position,
                observed_label: tab.label.clone(),
            });
        }

        if layouts.len() != seen_tabs.len() {
            self.tab_names = None;
            return Ok(ReconcileStatus {
                tab_name_disabled: true,
            });
        }
        let mut snapshot_panes = HashMap::new();
        for pane in &snapshot.panes {
            if pane.pane_id.is_empty()
                || snapshot_tab_workspaces.get(pane.tab_id.as_str()).copied()
                    != Some(pane.workspace_id.as_str())
                || snapshot_panes.insert(pane.pane_id.as_str(), pane).is_some()
            {
                self.tab_names = None;
                return Ok(ReconcileStatus {
                    tab_name_disabled: true,
                });
            }
        }

        let mut layout_pane_ids = HashSet::new();
        for layout in &snapshot.layouts {
            for layout_pane in &layout.panes {
                if layout_pane.pane_id.is_empty()
                    || !layout_pane_ids.insert(layout_pane.pane_id.as_str())
                    || snapshot_panes
                        .get(layout_pane.pane_id.as_str())
                        .is_none_or(|pane| {
                            pane.tab_id != layout.tab_id || pane.workspace_id != layout.workspace_id
                        })
                {
                    self.tab_names = None;
                    return Ok(ReconcileStatus {
                        tab_name_disabled: true,
                    });
                }
            }
        }
        if layout_pane_ids.len() != snapshot_panes.len() {
            self.tab_names = None;
            return Ok(ReconcileStatus {
                tab_name_disabled: true,
            });
        }

        if self.config.tab_name.enabled {
            for pane in panes {
                let (Some(workspace_id), Some(tab_id)) = (&pane.workspace_id, &pane.tab_id) else {
                    return Ok(ReconcileStatus::default());
                };
                let Some(snapshot_pane) = snapshot_panes.get(pane.key.pane_id.as_str()) else {
                    return Ok(ReconcileStatus::default());
                };
                if snapshot_pane.workspace_id != *workspace_id || snapshot_pane.tab_id != *tab_id {
                    return Ok(ReconcileStatus::default());
                }
            }
        }

        let pane_inputs: HashMap<_, _> = panes
            .iter()
            .map(|pane| (pane.key.pane_id.as_str(), pane))
            .collect();
        let mut tab_panes = Vec::with_capacity(snapshot.panes.len());
        for tab in &tabs {
            let layout = layouts[tab.tab_id.as_str()];
            let mut ordered_panes: Vec<_> = layout.panes.iter().collect();
            ordered_panes.sort_by_key(|pane| (pane.rect.y, pane.rect.x, pane.pane_id.as_str()));
            for layout_pane in ordered_panes {
                let (context, binding_identity) = pane_inputs
                    .get(layout_pane.pane_id.as_str())
                    .map_or((PaneContext::Unsupported, None), |pane| {
                        match outcomes.get(&pane.key) {
                            Some(BackendOutcome::Resolved {
                                agent,
                                binding,
                                view,
                            }) => (
                                PaneContext::Supported {
                                    agent: (*agent).to_owned(),
                                    identity: view.session_identity.clone(),
                                    display: view
                                        .tab_name_source
                                        .clone()
                                        .map(DisplayState::Resolved)
                                        .unwrap_or(DisplayState::Unresolved),
                                },
                                Some(contributor_binding_identity(
                                    agent,
                                    binding,
                                    Some(&view.session_identity),
                                )),
                            ),
                            Some(BackendOutcome::FailedBinding { agent, binding }) => (
                                PaneContext::Failed {
                                    agent: (*agent).to_owned(),
                                },
                                Some(contributor_binding_identity(agent, binding, None)),
                            ),
                            Some(BackendOutcome::FailedIdentity {
                                agent,
                                session_identity,
                            }) => (
                                PaneContext::Supported {
                                    agent: (*agent).to_owned(),
                                    identity: session_identity.clone(),
                                    display: DisplayState::Failed,
                                },
                                Some(session_identity.as_bytes().to_vec()),
                            ),
                            Some(BackendOutcome::Failed) => {
                                let binding = self.backends.binding(&pane.key);
                                let retained = self.panes.get(&pane.key.pane_id).filter(|state| {
                                    state.reported
                                        && state.terminal_id == pane.key.terminal_id
                                        && state.agent.eq_ignore_ascii_case(&pane.agent)
                                        && binding
                                            .is_some_and(|binding| state.binding == binding.path)
                                });
                                let context = retained
                                    .map(|state| PaneContext::Supported {
                                        agent: state.agent.clone(),
                                        identity: state.session_identity.clone(),
                                        display: DisplayState::Failed,
                                    })
                                    .unwrap_or(PaneContext::Unsupported);
                                let binding_identity = binding.map(|binding| {
                                    contributor_binding_identity(
                                        retained.map_or(pane.agent.as_str(), |state| {
                                            state.agent.as_str()
                                        }),
                                        binding,
                                        retained.map(|state| state.session_identity.as_str()),
                                    )
                                });
                                (context, binding_identity)
                            }
                            Some(BackendOutcome::Unbound) | None => {
                                (PaneContext::Unsupported, None)
                            }
                        }
                    });
                tab_panes.push(TabPaneSnapshot {
                    pane_id: layout_pane.pane_id.clone(),
                    terminal_id: pane_inputs
                        .get(layout_pane.pane_id.as_str())
                        .map_or_else(String::new, |pane| pane.key.terminal_id.clone()),
                    binding_identity,
                    tab_id: tab.tab_id.clone(),
                    context,
                });
            }
        }

        let rename_observations = std::mem::take(&mut self.pending_tab_renames);
        if self
            .tab_names
            .as_mut()
            .unwrap()
            .observe_renames(&rename_observations, &tabs, &tab_panes)
            .is_err()
        {
            self.tab_names = None;
            return Ok(ReconcileStatus {
                tab_name_disabled: true,
            });
        }

        let effects = match self.tab_names.as_mut().unwrap().reconcile(
            self.config.tab_name.enabled,
            &tabs,
            &tab_panes,
            now,
        ) {
            Ok(effects) => effects,
            Err(_) => {
                self.tab_names = None;
                return Ok(ReconcileStatus {
                    tab_name_disabled: true,
                });
            }
        };

        let mut disable_tab_names = false;
        for effect in effects {
            let completion = match api.rename_tab(effect.tab_id(), effect.label()) {
                Ok(tab) if tab.tab_id == effect.tab_id() && tab.label == effect.label() => {
                    RenameCompletion::Applied
                }
                Ok(_) => {
                    disable_tab_names = true;
                    break;
                }
                Err(error) if A::is_missing_tab_error(&error) => RenameCompletion::MissingTab,
                Err(error) => return Err(error),
            };
            if self
                .tab_names
                .as_mut()
                .unwrap()
                .complete_rename(effect.token(), completion)
                .is_err()
            {
                disable_tab_names = true;
                break;
            }
        }
        if disable_tab_names {
            self.tab_names = None;
        }
        Ok(ReconcileStatus {
            tab_name_disabled: disable_tab_names,
        })
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

fn contributor_binding_identity(
    agent: &str,
    binding: &Binding,
    session_identity: Option<&str>,
) -> Vec<u8> {
    if agent.eq_ignore_ascii_case(CLAUDE_AGENT)
        && let Some(session_identity) = session_identity
    {
        return session_identity.as_bytes().to_vec();
    }
    binding.path.as_os_str().as_bytes().to_vec()
}
