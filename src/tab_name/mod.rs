mod state;

use crate::text::context_label;
use state::{
    Applied, AppliedSource, Baseline, PendingDisposition, PendingTransition, PersistedState,
    Selection, StateFile, TabState, digest_composition, digest_contributor_generations,
    digest_identity, digest_label,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::time::Instant;
use thiserror::Error;

pub struct TabSnapshot {
    pub tab_id: String,
    pub workspace_id: String,
    pub position: usize,
    pub observed_label: String,
}

pub struct PaneSnapshot {
    pub pane_id: String,
    pub terminal_id: String,
    pub binding_identity: Option<Vec<u8>>,
    pub tab_id: String,
    pub context: PaneContext,
}

pub struct TabRenameObservation {
    pub tab_id: String,
    pub label: String,
}

pub enum PaneContext {
    Unsupported,
    Failed {
        agent: String,
    },
    Supported {
        agent: String,
        identity: String,
        display: DisplayState,
    },
}

pub enum DisplayState {
    Resolved(String),
    Unresolved,
    Failed,
}

pub struct RenameEffect {
    tab_id: String,
    label: String,
    token: TransitionToken,
}

impl RenameEffect {
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn token(&self) -> &TransitionToken {
        &self.token
    }
}

#[derive(Clone)]
pub struct TransitionToken {
    tab_id: String,
    target_digest: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RenameCompletion {
    Applied,
    MissingTab,
}

#[derive(Debug, Error)]
pub enum TabNameError {
    #[error("tab name state is unavailable")]
    State,
    #[error("tab name transition is stale")]
    StaleTransition,
}

pub struct TabNameManager {
    state_file: StateFile,
    document: PersistedState,
    retained_components: HashMap<String, RetainedComponent>,
    expected_events: HashMap<String, VecDeque<String>>,
}

struct RetainedComponent {
    identity_digest: String,
    label: String,
}

#[derive(Clone)]
struct Composition {
    selection: Selection,
    label: String,
}

struct IncompleteCompositionEvidence {
    generation_digest: String,
    identity_digest: Option<String>,
}

impl TabNameManager {
    pub fn load(state_dir: &Path, socket_path: &Path) -> Result<Self, TabNameError> {
        let (state_file, document) =
            StateFile::open(state_dir, socket_path).map_err(|_| TabNameError::State)?;
        Ok(Self {
            state_file,
            document,
            retained_components: HashMap::new(),
            expected_events: HashMap::new(),
        })
    }

    pub fn reset_event_expectations(&mut self) {
        self.expected_events.clear();
    }

    pub fn needs_snapshot(&self, enabled: bool) -> bool {
        enabled || self.document.cleanup_pending || !self.document.tabs.is_empty()
    }

    pub fn observe_renames(
        &mut self,
        observations: &[TabRenameObservation],
        tabs: &[TabSnapshot],
        panes: &[PaneSnapshot],
    ) -> Result<(), TabNameError> {
        let before = self.document.clone();
        let compositions = self.compositions(tabs, panes);
        let tabs_by_id: HashMap<_, _> = tabs.iter().map(|tab| (tab.tab_id.as_str(), tab)).collect();
        for observation in observations {
            let observed_digest = digest_label(&observation.label);
            if self.consume_expected_event(&observation.tab_id, &observed_digest) {
                continue;
            }
            let Some(tab) = tabs_by_id.get(observation.tab_id.as_str()).copied() else {
                continue;
            };
            let composition = compositions
                .get(&observation.tab_id)
                .and_then(Option::as_ref);
            let incomplete_evidence = if self.document.cleanup_pending {
                None
            } else {
                self.incomplete_composition_evidence(tab, panes)
            };
            let selection = self
                .document
                .tabs
                .get(&observation.tab_id)
                .and_then(|tab_state| {
                    incomplete_evidence
                        .as_ref()
                        .and_then(|evidence| recover_incomplete_selection(tab_state, evidence))
                        .or_else(|| composition.map(|value| value.selection.clone()))
                });
            let Some(tab_state) = self.document.tabs.get_mut(&observation.tab_id) else {
                continue;
            };
            let pending_target = tab_state
                .pending
                .as_ref()
                .map(|pending| pending.target_digest.as_str());
            let applied_target = tab_state
                .applied
                .as_ref()
                .map(|applied| applied.target_digest.as_str());
            if tab_state.stale_target_digest.as_deref() == Some(observed_digest.as_str()) {
                tab_state.stale_target_digest = None;
                continue;
            }
            if pending_target == Some(observed_digest.as_str())
                || applied_target == Some(observed_digest.as_str())
            {
                continue;
            }

            let stale_target_digest = tab_state
                .stale_target_digest
                .clone()
                .or_else(|| pending_target.map(ToOwned::to_owned))
                .or_else(|| applied_target.map(ToOwned::to_owned));
            record_manual_label(tab_state, selection.as_ref(), &observation.label);
            tab_state.stale_target_digest = stale_target_digest;
        }
        if self.document != before {
            if let Err(error) = self.persist_document() {
                self.document = before;
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn reconcile(
        &mut self,
        enabled: bool,
        tabs: &[TabSnapshot],
        panes: &[PaneSnapshot],
        _now: Instant,
    ) -> Result<Vec<RenameEffect>, TabNameError> {
        let before = self.document.clone();
        let live_tabs: std::collections::HashSet<_> =
            tabs.iter().map(|tab| tab.tab_id.as_str()).collect();
        self.document
            .tabs
            .retain(|tab_id, _| live_tabs.contains(tab_id.as_str()));
        self.expected_events
            .retain(|tab_id, _| live_tabs.contains(tab_id.as_str()));

        if !enabled && !self.document.tabs.is_empty() {
            self.document.cleanup_pending = true;
        }
        let acquiring = enabled && !self.document.cleanup_pending;
        let compositions = self.compositions(tabs, panes);
        let mut effects = Vec::new();
        let mut release_tabs = Vec::new();

        for tab in tabs {
            let generated_composition = compositions.get(&tab.tab_id).and_then(Option::as_ref);
            let incomplete_evidence = if acquiring {
                self.incomplete_composition_evidence(tab, panes)
            } else {
                None
            };
            let recovered_selection = self.document.tabs.get(&tab.tab_id).and_then(|tab_state| {
                incomplete_evidence
                    .as_ref()
                    .and_then(|evidence| recover_incomplete_selection(tab_state, evidence))
            });
            let composition = if recovered_selection.is_some() {
                None
            } else {
                generated_composition
            };
            let active_selection = recovered_selection
                .clone()
                .or_else(|| composition.map(|composition| composition.selection.clone()));
            let observation = self
                .document
                .tabs
                .get_mut(&tab.tab_id)
                .map_or(Observation::Retained, |tab_state| {
                    recover_observation(tab_state, tab, active_selection.as_ref())
                });
            if observation == Observation::Released {
                release_tabs.push(tab.tab_id.clone());
                continue;
            }
            if let (Some(selection), Some(tab_state)) = (
                recovered_selection.as_ref(),
                self.document.tabs.get_mut(&tab.tab_id),
            ) {
                if observed_owned_selection_matches(tab_state, tab, selection) {
                    tab_state.selection = Some(selection.clone());
                    continue;
                }
            }
            if self
                .document
                .tabs
                .get(&tab.tab_id)
                .is_some_and(|tab_state| tab_state.release_confirmed)
                && self.has_expected_events(&tab.tab_id)
            {
                continue;
            }
            if observation == Observation::PendingPrior {
                let tab_state = self.document.tabs.get_mut(&tab.tab_id).unwrap();
                if !acquiring {
                    tab_state.pending = None;
                } else {
                    let pending = tab_state.pending.clone().unwrap();
                    if let Some(target) = pending_target_label(
                        tab_state,
                        tab,
                        composition,
                        active_selection.as_ref(),
                        &pending,
                    ) {
                        plan_target(
                            &tab.tab_id,
                            &tab.observed_label,
                            target,
                            pending.disposition,
                            tab_state,
                            &mut effects,
                        );
                        continue;
                    }
                    if recovered_selection.is_none() {
                        tab_state.pending = None;
                    }
                }
            }

            if !acquiring {
                let Some(tab_state) = self.document.tabs.get_mut(&tab.tab_id) else {
                    continue;
                };
                if observation != Observation::StaleTarget
                    && observed_is_manual(tab_state, &tab.observed_label)
                {
                    record_manual(tab_state, active_selection.as_ref(), &tab.observed_label);
                    release_tabs.push(tab.tab_id.clone());
                    continue;
                }
                let target = baseline_label(&tab_state.baseline, tab.position);
                if plan_release(
                    &tab.tab_id,
                    &tab.observed_label,
                    target,
                    tab_state,
                    &mut effects,
                ) {
                    release_tabs.push(tab.tab_id.clone());
                }
                continue;
            }

            if !self.document.tabs.contains_key(&tab.tab_id) {
                let Some(composition) = composition else {
                    continue;
                };
                let identity_digest = composition.selection.identity_digest.clone();
                let state = TabState {
                    baseline: capture_baseline(tab),
                    selection: Some(composition.selection.clone()),
                    overrides: BTreeMap::new(),
                    applied: Some(Applied {
                        target_digest: digest_label(&tab.observed_label),
                        source: AppliedSource::Baseline,
                    }),
                    pending: None,
                    stale_target_digest: None,
                    release_confirmed: false,
                };
                self.document.tabs.insert(tab.tab_id.clone(), state);
                let tab_state = self.document.tabs.get_mut(&tab.tab_id).unwrap();
                plan_target(
                    &tab.tab_id,
                    &tab.observed_label,
                    composition.label.clone(),
                    PendingDisposition::Keep {
                        source: AppliedSource::Generated { identity_digest },
                    },
                    tab_state,
                    &mut effects,
                );
                continue;
            }

            let tab_state = self.document.tabs.get_mut(&tab.tab_id).unwrap();
            if let Some(selection) = recovered_selection {
                tab_state.selection = Some(selection.clone());
                let identity_digest = selection.identity_digest;
                if let Some(label) = tab_state.overrides.get(&identity_digest).cloned() {
                    plan_target(
                        &tab.tab_id,
                        &tab.observed_label,
                        label,
                        PendingDisposition::Keep {
                            source: AppliedSource::Override { identity_digest },
                        },
                        tab_state,
                        &mut effects,
                    );
                }
                continue;
            }
            let Some(composition) = composition else {
                tab_state.selection = None;
                let target = baseline_label(&tab_state.baseline, tab.position);
                plan_target(
                    &tab.tab_id,
                    &tab.observed_label,
                    target,
                    PendingDisposition::Keep {
                        source: AppliedSource::Baseline,
                    },
                    tab_state,
                    &mut effects,
                );
                continue;
            };

            tab_state.selection = Some(composition.selection.clone());
            let identity_digest = composition.selection.identity_digest.clone();
            if let Some(label) = tab_state.overrides.get(&identity_digest).cloned() {
                plan_target(
                    &tab.tab_id,
                    &tab.observed_label,
                    label,
                    PendingDisposition::Keep {
                        source: AppliedSource::Override { identity_digest },
                    },
                    tab_state,
                    &mut effects,
                );
            } else {
                plan_target(
                    &tab.tab_id,
                    &tab.observed_label,
                    composition.label.clone(),
                    PendingDisposition::Keep {
                        source: AppliedSource::Generated { identity_digest },
                    },
                    tab_state,
                    &mut effects,
                );
            }
        }

        for tab_id in release_tabs {
            self.document.tabs.remove(&tab_id);
            self.expected_events.remove(&tab_id);
        }
        if self.document.tabs.is_empty() {
            self.document.cleanup_pending = false;
        }
        if self.document != before {
            if let Err(error) = self.persist_document() {
                self.document = before;
                return Err(error);
            }
        }
        Ok(effects)
    }

    pub fn complete_rename(
        &mut self,
        token: &TransitionToken,
        completion: RenameCompletion,
    ) -> Result<(), TabNameError> {
        let before = self.document.clone();
        let Some(tab_state) = self.document.tabs.get_mut(&token.tab_id) else {
            return if completion == RenameCompletion::MissingTab {
                Ok(())
            } else {
                Err(TabNameError::StaleTransition)
            };
        };
        let Some(pending) = tab_state.pending.clone() else {
            return Err(TabNameError::StaleTransition);
        };
        if pending.target_digest != token.target_digest {
            return Err(TabNameError::StaleTransition);
        }
        match completion {
            RenameCompletion::MissingTab => {
                self.document.tabs.remove(&token.tab_id);
            }
            RenameCompletion::Applied => match pending.disposition {
                PendingDisposition::Keep { source } => {
                    tab_state.applied = Some(Applied {
                        target_digest: pending.target_digest,
                        source,
                    });
                    tab_state.pending = None;
                }
                PendingDisposition::Release => {
                    tab_state.applied = Some(Applied {
                        target_digest: pending.target_digest,
                        source: AppliedSource::Baseline,
                    });
                    tab_state.pending = None;
                    tab_state.selection = None;
                    tab_state.release_confirmed = true;
                }
            },
        }
        if self.document.tabs.is_empty() {
            self.document.cleanup_pending = false;
        }
        if let Err(error) = self.persist_document() {
            self.document = before;
            return Err(error);
        }
        match completion {
            RenameCompletion::Applied => self
                .expected_events
                .entry(token.tab_id.clone())
                .or_default()
                .push_back(token.target_digest.clone()),
            RenameCompletion::MissingTab => {
                self.expected_events.remove(&token.tab_id);
            }
        }
        Ok(())
    }

    fn has_expected_events(&self, tab_id: &str) -> bool {
        self.expected_events
            .get(tab_id)
            .is_some_and(|events| !events.is_empty())
    }

    fn consume_expected_event(&mut self, tab_id: &str, digest: &str) -> bool {
        let Some(events) = self.expected_events.get_mut(tab_id) else {
            return false;
        };
        let Some(index) = events.iter().position(|expected| expected == digest) else {
            return false;
        };
        events.remove(index);
        if events.is_empty() {
            self.expected_events.remove(tab_id);
        }
        true
    }

    fn compositions(
        &mut self,
        tabs: &[TabSnapshot],
        panes: &[PaneSnapshot],
    ) -> HashMap<String, Option<Composition>> {
        let live_generations: std::collections::HashSet<_> = panes
            .iter()
            .filter_map(contributor_generation_digest)
            .collect();
        self.retained_components
            .retain(|generation, _| live_generations.contains(generation));

        tabs.iter()
            .map(|tab| (tab.tab_id.clone(), self.composition(tab, panes)))
            .collect()
    }

    fn incomplete_composition_evidence(
        &self,
        tab: &TabSnapshot,
        panes: &[PaneSnapshot],
    ) -> Option<IncompleteCompositionEvidence> {
        let mut identities = Vec::new();
        let mut generations = Vec::new();
        let mut incomplete = false;
        let mut all_identities_known = true;
        for pane in panes.iter().filter(|pane| pane.tab_id == tab.tab_id) {
            let Some(binding_identity) = pane.binding_identity.as_deref() else {
                continue;
            };
            let (agent, identity, contributes) = match &pane.context {
                PaneContext::Unsupported => continue,
                PaneContext::Failed { agent } => {
                    incomplete = true;
                    (agent.as_str(), None, true)
                }
                PaneContext::Supported {
                    agent,
                    identity,
                    display,
                } => {
                    let contributes = match display {
                        DisplayState::Resolved(title) => context_label(title).is_some(),
                        DisplayState::Failed => {
                            let pane_identity = digest_identity("", agent, identity);
                            let generation = contributor_generation_digest(pane)?;
                            let retained = self
                                .retained_components
                                .get(&generation)
                                .is_some_and(|retained| retained.identity_digest == pane_identity);
                            incomplete |= !retained;
                            true
                        }
                        DisplayState::Unresolved => false,
                    };
                    (agent.as_str(), Some(identity.as_str()), contributes)
                }
            };
            if contributes {
                if let Some(identity) = identity {
                    identities.push((agent, identity));
                } else {
                    all_identities_known = false;
                }
                generations.push((
                    pane.pane_id.as_str(),
                    pane.terminal_id.as_str(),
                    agent,
                    binding_identity,
                ));
            }
        }
        if !incomplete || generations.is_empty() {
            return None;
        }
        let identity_digest =
            all_identities_known.then(|| digest_composition(&tab.tab_id, &identities));
        Some(IncompleteCompositionEvidence {
            generation_digest: digest_contributor_generations(&generations),
            identity_digest,
        })
    }

    fn composition(&mut self, tab: &TabSnapshot, panes: &[PaneSnapshot]) -> Option<Composition> {
        struct Contributor<'a> {
            pane_id: &'a str,
            terminal_id: &'a str,
            agent: &'a str,
            binding_identity: &'a [u8],
            identity: &'a str,
            label: String,
        }

        let mut contributors = Vec::new();
        for pane in panes.iter().filter(|pane| pane.tab_id == tab.tab_id) {
            let Some(generation) = contributor_generation_digest(pane) else {
                continue;
            };
            let PaneContext::Supported {
                agent,
                identity,
                display,
            } = &pane.context
            else {
                continue;
            };
            let Some(binding_identity) = pane.binding_identity.as_deref() else {
                continue;
            };
            let pane_identity = digest_identity("", agent, identity);
            let label = match display {
                DisplayState::Resolved(title) => {
                    let label = context_label(title);
                    if let Some(label) = &label {
                        self.retained_components.insert(
                            generation.clone(),
                            RetainedComponent {
                                identity_digest: pane_identity.clone(),
                                label: label.clone(),
                            },
                        );
                    } else {
                        self.retained_components.remove(&generation);
                    }
                    label
                }
                DisplayState::Failed => self
                    .retained_components
                    .get(&generation)
                    .filter(|retained| retained.identity_digest == pane_identity)
                    .map(|retained| retained.label.clone()),
                DisplayState::Unresolved => {
                    self.retained_components.remove(&generation);
                    None
                }
            };
            if let Some(label) = label {
                contributors.push(Contributor {
                    pane_id: &pane.pane_id,
                    terminal_id: &pane.terminal_id,
                    agent,
                    binding_identity,
                    identity,
                    label,
                });
            }
        }
        contributors.first()?;
        let identities: Vec<_> = contributors
            .iter()
            .map(|contributor| (contributor.agent, contributor.identity))
            .collect();
        let generations: Vec<_> = contributors
            .iter()
            .map(|contributor| {
                (
                    contributor.pane_id,
                    contributor.terminal_id,
                    contributor.agent,
                    contributor.binding_identity,
                )
            })
            .collect();
        Some(Composition {
            selection: Selection {
                generation_digest: digest_contributor_generations(&generations),
                identity_digest: digest_composition(&tab.tab_id, &identities),
            },
            label: contributors
                .into_iter()
                .map(|contributor| contributor.label)
                .collect::<Vec<_>>()
                .join(" + "),
        })
    }

    fn persist_document(&self) -> Result<(), TabNameError> {
        if self.document.tabs.is_empty() && !self.document.cleanup_pending {
            self.state_file.remove().map_err(|_| TabNameError::State)
        } else {
            self.state_file
                .persist(&self.document)
                .map_err(|_| TabNameError::State)
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Observation {
    Retained,
    StaleTarget,
    PendingPrior,
    Released,
}

fn recover_observation(
    tab_state: &mut TabState,
    tab: &TabSnapshot,
    selection: Option<&Selection>,
) -> Observation {
    let observed_digest = digest_label(&tab.observed_label);
    if tab_state.stale_target_digest.as_deref() == Some(observed_digest.as_str()) {
        return Observation::StaleTarget;
    }
    if let Some(pending) = tab_state.pending.clone() {
        if observed_digest == pending.target_digest {
            match pending.disposition {
                PendingDisposition::Keep { source } => {
                    tab_state.applied = Some(Applied {
                        target_digest: pending.target_digest,
                        source,
                    });
                    tab_state.pending = None;
                    return Observation::Retained;
                }
                PendingDisposition::Release => return Observation::Released,
            }
        }
        if observed_digest == pending.prior_digest {
            return Observation::PendingPrior;
        }
        record_manual(tab_state, selection, &tab.observed_label);
        return Observation::Retained;
    }
    if observed_is_manual(tab_state, &tab.observed_label) {
        record_manual(tab_state, selection, &tab.observed_label);
    }
    Observation::Retained
}

fn recover_incomplete_selection(
    tab_state: &TabState,
    evidence: &IncompleteCompositionEvidence,
) -> Option<Selection> {
    let persisted = tab_state.selection.as_ref()?;
    let generation_matches = persisted.generation_digest == evidence.generation_digest;
    if !generation_matches
        || evidence
            .identity_digest
            .as_ref()
            .is_some_and(|digest| digest != &persisted.identity_digest)
        || !tab_state_owns_identity(tab_state, &persisted.identity_digest)
    {
        return None;
    }
    Some(Selection {
        generation_digest: evidence.generation_digest.clone(),
        identity_digest: persisted.identity_digest.clone(),
    })
}

fn tab_state_owns_identity(tab_state: &TabState, identity_digest: &str) -> bool {
    tab_state
        .applied
        .as_ref()
        .and_then(|applied| source_identity_digest(&applied.source))
        == Some(identity_digest)
        || tab_state.pending.as_ref().is_some_and(|pending| {
            matches!(
                &pending.disposition,
                PendingDisposition::Keep { source }
                    if source_identity_digest(source) == Some(identity_digest)
            )
        })
}

fn source_identity_digest(source: &AppliedSource) -> Option<&str> {
    match source {
        AppliedSource::Generated { identity_digest }
        | AppliedSource::Override { identity_digest } => Some(identity_digest),
        AppliedSource::Baseline => None,
    }
}

fn observed_owned_selection_matches(
    tab_state: &TabState,
    tab: &TabSnapshot,
    selection: &Selection,
) -> bool {
    tab_state.applied.as_ref().is_some_and(|applied| {
        source_identity_digest(&applied.source) == Some(selection.identity_digest.as_str())
            && applied.target_digest == digest_label(&tab.observed_label)
    })
}

fn pending_target_label(
    tab_state: &TabState,
    tab: &TabSnapshot,
    composition: Option<&Composition>,
    selection: Option<&Selection>,
    pending: &PendingTransition,
) -> Option<String> {
    match &pending.disposition {
        PendingDisposition::Release
        | PendingDisposition::Keep {
            source: AppliedSource::Baseline,
        } => Some(baseline_label(&tab_state.baseline, tab.position)),
        PendingDisposition::Keep {
            source: AppliedSource::Override { identity_digest },
        } => selection
            .filter(|selection| selection.identity_digest == *identity_digest)
            .and_then(|_| tab_state.overrides.get(identity_digest).cloned()),
        PendingDisposition::Keep {
            source: AppliedSource::Generated { identity_digest },
        } => composition
            .filter(|composition| composition.selection.identity_digest == *identity_digest)
            .map(|composition| composition.label.clone()),
    }
}

fn observed_is_manual(tab_state: &TabState, label: &str) -> bool {
    tab_state
        .applied
        .as_ref()
        .is_some_and(|applied| applied.target_digest != digest_label(label))
}

fn record_manual(tab_state: &mut TabState, selection: Option<&Selection>, label: &str) {
    record_manual_label(tab_state, selection, label);
}

fn record_manual_label(tab_state: &mut TabState, selection: Option<&Selection>, label: &str) {
    tab_state.baseline = Baseline::Exact {
        value: label.to_owned(),
    };
    tab_state.pending = None;
    tab_state.release_confirmed = false;
    if let Some(selection) = selection {
        let selection = selection.clone();
        tab_state.selection = Some(selection.clone());
        tab_state
            .overrides
            .insert(selection.identity_digest.clone(), label.to_owned());
        tab_state.applied = Some(Applied {
            target_digest: digest_label(label),
            source: AppliedSource::Override {
                identity_digest: selection.identity_digest,
            },
        });
    } else {
        tab_state.selection = None;
        tab_state.applied = Some(Applied {
            target_digest: digest_label(label),
            source: AppliedSource::Baseline,
        });
    }
}

fn contributor_generation_digest(pane: &PaneSnapshot) -> Option<String> {
    let binding_identity = pane.binding_identity.as_deref()?;
    let agent = match &pane.context {
        PaneContext::Unsupported => return None,
        PaneContext::Failed { agent } | PaneContext::Supported { agent, .. } => agent,
    };
    Some(digest_contributor_generations(&[(
        &pane.pane_id,
        &pane.terminal_id,
        agent,
        binding_identity,
    )]))
}

fn capture_baseline(tab: &TabSnapshot) -> Baseline {
    if tab.observed_label == tab.position.to_string() {
        Baseline::ProbableAuto
    } else {
        Baseline::Exact {
            value: tab.observed_label.clone(),
        }
    }
}

fn baseline_label(baseline: &Baseline, position: usize) -> String {
    match baseline {
        Baseline::Exact { value } => value.clone(),
        Baseline::ProbableAuto => position.to_string(),
    }
}

fn plan_release(
    tab_id: &str,
    observed_label: &str,
    target: String,
    tab_state: &mut TabState,
    effects: &mut Vec<RenameEffect>,
) -> bool {
    if digest_label(observed_label) == digest_label(&target) {
        return true;
    }
    plan_target(
        tab_id,
        observed_label,
        target,
        PendingDisposition::Release,
        tab_state,
        effects,
    );
    false
}

fn plan_target(
    tab_id: &str,
    observed_label: &str,
    target: String,
    disposition: PendingDisposition,
    tab_state: &mut TabState,
    effects: &mut Vec<RenameEffect>,
) {
    let prior_digest = digest_label(observed_label);
    let target_digest = digest_label(&target);
    if prior_digest == target_digest {
        match disposition {
            PendingDisposition::Keep { source } => {
                tab_state.applied = Some(Applied {
                    target_digest,
                    source,
                });
                tab_state.pending = None;
            }
            PendingDisposition::Release => {}
        }
        return;
    }
    tab_state.release_confirmed = false;
    tab_state.pending = Some(PendingTransition {
        prior_digest,
        target_digest: target_digest.clone(),
        disposition,
    });
    effects.push(RenameEffect {
        tab_id: tab_id.to_owned(),
        label: target,
        token: TransitionToken {
            tab_id: tab_id.to_owned(),
            target_digest,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(tab_id: &str, position: usize, label: &str, _ignored_pane_id: &str) -> TabSnapshot {
        TabSnapshot {
            tab_id: tab_id.into(),
            workspace_id: tab_id.split(':').next().unwrap().into(),
            position,
            observed_label: label.into(),
        }
    }

    fn binding_identity(identity: &str) -> Option<Vec<u8>> {
        Some(format!("binding-{identity}").into_bytes())
    }

    fn resolved(pane_id: &str, tab_id: &str, identity: &str, title: &str) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: format!("terminal-{pane_id}"),
            binding_identity: binding_identity(identity),
            tab_id: tab_id.into(),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Resolved(title.into()),
            },
        }
    }

    fn failed(pane_id: &str, tab_id: &str, identity: &str) -> PaneSnapshot {
        failed_with_terminal(pane_id, &format!("terminal-{pane_id}"), tab_id, identity)
    }

    fn failed_with_terminal(
        pane_id: &str,
        terminal_id: &str,
        tab_id: &str,
        identity: &str,
    ) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            binding_identity: binding_identity(identity),
            tab_id: tab_id.into(),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Failed,
            },
        }
    }

    fn failed_without_identity(pane_id: &str, tab_id: &str, binding: &str) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: format!("terminal-{pane_id}"),
            binding_identity: binding_identity(binding),
            tab_id: tab_id.into(),
            context: PaneContext::Failed { agent: "pi".into() },
        }
    }

    fn unresolved(pane_id: &str, tab_id: &str, identity: &str) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: format!("terminal-{pane_id}"),
            binding_identity: binding_identity(identity),
            tab_id: tab_id.into(),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Unresolved,
            },
        }
    }

    fn unsupported(pane_id: &str, tab_id: &str) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: String::new(),
            binding_identity: None,
            tab_id: tab_id.into(),
            context: PaneContext::Unsupported,
        }
    }

    fn failed_aggregate() -> [PaneSnapshot; 2] {
        [
            failed("w1:p1", "w1:t1", "session-a"),
            failed("w1:p2", "w1:t1", "session-b"),
        ]
    }

    fn complete(manager: &mut TabNameManager, effect: &RenameEffect) {
        manager
            .complete_rename(effect.token(), RenameCompletion::Applied)
            .unwrap();
    }

    fn composition_a() -> [PaneSnapshot; 2] {
        [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ]
    }

    fn incomplete_composition_a() -> [PaneSnapshot; 2] {
        [
            failed_without_identity("w1:p1", "w1:t1", "session-a"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ]
    }

    fn composition_b() -> [PaneSnapshot; 1] {
        [resolved("w1:p3", "w1:t1", "session-c", "Gamma")]
    }

    fn manager_with_composition_overrides(
        state_dir: &Path,
        socket: &Path,
        now: Instant,
    ) -> TabNameManager {
        let mut manager = TabNameManager::load(state_dir, socket).unwrap();
        let composition_a = composition_a();
        let acquired = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline-a", "w1:p1")],
                &composition_a,
                now,
            )
            .unwrap();
        complete(&mut manager, &acquired[0]);
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "manual-a", "w1:p1")],
                    &composition_a,
                    now,
                )
                .unwrap()
                .is_empty()
        );

        let composition_b = composition_b();
        let switched = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "manual-a", "w1:p3")],
                &composition_b,
                now,
            )
            .unwrap();
        assert_eq!(switched[0].label(), "Gamma");
        complete(&mut manager, &switched[0]);
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "manual-b", "w1:p3")],
                    &composition_b,
                    now,
                )
                .unwrap()
                .is_empty()
        );
        manager
    }

    #[test]
    fn snapshot_need_is_exposed_without_revealing_state_contents() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-needs-snapshot.sock")).unwrap();
        assert!(!manager.needs_snapshot(false));
        assert!(manager.needs_snapshot(true));

        let effect = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "1", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                Instant::now(),
            )
            .unwrap()
            .remove(0);
        assert!(manager.needs_snapshot(false));
        manager
            .complete_rename(effect.token(), RenameCompletion::MissingTab)
            .unwrap();
        assert!(!manager.needs_snapshot(false));
    }

    #[test]
    fn disable_release_ack_keeps_state_for_queued_manual_event() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-release-ack.sock")).unwrap();
        let now = Instant::now();
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Generated")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        let cleanup = manager
            .reconcile(false, &[tab("w1:t1", 1, "Generated", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &cleanup[0]);

        let plugin_snapshot = [tab("w1:t1", 1, "baseline", "w1:p1")];
        manager
            .observe_renames(
                &[TabRenameObservation {
                    tab_id: "w1:t1".into(),
                    label: "manual-after-ack".into(),
                }],
                &plugin_snapshot,
                &panes,
            )
            .unwrap();
        let restored = manager
            .reconcile(false, &plugin_snapshot, &panes, now)
            .unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label(), "manual-after-ack");
    }

    #[test]
    fn disable_cleanup_restores_manual_event_over_stale_plugin_target() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-event-disable.sock")).unwrap();
        let now = Instant::now();
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Generated")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);

        let stale_snapshot = [tab("w1:t1", 1, "Generated", "w1:p1")];
        manager
            .observe_renames(
                &[TabRenameObservation {
                    tab_id: "w1:t1".into(),
                    label: "manual-race".into(),
                }],
                &stale_snapshot,
                &panes,
            )
            .unwrap();
        let cleanup = manager
            .reconcile(false, &stale_snapshot, &panes, now)
            .unwrap();

        assert_eq!(cleanup.len(), 1);
        assert_eq!(cleanup[0].label(), "manual-race");
    }

    #[test]
    fn delayed_old_plugin_event_is_not_classified_as_manual() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-delayed-event.sock")).unwrap();
        let now = Instant::now();
        let first = [resolved("w1:p1", "w1:t1", "session-a", "Generated one")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &first, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        let second = [resolved("w1:p1", "w1:t1", "session-a", "Generated two")];
        let update = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Generated one", "w1:p1")],
                &second,
                now,
            )
            .unwrap();
        complete(&mut manager, &update[0]);

        let current = [tab("w1:t1", 1, "Generated two", "w1:p1")];
        manager
            .observe_renames(
                &[TabRenameObservation {
                    tab_id: "w1:t1".into(),
                    label: "Generated one".into(),
                }],
                &current,
                &second,
            )
            .unwrap();

        assert!(
            manager
                .reconcile(true, &current, &second, now)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn queued_manual_event_survives_plugin_rename_race() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-event-race.sock")).unwrap();
        let now = Instant::now();
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Generated")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);

        let manual = "manual won the race";
        let overwritten_snapshot = [tab("w1:t1", 1, "Generated", "w1:p1")];
        manager
            .observe_renames(
                &[TabRenameObservation {
                    tab_id: "w1:t1".into(),
                    label: manual.into(),
                }],
                &overwritten_snapshot,
                &panes,
            )
            .unwrap();
        let restored = manager
            .reconcile(true, &overwritten_snapshot, &panes, now)
            .unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label(), manual);
    }

    #[test]
    fn missing_or_closed_tab_discards_owned_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-close.sock")).unwrap();
        let now = Instant::now();
        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "1", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                now,
            )
            .unwrap();
        manager
            .complete_rename(effects[0].token(), RenameCompletion::MissingTab)
            .unwrap();
        assert!(manager.document.tabs.is_empty());
        assert!(!manager.state_file.path().exists());

        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "1", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                now,
            )
            .unwrap();
        complete(&mut manager, &effects[0]);
        assert!(manager.reconcile(true, &[], &[], now).unwrap().is_empty());
        assert!(manager.document.tabs.is_empty());
        assert!(!manager.state_file.path().exists());
    }

    #[test]
    fn finalization_failure_keeps_pending_transition_retryable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-finalize-failure.sock"))
                .unwrap();
        let effect = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "1", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                Instant::now(),
            )
            .unwrap()
            .remove(0);
        let directory = temp.path().join("tab-name");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();

        assert!(matches!(
            manager.complete_rename(effect.token(), RenameCompletion::Applied),
            Err(TabNameError::State)
        ));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        manager
            .complete_rename(effect.token(), RenameCompletion::Applied)
            .unwrap();
    }

    #[test]
    fn persistence_failure_returns_no_rename_effect() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-write-failure.sock")).unwrap();
        let directory = temp.path().join("tab-name");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();

        let result = manager.reconcile(
            true,
            &[tab("w1:t1", 1, "1", "w1:p1")],
            &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
            Instant::now(),
        );

        assert!(matches!(result, Err(TabNameError::State)));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn unexpected_label_during_pending_transition_becomes_manual_override() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-pending-manual.sock")).unwrap();
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();

        let manual = "manual during pending";
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p1")], &panes, now,)
                .unwrap()
                .is_empty()
        );
        let reordered = [
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
        ];
        let generated = manager
            .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p2")], &reordered, now)
            .unwrap();
        assert_eq!(generated[0].label(), "Beta + Alpha");
    }

    #[test]
    fn disable_cleanup_preserves_offline_manual_label_and_resumes_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-disable.sock");
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t2", "session-b", "Beta"),
        ];
        let tabs = [
            tab("w1:t1", 1, "first baseline", "w1:p1"),
            tab("w1:t2", 2, "second baseline", "w1:p2"),
        ];
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let acquired = manager.reconcile(true, &tabs, &panes, now).unwrap();
        assert_eq!(acquired.len(), 2);
        for effect in &acquired {
            complete(&mut manager, effect);
        }

        let disabled_tabs = [
            tab("w1:t1", 1, "Alpha", "w1:p1"),
            tab("w1:t2", 2, "offline manual", "w1:p2"),
        ];
        let cleanup = manager
            .reconcile(false, &disabled_tabs, &panes, now)
            .unwrap();
        assert_eq!(cleanup.len(), 1);
        assert_eq!(cleanup[0].tab_id(), "w1:t1");
        assert_eq!(cleanup[0].label(), "first baseline");
        assert!(manager.document.cleanup_pending);
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let retried = restarted
            .reconcile(false, &disabled_tabs, &panes, now)
            .unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].label(), "first baseline");
        complete(&mut restarted, &retried[0]);
        restarted.reset_event_expectations();
        let confirmed_tabs = [
            tab("w1:t1", 1, "first baseline", "w1:p1"),
            tab("w1:t2", 2, "offline manual", "w1:p2"),
        ];
        assert!(
            restarted
                .reconcile(false, &confirmed_tabs, &panes, now)
                .unwrap()
                .is_empty()
        );
        assert!(restarted.document.tabs.is_empty());
        assert!(!restarted.state_file.path().exists());
    }

    #[test]
    fn pane_move_keeps_manual_suppression_tab_local() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-move.sock")).unwrap();
        let now = Instant::now();
        let original = [resolved("w1:p1", "w1:t1", "session-a", "Alpha")];
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "source baseline", "w1:p1")],
                &original,
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);
        let manual = "manual only in source";
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p1")], &original, now,)
                .unwrap()
                .is_empty()
        );

        let moved = [resolved("w2:p1", "w2:t1", "session-a", "Alpha")];
        let effects = manager
            .reconcile(
                true,
                &[
                    tab("w1:t1", 1, manual, "w1:p-shell"),
                    tab("w2:t1", 1, "destination baseline", "w2:p1"),
                ],
                &moved,
                now,
            )
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].tab_id(), "w2:t1");
        assert_eq!(effects[0].label(), "Alpha");
        assert!(manager.document.tabs.contains_key("w1:t1"));
        assert!(manager.document.tabs["w1:t1"].selection.is_none());
    }

    #[test]
    fn release_pending_finishes_when_reordered_baseline_matches_prior_label() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-release-reorder.sock");
        let now = Instant::now();
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "1", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "2")],
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);
        let release = manager
            .reconcile(true, &[tab("w1:t1", 1, "2", "w1:p-shell")], &[], now)
            .unwrap();
        assert_eq!(release[0].label(), "1");
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        assert!(
            restarted
                .reconcile(true, &[tab("w1:t1", 2, "2", "w1:p-shell")], &[], now)
                .unwrap()
                .is_empty()
        );
        assert!(restarted.document.tabs["w1:t1"].selection.is_none());
    }

    #[test]
    fn probable_auto_tracks_reorder_but_manual_numeric_baseline_stays_exact() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-position.sock")).unwrap();
        let now = Instant::now();
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "1", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);

        let restored = manager
            .reconcile(true, &[tab("w1:t1", 3, "Alpha", "w1:p-shell")], &[], now)
            .unwrap();
        assert_eq!(restored[0].label(), "3");
        complete(&mut manager, &restored[0]);
        manager.reset_event_expectations();
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 3, "3", "w1:p-shell")], &[], now)
                .unwrap()
                .is_empty()
        );
        assert!(manager.document.tabs["w1:t1"].selection.is_none());

        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 3, "3", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 3, "1", "w1:p1")],
                    &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                    now,
                )
                .unwrap()
                .is_empty()
        );
        let exact = manager
            .reconcile(true, &[tab("w1:t1", 4, "1", "w1:p-shell")], &[], now)
            .unwrap();
        assert!(exact.is_empty());
        assert!(manager.document.tabs["w1:t1"].selection.is_none());
    }

    #[test]
    fn known_failed_identity_retains_its_prior_component() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-failed-component.sock"))
                .unwrap();
        let now = Instant::now();
        let initial_panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p1")],
                &initial_panes,
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);

        let failed = [
            failed("w1:p1", "w1:t1", "session-a"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta updated"),
        ];
        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha + Beta", "w1:p2")],
                &failed,
                now,
            )
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "Alpha + Beta updated");
    }

    #[test]
    fn newly_failed_pane_without_prior_component_is_omitted_from_partial_aggregate() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-new-failed-partial.sock"))
                .unwrap();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            failed("w1:p2", "w1:t1", "session-b"),
        ];

        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p1")],
                &panes,
                Instant::now(),
            )
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "Alpha");
    }

    #[test]
    fn failed_pane_that_was_previously_untitled_is_omitted_from_partial_aggregate() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager = TabNameManager::load(
            temp.path(),
            Path::new("/tmp/herdr-untitled-failed-partial.sock"),
        )
        .unwrap();
        let now = Instant::now();
        let initial_panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            unresolved("w1:p2", "w1:t1", "session-b"),
        ];
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p1")],
                &initial_panes,
                now,
            )
            .unwrap();
        assert_eq!(initial[0].label(), "Alpha");
        complete(&mut manager, &initial[0]);

        let failed_panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            failed("w1:p2", "w1:t1", "session-b"),
        ];
        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p1")],
                &failed_panes,
                now,
            )
            .unwrap();

        assert!(effects.is_empty());
        assert_eq!(
            manager.document.tabs["w1:t1"]
                .selection
                .as_ref()
                .unwrap()
                .identity_digest,
            digest_identity("w1:t1", "pi", "session-a")
        );
    }

    #[test]
    fn terminal_replacement_does_not_retain_failed_component() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager = TabNameManager::load(
            temp.path(),
            Path::new("/tmp/herdr-generation-replacement.sock"),
        )
        .unwrap();
        let now = Instant::now();
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Alpha")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);

        let replacement = [failed_with_terminal(
            "w1:p1",
            "replacement-terminal",
            "w1:t1",
            "session-a",
        )];
        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p1")],
                &replacement,
                now,
            )
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "baseline");
    }

    #[test]
    fn legacy_plaintext_pane_anchor_fails_safe_after_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-legacy-pane-anchor.sock");
        let now = Instant::now();
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Alpha")];
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        manager
            .document
            .tabs
            .get_mut("w1:t1")
            .unwrap()
            .selection
            .as_mut()
            .unwrap()
            .generation_digest = "w1:p1".into();
        manager.persist_document().unwrap();
        drop(manager);

        let mut failed = failed("w1:p1", "w1:t1", "session-a");
        failed.terminal_id = "replacement-terminal".into();
        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let effects = restarted
            .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p1")], &[failed], now)
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "baseline");
    }

    #[test]
    fn restart_holds_finalized_override_when_full_composition_is_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-incomplete-override.sock");
        let now = Instant::now();
        let mut manager = manager_with_composition_overrides(temp.path(), socket, now);
        let composition_a = composition_a();
        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "manual-b", "w1:p1")],
                &composition_a,
                now,
            )
            .unwrap();
        assert_eq!(restored[0].label(), "manual-a");
        complete(&mut manager, &restored[0]);
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let incomplete_a = incomplete_composition_a();
        assert!(
            restarted
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "manual-a", "w1:p1")],
                    &incomplete_a,
                    now,
                )
                .unwrap()
                .is_empty()
        );

        let composition_b = composition_b();
        let independent = restarted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "manual-a", "w1:p3")],
                &composition_b,
                now,
            )
            .unwrap();
        assert_eq!(independent[0].label(), "manual-b");
    }

    #[test]
    fn manual_event_during_incomplete_failure_is_attributed_to_full_composition() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-incomplete-manual-event.sock");
        let now = Instant::now();
        let mut manager = manager_with_composition_overrides(temp.path(), socket, now);
        let initial_a = composition_a();
        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "manual-b", "w1:p1")],
                &initial_a,
                now,
            )
            .unwrap();
        complete(&mut manager, &restored[0]);
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let incomplete_a = incomplete_composition_a();
        let renamed_tabs = [tab("w1:t1", 1, "manual-a-new", "w1:p1")];
        restarted
            .observe_renames(
                &[TabRenameObservation {
                    tab_id: "w1:t1".into(),
                    label: "manual-a-new".into(),
                }],
                &renamed_tabs,
                &incomplete_a,
            )
            .unwrap();
        assert!(
            restarted
                .reconcile(true, &renamed_tabs, &incomplete_a, now)
                .unwrap()
                .is_empty()
        );

        let recovered_a = composition_a();
        assert!(
            restarted
                .reconcile(true, &renamed_tabs, &recovered_a, now)
                .unwrap()
                .is_empty()
        );

        let partial_a = [resolved("w1:p2", "w1:t1", "session-b", "Beta")];
        let partial = restarted
            .reconcile(true, &renamed_tabs, &partial_a, now)
            .unwrap();
        assert_eq!(partial[0].label(), "Beta");
        complete(&mut restarted, &partial[0]);

        let composition_b = composition_b();
        let independent = restarted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta", "w1:p3")],
                &composition_b,
                now,
            )
            .unwrap();
        assert_eq!(independent[0].label(), "manual-b");
    }

    #[test]
    fn restart_finalizes_rpc_applied_pending_override_for_incomplete_composition() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-incomplete-pending-override.sock");
        let now = Instant::now();
        let mut manager = manager_with_composition_overrides(temp.path(), socket, now);
        let composition_a = composition_a();
        let pending = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "manual-b", "w1:p1")],
                &composition_a,
                now,
            )
            .unwrap();
        assert_eq!(pending[0].label(), "manual-a");
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let incomplete_a = incomplete_composition_a();
        assert!(
            restarted
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "manual-a", "w1:p1")],
                    &incomplete_a,
                    now,
                )
                .unwrap()
                .is_empty()
        );
        assert!(restarted.document.tabs["w1:t1"].pending.is_none());
    }

    #[test]
    fn restart_holds_applied_generated_label_when_known_composition_first_read_fails() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-applied-failed-restart.sock");
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        drop(manager);

        let failed = failed_aggregate();
        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();

        assert!(
            restarted
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "Alpha + Beta", "w1:p1")],
                    &failed,
                    now,
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn restart_holds_rpc_applied_pending_label_when_known_composition_first_read_fails() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pending-failed-restart.sock");
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        assert_eq!(initial[0].label(), "Alpha + Beta");
        drop(manager);

        let failed = failed_aggregate();
        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();

        assert!(
            restarted
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "Alpha + Beta", "w1:p1")],
                    &failed,
                    now,
                )
                .unwrap()
                .is_empty()
        );
        assert!(restarted.document.tabs["w1:t1"].pending.is_none());
    }

    #[test]
    fn generated_pending_does_not_block_new_identity_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-generated-retry.sock");
        let now = Instant::now();
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let old_a = [resolved("w1:p1", "w1:t1", "session-a", "Alpha old")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &old_a, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        let new_a = [resolved("w1:p1", "w1:t1", "session-a", "Alpha new")];
        let pending = manager
            .reconcile(true, &[tab("w1:t1", 1, "Alpha old", "w1:p1")], &new_a, now)
            .unwrap();
        assert_eq!(pending[0].label(), "Alpha new");
        drop(manager);

        let b = [resolved("w1:p2", "w1:t1", "session-b", "Beta")];
        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let effects = restarted
            .reconcile(true, &[tab("w1:t1", 1, "Alpha old", "w1:p2")], &b, now)
            .unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "Beta");
    }

    #[test]
    fn unresolved_identity_baseline_transition_retries_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-unresolved-retry.sock");
        let now = Instant::now();
        let a = [resolved("w1:p1", "w1:t1", "session-a", "Alpha")];
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &a, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        let b = [failed("w1:p2", "w1:t1", "session-b")];
        let pending = manager
            .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p2")], &b, now)
            .unwrap();
        assert_eq!(pending[0].label(), "baseline");
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let retried = restarted
            .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p2")], &b, now)
            .unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].label(), "baseline");
    }

    #[test]
    fn session_failure_retains_but_changed_unresolved_identity_restores_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-transition.sock")).unwrap();
        let now = Instant::now();
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p1")],
                &[resolved("w1:p1", "w1:t1", "session-a", "Alpha")],
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);

        let failed_panes = [failed("w1:p1", "w1:t1", "session-a")];
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "Alpha", "w1:p1")],
                    &failed_panes,
                    now,
                )
                .unwrap()
                .is_empty()
        );

        let unresolved = [
            failed_panes.into_iter().next().unwrap(),
            unresolved("w1:p2", "w1:t1", "session-b"),
        ];
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p2")], &unresolved, now,)
                .unwrap()
                .is_empty()
        );

        let resolved_b = [
            failed("w1:p1", "w1:t1", "session-a"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let aggregate = manager
            .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p2")], &resolved_b, now)
            .unwrap();
        assert_eq!(aggregate[0].label(), "Alpha + Beta");
    }

    #[test]
    fn unsupported_panes_are_omitted_from_the_aggregate() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-shell.sock")).unwrap();
        let now = Instant::now();
        let initial_panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p2")],
                &initial_panes,
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);

        let shell = unsupported("w1:p3", "w1:t1");
        let updated_panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta updated"),
            shell,
        ];
        let updated = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha + Beta", "w1:p3")],
                &updated_panes,
                now,
            )
            .unwrap();
        assert_eq!(updated[0].label(), "Alpha + Beta updated");
        complete(&mut manager, &updated[0]);

        let remaining = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            unsupported("w1:p3", "w1:t1"),
        ];
        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha + Beta updated", "w1:p3")],
                &remaining,
                now,
            )
            .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label(), "Alpha");
    }

    #[test]
    fn manual_rename_without_a_composition_updates_only_the_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-empty-manual.sock")).unwrap();
        let now = Instant::now();
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Alpha")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);

        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, "manual-a", "w1:p1")], &panes, now,)
                .unwrap()
                .is_empty()
        );
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, "manual-a", "w1:p-shell")], &[], now,)
                .unwrap()
                .is_empty()
        );
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, "empty manual", "w1:p-shell")],
                    &[],
                    now,
                )
                .unwrap()
                .is_empty()
        );

        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "empty manual", "w1:p1")],
                &panes,
                now,
            )
            .unwrap();
        assert_eq!(restored[0].label(), "manual-a");
    }

    #[test]
    fn manual_override_is_scoped_to_the_ordered_composition() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-manual.sock")).unwrap();
        let now = Instant::now();
        let original = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let initial = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p1")],
                &original,
                now,
            )
            .unwrap();
        complete(&mut manager, &initial[0]);

        let manual = "a deliberately very long manual label";
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p2")], &original, now,)
                .unwrap()
                .is_empty()
        );

        let reordered = [
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
        ];
        let generated = manager
            .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p1")], &reordered, now)
            .unwrap();
        assert_eq!(generated[0].label(), "Beta + Alpha");
        complete(&mut manager, &generated[0]);

        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta + Alpha", "w1:p2")],
                &original,
                now,
            )
            .unwrap();
        assert_eq!(restored[0].label(), manual);
    }

    #[test]
    fn restart_classifies_pending_neither_as_current_identity_manual_override() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-neither.sock");
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        drop(manager);

        let manual = "manual after restart";
        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        assert!(
            restarted
                .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p1")], &panes, now,)
                .unwrap()
                .is_empty()
        );
        drop(restarted);

        let mut persisted = TabNameManager::load(temp.path(), socket).unwrap();
        let reordered = [
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
        ];
        let generated = persisted
            .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p2")], &reordered, now)
            .unwrap();
        assert_eq!(generated[0].label(), "Beta + Alpha");
        complete(&mut persisted, &generated[0]);
        let restored = persisted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta + Alpha", "w1:p1")],
                &panes,
                now,
            )
            .unwrap();
        assert_eq!(restored[0].label(), manual);
    }

    #[test]
    fn recovers_pending_transition_from_prior_or_target_digest() {
        let socket = Path::new("/tmp/herdr-recovery.sock");
        let prior_temp = tempfile::tempdir().unwrap();
        let tabs = [tab("w1:t1", 1, "1", "w1:p1")];
        let panes = [resolved("w1:p1", "w1:t1", "session-a", "Alpha context")];
        let mut manager = TabNameManager::load(prior_temp.path(), socket).unwrap();
        let first = manager
            .reconcile(true, &tabs, &panes, Instant::now())
            .unwrap();
        assert_eq!(first[0].label(), "Alpha context");
        drop(manager);

        let mut restarted = TabNameManager::load(prior_temp.path(), socket).unwrap();
        let retried = restarted
            .reconcile(true, &tabs, &panes, Instant::now())
            .unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].label(), "Alpha context");

        let target_temp = tempfile::tempdir().unwrap();
        let mut manager = TabNameManager::load(target_temp.path(), socket).unwrap();
        manager
            .reconcile(true, &tabs, &panes, Instant::now())
            .unwrap();
        drop(manager);
        let mut restarted = TabNameManager::load(target_temp.path(), socket).unwrap();
        let applied_tabs = [tab("w1:t1", 1, "Alpha context", "w1:p1")];
        assert!(
            restarted
                .reconcile(true, &applied_tabs, &panes, Instant::now())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn duplicate_components_are_retained_without_an_aggregate_cap() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-duplicates.sock")).unwrap();
        let title = "abcdefghijklmnopqrst";
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", title),
            resolved("w1:p2", "w1:t1", "session-b", title),
        ];

        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p1")],
                &panes,
                Instant::now(),
            )
            .unwrap();

        assert_eq!(
            effects[0].label(),
            "abcdefghijklmnopqrst + abcdefghijklmnopqrst"
        );
    }

    #[test]
    fn resolved_panes_form_an_ordered_aggregate_independent_of_focus() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-aggregate.sock")).unwrap();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];

        let effects = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p2")],
                &panes,
                Instant::now(),
            )
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "Alpha + Beta");
    }

    #[test]
    fn captures_baseline_before_planning_a_bounded_generated_label() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-test.sock")).unwrap();
        let tabs = [tab("w1:t1", 1, "1", "w1:p1")];
        let panes = [resolved(
            "w1:p1",
            "w1:t1",
            "session-private-1",
            "a generated context title that is long",
        )];

        let effects = manager
            .reconcile(true, &tabs, &panes, Instant::now())
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].tab_id(), "w1:t1");
        assert_eq!(effects[0].label(), "a generated context…");
    }
}
