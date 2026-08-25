mod state;

use crate::text::tab_label;
use state::{
    Applied, AppliedSource, Baseline, PendingDisposition, PendingTransition, PersistedState,
    Selection, StateFile, TabState, digest_identity, digest_label,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};
use thiserror::Error;

pub const FOCUS_DEBOUNCE: Duration = Duration::from_millis(150);

pub struct TabSnapshot {
    pub tab_id: String,
    pub workspace_id: String,
    pub position: usize,
    pub observed_label: String,
    pub focused_pane_id: String,
}

pub struct PaneSnapshot {
    pub pane_id: String,
    pub tab_id: String,
    pub context: PaneContext,
}

pub struct TabRenameObservation {
    pub tab_id: String,
    pub label: String,
}

pub enum PaneContext {
    Unsupported,
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
    focus_deadlines: HashMap<String, Instant>,
    expected_events: HashMap<String, VecDeque<String>>,
}

impl TabNameManager {
    pub fn load(state_dir: &Path, socket_path: &Path) -> Result<Self, TabNameError> {
        let (state_file, document) =
            StateFile::open(state_dir, socket_path).map_err(|_| TabNameError::State)?;
        Ok(Self {
            state_file,
            document,
            focus_deadlines: HashMap::new(),
            expected_events: HashMap::new(),
        })
    }

    pub fn reset_event_expectations(&mut self) {
        self.expected_events.clear();
    }

    pub fn note_focus(&mut self, tab_id: &str, now: Instant) {
        self.focus_deadlines
            .insert(tab_id.to_owned(), now + FOCUS_DEBOUNCE);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.focus_deadlines.values().copied().min()
    }

    pub fn defer_due_focus(&mut self, now: Instant) {
        for deadline in self.focus_deadlines.values_mut() {
            if *deadline <= now {
                *deadline = now + FOCUS_DEBOUNCE;
            }
        }
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
        let tabs_by_id: HashMap<_, _> = tabs.iter().map(|tab| (tab.tab_id.as_str(), tab)).collect();
        for observation in observations {
            let observed_digest = digest_label(&observation.label);
            if self.consume_expected_event(&observation.tab_id, &observed_digest) {
                continue;
            }
            let Some(tab) = tabs_by_id.get(observation.tab_id.as_str()) else {
                continue;
            };
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
            record_manual_label(tab_state, tab, panes, &observation.label);
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
        now: Instant,
    ) -> Result<Vec<RenameEffect>, TabNameError> {
        let before = self.document.clone();
        let live_tabs: std::collections::HashSet<_> =
            tabs.iter().map(|tab| tab.tab_id.as_str()).collect();
        self.document
            .tabs
            .retain(|tab_id, _| live_tabs.contains(tab_id.as_str()));
        self.focus_deadlines
            .retain(|tab_id, _| live_tabs.contains(tab_id.as_str()));
        self.expected_events
            .retain(|tab_id, _| live_tabs.contains(tab_id.as_str()));

        if !enabled && !self.document.tabs.is_empty() {
            self.document.cleanup_pending = true;
        }
        let acquiring = enabled && !self.document.cleanup_pending;
        let panes_by_id: HashMap<_, _> = panes
            .iter()
            .map(|pane| (pane.pane_id.as_str(), pane))
            .collect();
        let mut effects = Vec::new();
        let mut release_tabs = Vec::new();

        for tab in tabs {
            if !self.focus_deadlines.contains_key(&tab.tab_id) {
                let focused_identity = panes_by_id
                    .get(tab.focused_pane_id.as_str())
                    .and_then(|pane| context_digest(pane, &tab.tab_id));
                let selected_identity = self
                    .document
                    .tabs
                    .get(&tab.tab_id)
                    .and_then(|tab_state| tab_state.selection.as_ref())
                    .map(|selection| selection.identity_digest.as_str());
                if focused_identity.as_deref().is_some()
                    && focused_identity.as_deref() != selected_identity
                    && selected_identity.is_some()
                {
                    self.focus_deadlines
                        .insert(tab.tab_id.clone(), now + FOCUS_DEBOUNCE);
                }
            }
            let focus_blocked = self
                .focus_deadlines
                .get(&tab.tab_id)
                .is_some_and(|deadline| now < *deadline);
            if !focus_blocked {
                self.focus_deadlines.remove(&tab.tab_id);
            }

            let observation = self
                .document
                .tabs
                .get_mut(&tab.tab_id)
                .map_or(Observation::Retained, |tab_state| {
                    recover_observation(tab_state, tab, panes)
                });
            if observation == Observation::Released {
                release_tabs.push(tab.tab_id.clone());
                continue;
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
                    if let Some(target) = pending_target_label(tab_state, tab, panes, &pending) {
                        if matches!(&pending.disposition, PendingDisposition::Release)
                            && digest_label(&target) == digest_label(&tab.observed_label)
                        {
                            release_tabs.push(tab.tab_id.clone());
                            continue;
                        }
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
                    if !pending_generated_focus_changed(&pending, tab, panes)
                        && pending_generated_identity_is_live(&pending, tab, panes)
                    {
                        continue;
                    }
                    tab_state.pending = None;
                }
            }

            if !acquiring {
                let Some(tab_state) = self.document.tabs.get_mut(&tab.tab_id) else {
                    continue;
                };
                if observation != Observation::StaleTarget
                    && observed_is_manual(tab_state, &tab.observed_label)
                {
                    record_manual(tab_state, tab, panes);
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
                if focus_blocked {
                    continue;
                }
                let Some(pane) = panes_by_id.get(tab.focused_pane_id.as_str()) else {
                    continue;
                };
                let Some((identity_digest, title)) = resolved_context(pane, &tab.tab_id) else {
                    continue;
                };
                let Some(label) = tab_label(title) else {
                    continue;
                };
                let baseline = capture_baseline(tab);
                let state = TabState {
                    baseline,
                    selection: Some(Selection {
                        pane_id: pane.pane_id.clone(),
                        identity_digest: identity_digest.clone(),
                    }),
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
                    label,
                    PendingDisposition::Keep {
                        source: AppliedSource::Generated { identity_digest },
                    },
                    tab_state,
                    &mut effects,
                );
                continue;
            }

            let tab_state = self.document.tabs.get_mut(&tab.tab_id).unwrap();
            let selection_changed = if focus_blocked {
                false
            } else {
                adopt_focused_selection(tab_state, tab, &panes_by_id)
            };
            if !selection_is_live(tab_state, &tab.tab_id, panes) {
                tab_state.selection = None;
            }
            let Some(selection) = tab_state.selection.clone() else {
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
            };
            let selected = panes.iter().find(|pane| {
                pane.pane_id == selection.pane_id
                    && pane.tab_id == tab.tab_id
                    && context_digest(pane, &tab.tab_id).as_deref()
                        == Some(selection.identity_digest.as_str())
            });
            let Some(selected) = selected else {
                continue;
            };
            if let Some(label) = tab_state.overrides.get(&selection.identity_digest).cloned() {
                plan_target(
                    &tab.tab_id,
                    &tab.observed_label,
                    label,
                    PendingDisposition::Keep {
                        source: AppliedSource::Override {
                            identity_digest: selection.identity_digest,
                        },
                    },
                    tab_state,
                    &mut effects,
                );
                continue;
            }
            match &selected.context {
                PaneContext::Supported {
                    display: DisplayState::Resolved(title),
                    ..
                } => {
                    if let Some(label) = tab_label(title) {
                        plan_target(
                            &tab.tab_id,
                            &tab.observed_label,
                            label,
                            PendingDisposition::Keep {
                                source: AppliedSource::Generated {
                                    identity_digest: selection.identity_digest,
                                },
                            },
                            tab_state,
                            &mut effects,
                        );
                    }
                }
                PaneContext::Supported {
                    display: DisplayState::Unresolved | DisplayState::Failed,
                    ..
                } if selection_changed => {
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
                }
                PaneContext::Supported { .. } | PaneContext::Unsupported => {}
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
    panes: &[PaneSnapshot],
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
        record_manual(tab_state, tab, panes);
        return Observation::Retained;
    }
    if observed_is_manual(tab_state, &tab.observed_label) {
        record_manual(tab_state, tab, panes);
    }
    Observation::Retained
}

fn pending_generated_focus_changed(
    pending: &PendingTransition,
    tab: &TabSnapshot,
    panes: &[PaneSnapshot],
) -> bool {
    let PendingDisposition::Keep {
        source: AppliedSource::Generated { identity_digest },
    } = &pending.disposition
    else {
        return false;
    };
    panes
        .iter()
        .find(|pane| pane.pane_id == tab.focused_pane_id && pane.tab_id == tab.tab_id)
        .and_then(|pane| context_digest(pane, &tab.tab_id))
        .is_some_and(|focused_identity| focused_identity != *identity_digest)
}

fn pending_generated_identity_is_live(
    pending: &PendingTransition,
    tab: &TabSnapshot,
    panes: &[PaneSnapshot],
) -> bool {
    let PendingDisposition::Keep {
        source: AppliedSource::Generated { identity_digest },
    } = &pending.disposition
    else {
        return false;
    };
    panes.iter().any(|pane| {
        pane.tab_id == tab.tab_id
            && context_digest(pane, &tab.tab_id).as_deref() == Some(identity_digest.as_str())
    })
}

fn pending_target_label(
    tab_state: &TabState,
    tab: &TabSnapshot,
    panes: &[PaneSnapshot],
    pending: &PendingTransition,
) -> Option<String> {
    match &pending.disposition {
        PendingDisposition::Release
        | PendingDisposition::Keep {
            source: AppliedSource::Baseline,
        } => Some(baseline_label(&tab_state.baseline, tab.position)),
        PendingDisposition::Keep {
            source: AppliedSource::Override { identity_digest },
        } => tab_state.overrides.get(identity_digest).cloned(),
        PendingDisposition::Keep {
            source: AppliedSource::Generated { identity_digest },
        } => panes.iter().find_map(|pane| {
            (pane.tab_id == tab.tab_id
                && context_digest(pane, &tab.tab_id).as_deref() == Some(identity_digest.as_str()))
            .then(|| match &pane.context {
                PaneContext::Supported {
                    display: DisplayState::Resolved(title),
                    ..
                } => tab_label(title),
                PaneContext::Supported { .. } | PaneContext::Unsupported => None,
            })
            .flatten()
        }),
    }
}

fn observed_is_manual(tab_state: &TabState, label: &str) -> bool {
    tab_state
        .applied
        .as_ref()
        .is_some_and(|applied| applied.target_digest != digest_label(label))
}

fn record_manual(tab_state: &mut TabState, tab: &TabSnapshot, panes: &[PaneSnapshot]) {
    record_manual_label(tab_state, tab, panes, &tab.observed_label);
}

fn record_manual_label(
    tab_state: &mut TabState,
    tab: &TabSnapshot,
    panes: &[PaneSnapshot],
    label: &str,
) {
    let focused_selection = panes
        .iter()
        .find(|pane| pane.pane_id == tab.focused_pane_id)
        .and_then(|pane| {
            context_digest(pane, &tab.tab_id).map(|identity_digest| Selection {
                pane_id: pane.pane_id.clone(),
                identity_digest,
            })
        });
    let attributed_selection = focused_selection.or_else(|| {
        tab_state
            .selection
            .clone()
            .filter(|_| selection_is_live(tab_state, &tab.tab_id, panes))
    });
    tab_state.baseline = Baseline::Exact {
        value: label.to_owned(),
    };
    tab_state.pending = None;
    tab_state.release_confirmed = false;
    if let Some(selection) = attributed_selection {
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

fn adopt_focused_selection(
    tab_state: &mut TabState,
    tab: &TabSnapshot,
    panes_by_id: &HashMap<&str, &PaneSnapshot>,
) -> bool {
    let Some(pane) = panes_by_id.get(tab.focused_pane_id.as_str()) else {
        return false;
    };
    let Some(identity_digest) = context_digest(pane, &tab.tab_id) else {
        return false;
    };
    let changed = tab_state
        .selection
        .as_ref()
        .is_none_or(|selection| selection.identity_digest != identity_digest);
    tab_state.selection = Some(Selection {
        pane_id: pane.pane_id.clone(),
        identity_digest,
    });
    changed
}

fn selection_is_live(tab_state: &TabState, tab_id: &str, panes: &[PaneSnapshot]) -> bool {
    let Some(selection) = &tab_state.selection else {
        return false;
    };
    panes.iter().any(|pane| {
        pane.pane_id == selection.pane_id
            && pane.tab_id == tab_id
            && context_digest(pane, tab_id).as_deref() == Some(selection.identity_digest.as_str())
    })
}

fn context_digest(pane: &PaneSnapshot, tab_id: &str) -> Option<String> {
    if pane.tab_id != tab_id {
        return None;
    }
    match &pane.context {
        PaneContext::Supported {
            agent, identity, ..
        } => Some(digest_identity(tab_id, agent, identity)),
        PaneContext::Unsupported => None,
    }
}

fn resolved_context<'a>(pane: &'a PaneSnapshot, tab_id: &str) -> Option<(String, &'a str)> {
    match &pane.context {
        PaneContext::Supported {
            agent,
            identity,
            display: DisplayState::Resolved(title),
        } if pane.tab_id == tab_id => Some((digest_identity(tab_id, agent, identity), title)),
        PaneContext::Supported { .. } | PaneContext::Unsupported => None,
    }
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

    fn tab(tab_id: &str, position: usize, label: &str, focused_pane_id: &str) -> TabSnapshot {
        TabSnapshot {
            tab_id: tab_id.into(),
            workspace_id: tab_id.split(':').next().unwrap().into(),
            position,
            observed_label: label.into(),
            focused_pane_id: focused_pane_id.into(),
        }
    }

    fn resolved(pane_id: &str, tab_id: &str, identity: &str, title: &str) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            tab_id: tab_id.into(),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Resolved(title.into()),
            },
        }
    }

    fn complete(manager: &mut TabNameManager, effect: &RenameEffect) {
        manager
            .complete_rename(effect.token(), RenameCompletion::Applied)
            .unwrap();
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
    fn manual_label_during_focus_debounce_belongs_to_current_focused_session() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-focus-manual.sock")).unwrap();
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);

        manager.note_focus("w1:t1", now);
        let manual = "manual for focused B";
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, manual, "w1:p2")],
                    &panes,
                    now + Duration::from_millis(10),
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, manual, "w1:p2")],
                    &panes,
                    now + FOCUS_DEBOUNCE,
                )
                .unwrap()
                .is_empty()
        );

        manager.note_focus("w1:t1", now + Duration::from_millis(200));
        let alpha = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, manual, "w1:p1")],
                &panes,
                now + Duration::from_millis(350),
            )
            .unwrap();
        assert_eq!(alpha[0].label(), "Alpha");
        complete(&mut manager, &alpha[0]);

        manager.note_focus("w1:t1", now + Duration::from_millis(400));
        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p2")],
                &panes,
                now + Duration::from_millis(550),
            )
            .unwrap();
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
        manager.note_focus("w1:t1", now);
        let beta = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, manual, "w1:p2")],
                &panes,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();
        assert_eq!(beta[0].label(), "Beta");
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
        assert!(!manager.document.tabs.contains_key("w1:t1"));
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
        assert!(restarted.document.tabs.is_empty());
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
        assert!(manager.document.tabs.is_empty());

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
        assert!(manager.document.tabs.is_empty());
    }

    #[test]
    fn generated_pending_does_not_block_focused_identity_when_old_identity_is_failed() {
        let temp = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-generated-focused.sock");
        let now = Instant::now();
        let mut manager = TabNameManager::load(temp.path(), socket).unwrap();
        let old_a = [resolved("w1:p1", "w1:t1", "session-a", "Alpha old")];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &old_a, now)
            .unwrap();
        complete(&mut manager, &initial[0]);
        let new_a = [resolved("w1:p1", "w1:t1", "session-a", "Alpha new")];
        manager
            .reconcile(true, &[tab("w1:t1", 1, "Alpha old", "w1:p1")], &new_a, now)
            .unwrap();
        drop(manager);

        let panes = [
            PaneSnapshot {
                pane_id: "w1:p1".into(),
                tab_id: "w1:t1".into(),
                context: PaneContext::Supported {
                    agent: "pi".into(),
                    identity: "session-a".into(),
                    display: DisplayState::Failed,
                },
            },
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        assert!(
            restarted
                .reconcile(true, &[tab("w1:t1", 1, "Alpha old", "w1:p2")], &panes, now)
                .unwrap()
                .is_empty()
        );
        let effects = restarted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha old", "w1:p2")],
                &panes,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].label(), "Beta");
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
        let baseline = restarted
            .reconcile(true, &[tab("w1:t1", 1, "Alpha old", "w1:p2")], &b, now)
            .unwrap();
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].label(), "baseline");
        complete(&mut restarted, &baseline[0]);
        restarted.reset_event_expectations();
        let effects = restarted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p2")],
                &b,
                now + FOCUS_DEBOUNCE,
            )
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
        let b = [PaneSnapshot {
            pane_id: "w1:p2".into(),
            tab_id: "w1:t1".into(),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: "session-b".into(),
                display: DisplayState::Failed,
            },
        }];
        manager.note_focus("w1:t1", now);
        let pending = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p2")],
                &b,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();
        assert_eq!(pending[0].label(), "baseline");
        drop(manager);

        let mut restarted = TabNameManager::load(temp.path(), socket).unwrap();
        let retried = restarted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p2")],
                &b,
                now + FOCUS_DEBOUNCE,
            )
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

        let failed = [PaneSnapshot {
            pane_id: "w1:p1".into(),
            tab_id: "w1:t1".into(),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: "session-a".into(),
                display: DisplayState::Failed,
            },
        }];
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p1")], &failed, now,)
                .unwrap()
                .is_empty()
        );

        let unresolved = [
            failed.into_iter().next().unwrap(),
            PaneSnapshot {
                pane_id: "w1:p2".into(),
                tab_id: "w1:t1".into(),
                context: PaneContext::Supported {
                    agent: "pi".into(),
                    identity: "session-b".into(),
                    display: DisplayState::Unresolved,
                },
            },
        ];
        manager.note_focus("w1:t1", now);
        let baseline = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p2")],
                &unresolved,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();
        assert_eq!(baseline[0].label(), "baseline");
        complete(&mut manager, &baseline[0]);

        let resolved_b = [resolved("w1:p2", "w1:t1", "session-b", "Beta")];
        let beta = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "baseline", "w1:p2")],
                &resolved_b,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();
        assert_eq!(beta[0].label(), "Beta");
    }

    #[test]
    fn non_agent_focus_retains_only_the_last_selected_live_session() {
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

        let shell = PaneSnapshot {
            pane_id: "w1:p3".into(),
            tab_id: "w1:t1".into(),
            context: PaneContext::Unsupported,
        };
        let updated_panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta updated"),
            shell,
        ];
        let updated = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta", "w1:p3")],
                &updated_panes,
                now,
            )
            .unwrap();
        assert_eq!(updated[0].label(), "Beta updated");
        complete(&mut manager, &updated[0]);

        let remaining = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            PaneSnapshot {
                pane_id: "w1:p3".into(),
                tab_id: "w1:t1".into(),
                context: PaneContext::Unsupported,
            },
        ];
        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta updated", "w1:p3")],
                &remaining,
                now,
            )
            .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label(), "baseline");
    }

    #[test]
    fn snapshot_detected_focus_change_starts_its_own_debounce() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-snapshot-focus.sock")).unwrap();
        let now = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, now)
            .unwrap();
        complete(&mut manager, &initial[0]);

        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, "Alpha", "w1:p2")], &panes, now,)
                .unwrap()
                .is_empty()
        );
        assert_eq!(manager.next_deadline(), Some(now + FOCUS_DEBOUNCE));
        let beta = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Alpha", "w1:p2")],
                &panes,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();
        assert_eq!(beta[0].label(), "Beta");
    }

    #[test]
    fn manual_override_is_identity_scoped_and_focus_debounced() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager =
            TabNameManager::load(temp.path(), Path::new("/tmp/herdr-manual.sock")).unwrap();
        let start = Instant::now();
        let panes = [
            resolved("w1:p1", "w1:t1", "session-a", "Alpha"),
            resolved("w1:p2", "w1:t1", "session-b", "Beta"),
        ];
        let initial = manager
            .reconcile(true, &[tab("w1:t1", 1, "baseline", "w1:p1")], &panes, start)
            .unwrap();
        complete(&mut manager, &initial[0]);

        let manual = "a deliberately very long manual label";
        assert!(
            manager
                .reconcile(true, &[tab("w1:t1", 1, manual, "w1:p1")], &panes, start,)
                .unwrap()
                .is_empty()
        );

        manager.note_focus("w1:t1", start);
        manager.note_focus("w1:t1", start + Duration::from_millis(100));
        assert_eq!(
            manager.next_deadline(),
            Some(start + Duration::from_millis(250))
        );
        assert!(
            manager
                .reconcile(
                    true,
                    &[tab("w1:t1", 1, manual, "w1:p2")],
                    &panes,
                    start + Duration::from_millis(249),
                )
                .unwrap()
                .is_empty()
        );
        let beta = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, manual, "w1:p2")],
                &panes,
                start + Duration::from_millis(250),
            )
            .unwrap();
        assert_eq!(beta[0].label(), "Beta");
        complete(&mut manager, &beta[0]);

        manager.note_focus("w1:t1", start + Duration::from_millis(300));
        let restored = manager
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta", "w1:p1")],
                &panes,
                start + Duration::from_millis(450),
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
        persisted.note_focus("w1:t1", now);
        let beta = persisted
            .reconcile(
                true,
                &[tab("w1:t1", 1, manual, "w1:p2")],
                &panes,
                now + FOCUS_DEBOUNCE,
            )
            .unwrap();
        assert_eq!(beta[0].label(), "Beta");
        complete(&mut persisted, &beta[0]);
        persisted.note_focus("w1:t1", now + Duration::from_millis(200));
        let restored = persisted
            .reconcile(
                true,
                &[tab("w1:t1", 1, "Beta", "w1:p1")],
                &panes,
                now + Duration::from_millis(350),
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
