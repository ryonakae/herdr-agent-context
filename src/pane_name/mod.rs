mod state;

use std::{collections::BTreeMap, path::Path};

use state::{
    Applied, AppliedSource, PaneState, PendingDisposition, PendingTransition, PersistedState,
    Selection, StateFile, digest_generation, digest_identity, digest_label, digest_pane_id,
    digest_terminal,
};
use thiserror::Error;

use crate::text::context_label;

pub struct PaneSnapshot {
    pub pane_id: String,
    pub terminal_id: String,
    pub binding_identity: Option<Vec<u8>>,
    pub observed_label: Option<String>,
    pub context: PaneContext,
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
    pane_id: String,
    label: Option<String>,
    token: TransitionToken,
}

impl RenameEffect {
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn token(&self) -> &TransitionToken {
        &self.token
    }
}

#[derive(Clone)]
pub struct TransitionToken {
    pane_digest: String,
    target_digest: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RenameCompletion {
    Applied,
    MissingPane,
}

#[derive(Debug, Error)]
pub enum PaneNameError {
    #[error("pane name state is unavailable")]
    State,
    #[error("pane name transition is stale")]
    StaleTransition,
}

pub struct PaneNameManager {
    state_file: StateFile,
    document: PersistedState,
}

impl PaneNameManager {
    pub fn load(state_dir: &Path, socket_path: &Path) -> Result<Self, PaneNameError> {
        let (state_file, document) =
            StateFile::open(state_dir, socket_path).map_err(|_| PaneNameError::State)?;
        Ok(Self {
            state_file,
            document,
        })
    }

    pub fn needs_snapshot(&self, enabled: bool) -> bool {
        enabled || self.document.cleanup_pending || !self.document.panes.is_empty()
    }

    pub fn reconcile(
        &mut self,
        enabled: bool,
        panes: &[PaneSnapshot],
    ) -> Result<Vec<RenameEffect>, PaneNameError> {
        let before = self.document.clone();
        let live_panes: std::collections::HashSet<_> = panes
            .iter()
            .map(|pane| digest_pane_id(&pane.pane_id))
            .collect();
        self.document
            .panes
            .retain(|pane_digest, _| live_panes.contains(pane_digest));

        if !enabled && !self.document.panes.is_empty() {
            self.document.cleanup_pending = true;
        }
        let acquiring = enabled && !self.document.cleanup_pending;
        let mut effects = Vec::new();
        let mut released = Vec::new();

        for pane in panes {
            let pane_digest = digest_pane_id(&pane.pane_id);
            if !self.document.panes.contains_key(&pane_digest) {
                if !acquiring {
                    continue;
                }
                let Some(selection) = supported_selection(pane) else {
                    continue;
                };
                self.document.panes.insert(
                    pane_digest.clone(),
                    PaneState {
                        terminal_digest: digest_terminal(&pane.pane_id, &pane.terminal_id),
                        baseline: pane.observed_label.clone(),
                        selection: Some(selection),
                        overrides: BTreeMap::new(),
                        applied: Applied {
                            target_digest: digest_label(pane.observed_label.as_deref()),
                            source: AppliedSource::Baseline,
                        },
                        pending: None,
                    },
                );
            }

            let pane_state = self.document.panes.get_mut(&pane_digest).unwrap();
            let current_terminal_digest = digest_terminal(&pane.pane_id, &pane.terminal_id);
            let terminal_replaced = pane_state.terminal_digest != current_terminal_digest;
            if terminal_replaced {
                reset_for_terminal(pane_state, pane, current_terminal_digest);
            }

            let active_selection = observation_selection(pane_state, pane);
            let observation = if terminal_replaced {
                Observation::Retained
            } else {
                recover_observation(pane_state, pane, active_selection.as_ref())
            };
            if observation == Observation::Released {
                released.push(pane_digest.clone());
                continue;
            }

            if !acquiring {
                if observation != Observation::PendingPrior
                    && digest_label(pane.observed_label.as_deref())
                        == digest_label(pane_state.baseline.as_deref())
                {
                    released.push(pane_digest.clone());
                    continue;
                }
                if plan_target(
                    &pane_digest,
                    &pane.pane_id,
                    pane.observed_label.as_deref(),
                    pane_state.baseline.clone(),
                    PendingDisposition::Release,
                    pane_state,
                    &mut effects,
                ) {
                    released.push(pane_digest.clone());
                }
                continue;
            }

            match desired_target(pane_state, pane) {
                DesiredTarget::Hold => {}
                DesiredTarget::Target {
                    selection,
                    label,
                    source,
                } => {
                    pane_state.selection = selection;
                    plan_target(
                        &pane_digest,
                        &pane.pane_id,
                        pane.observed_label.as_deref(),
                        label,
                        PendingDisposition::Keep { source },
                        pane_state,
                        &mut effects,
                    );
                }
            }
        }

        for pane_digest in released {
            self.document.panes.remove(&pane_digest);
        }
        if self.document.panes.is_empty() {
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
    ) -> Result<(), PaneNameError> {
        let before = self.document.clone();
        let Some(pane_state) = self.document.panes.get_mut(&token.pane_digest) else {
            return if completion == RenameCompletion::MissingPane {
                Ok(())
            } else {
                Err(PaneNameError::StaleTransition)
            };
        };
        let Some(pending) = pane_state.pending.clone() else {
            return Err(PaneNameError::StaleTransition);
        };
        if pending.target_digest != token.target_digest {
            return Err(PaneNameError::StaleTransition);
        }

        match completion {
            RenameCompletion::MissingPane => {
                self.document.panes.remove(&token.pane_digest);
            }
            RenameCompletion::Applied => match pending.disposition {
                PendingDisposition::Keep { source } => {
                    pane_state.applied = Applied {
                        target_digest: pending.target_digest,
                        source,
                    };
                    pane_state.pending = None;
                }
                PendingDisposition::Release => {
                    self.document.panes.remove(&token.pane_digest);
                }
            },
        }
        if self.document.panes.is_empty() {
            self.document.cleanup_pending = false;
        }
        if let Err(error) = self.persist_document() {
            self.document = before;
            return Err(error);
        }
        Ok(())
    }

    fn persist_document(&self) -> Result<(), PaneNameError> {
        if self.document.panes.is_empty() && !self.document.cleanup_pending {
            self.state_file.remove().map_err(|_| PaneNameError::State)
        } else {
            self.state_file
                .persist(&self.document)
                .map_err(|_| PaneNameError::State)
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Observation {
    Retained,
    PendingPrior,
    Released,
}

enum DesiredTarget {
    Hold,
    Target {
        selection: Option<Selection>,
        label: Option<String>,
        source: AppliedSource,
    },
}

fn recover_observation(
    pane_state: &mut PaneState,
    pane: &PaneSnapshot,
    selection: Option<&Selection>,
) -> Observation {
    let observed_digest = digest_label(pane.observed_label.as_deref());
    if let Some(pending) = pane_state.pending.clone() {
        if observed_digest == pending.target_digest {
            return match pending.disposition {
                PendingDisposition::Keep { source } => {
                    pane_state.applied = Applied {
                        target_digest: pending.target_digest,
                        source,
                    };
                    pane_state.pending = None;
                    Observation::Retained
                }
                PendingDisposition::Release => Observation::Released,
            };
        }
        if observed_digest == pending.prior_digest {
            return Observation::PendingPrior;
        }
        record_manual(pane_state, selection, pane.observed_label.clone());
        return Observation::Retained;
    }

    if observed_digest != pane_state.applied.target_digest {
        record_manual(pane_state, selection, pane.observed_label.clone());
    }
    Observation::Retained
}

fn record_manual(pane_state: &mut PaneState, selection: Option<&Selection>, label: Option<String>) {
    pane_state.baseline = label.clone();
    pane_state.pending = None;
    if let Some(selection) = selection {
        pane_state.selection = Some(selection.clone());
        pane_state
            .overrides
            .insert(selection.identity_digest.clone(), label.clone());
        pane_state.applied = Applied {
            target_digest: digest_label(label.as_deref()),
            source: AppliedSource::Override {
                identity_digest: selection.identity_digest.clone(),
            },
        };
    } else {
        pane_state.selection = None;
        pane_state.applied = Applied {
            target_digest: digest_label(label.as_deref()),
            source: AppliedSource::Baseline,
        };
    }
}

fn reset_for_terminal(pane_state: &mut PaneState, pane: &PaneSnapshot, terminal_digest: String) {
    let observed_digest = digest_label(pane.observed_label.as_deref());
    let observed_was_owned = pane_state.applied.target_digest == observed_digest
        || pane_state.pending.as_ref().is_some_and(|pending| {
            pending.prior_digest == observed_digest || pending.target_digest == observed_digest
        });
    let baseline = if observed_was_owned {
        pane_state.baseline.clone()
    } else {
        pane.observed_label.clone()
    };
    *pane_state = PaneState {
        terminal_digest,
        baseline: baseline.clone(),
        selection: None,
        overrides: BTreeMap::new(),
        applied: Applied {
            target_digest: digest_label(baseline.as_deref()),
            source: AppliedSource::Baseline,
        },
        pending: None,
    };
}

fn observation_selection(pane_state: &PaneState, pane: &PaneSnapshot) -> Option<Selection> {
    if let Some(selection) = supported_selection(pane) {
        return Some(selection);
    }
    let PaneContext::Failed { agent } = &pane.context else {
        return None;
    };
    let binding_identity = pane.binding_identity.as_deref()?;
    let generation_digest =
        digest_generation(&pane.pane_id, &pane.terminal_id, agent, binding_identity);
    pane_state
        .selection
        .as_ref()
        .filter(|selection| selection.generation_digest == generation_digest)
        .cloned()
}

fn supported_selection(pane: &PaneSnapshot) -> Option<Selection> {
    let binding_identity = pane.binding_identity.as_deref()?;
    let PaneContext::Supported {
        agent, identity, ..
    } = &pane.context
    else {
        return None;
    };
    Some(Selection {
        generation_digest: digest_generation(
            &pane.pane_id,
            &pane.terminal_id,
            agent,
            binding_identity,
        ),
        identity_digest: digest_identity(&pane.pane_id, agent, identity),
    })
}

fn desired_target(pane_state: &PaneState, pane: &PaneSnapshot) -> DesiredTarget {
    let Some(selection) = supported_selection(pane) else {
        if matches!(&pane.context, PaneContext::Failed { .. }) {
            if let Some(selection) = observation_selection(pane_state, pane) {
                return retryable_failure_target(pane_state, selection)
                    .unwrap_or(DesiredTarget::Hold);
            }
        }
        return DesiredTarget::Target {
            selection: None,
            label: pane_state.baseline.clone(),
            source: AppliedSource::Baseline,
        };
    };

    if let Some(label) = pane_state.overrides.get(&selection.identity_digest) {
        return DesiredTarget::Target {
            selection: Some(selection.clone()),
            label: label.clone(),
            source: AppliedSource::Override {
                identity_digest: selection.identity_digest,
            },
        };
    }

    let PaneContext::Supported { display, .. } = &pane.context else {
        unreachable!();
    };
    match display {
        DisplayState::Resolved(title) => match context_label(title) {
            Some(label) => DesiredTarget::Target {
                selection: Some(selection.clone()),
                label: Some(label),
                source: AppliedSource::Generated {
                    identity_digest: selection.identity_digest,
                },
            },
            None => DesiredTarget::Target {
                selection: Some(selection),
                label: pane_state.baseline.clone(),
                source: AppliedSource::Baseline,
            },
        },
        DisplayState::Unresolved => DesiredTarget::Target {
            selection: Some(selection),
            label: pane_state.baseline.clone(),
            source: AppliedSource::Baseline,
        },
        DisplayState::Failed if pane_state.selection.as_ref() == Some(&selection) => {
            retryable_failure_target(pane_state, selection).unwrap_or(DesiredTarget::Hold)
        }
        DisplayState::Failed => DesiredTarget::Target {
            selection: Some(selection),
            label: pane_state.baseline.clone(),
            source: AppliedSource::Baseline,
        },
    }
}

fn retryable_failure_target(pane_state: &PaneState, selection: Selection) -> Option<DesiredTarget> {
    let PendingDisposition::Keep { source } = &pane_state.pending.as_ref()?.disposition else {
        return None;
    };
    match source {
        AppliedSource::Baseline => Some(DesiredTarget::Target {
            selection: Some(selection),
            label: pane_state.baseline.clone(),
            source: AppliedSource::Baseline,
        }),
        AppliedSource::Override { identity_digest }
            if identity_digest == &selection.identity_digest =>
        {
            pane_state
                .overrides
                .get(identity_digest)
                .map(|label| DesiredTarget::Target {
                    selection: Some(selection),
                    label: label.clone(),
                    source: source.clone(),
                })
        }
        AppliedSource::Generated { .. } | AppliedSource::Override { .. } => None,
    }
}

fn plan_target(
    pane_digest: &str,
    pane_id: &str,
    observed_label: Option<&str>,
    target: Option<String>,
    disposition: PendingDisposition,
    pane_state: &mut PaneState,
    effects: &mut Vec<RenameEffect>,
) -> bool {
    let prior_digest = digest_label(observed_label);
    let target_digest = digest_label(target.as_deref());
    if prior_digest == target_digest {
        return match disposition {
            PendingDisposition::Keep { source } => {
                pane_state.applied = Applied {
                    target_digest,
                    source,
                };
                pane_state.pending = None;
                false
            }
            PendingDisposition::Release => true,
        };
    }
    pane_state.pending = Some(PendingTransition {
        prior_digest,
        target_digest: target_digest.clone(),
        disposition,
    });
    effects.push(RenameEffect {
        pane_id: pane_id.to_owned(),
        label: target,
        token: TransitionToken {
            pane_digest: pane_digest.to_owned(),
            target_digest,
        },
    });
    false
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn resolved(
        pane_id: &str,
        terminal_id: &str,
        identity: &str,
        title: &str,
        observed_label: Option<&str>,
    ) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            binding_identity: Some(format!("binding-{identity}").into_bytes()),
            observed_label: observed_label.map(str::to_owned),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Resolved(title.into()),
            },
        }
    }

    fn unresolved(
        pane_id: &str,
        terminal_id: &str,
        identity: &str,
        observed_label: Option<&str>,
    ) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            binding_identity: Some(format!("binding-{identity}").into_bytes()),
            observed_label: observed_label.map(str::to_owned),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Unresolved,
            },
        }
    }

    fn failed(
        pane_id: &str,
        terminal_id: &str,
        identity: &str,
        binding: &str,
        observed_label: Option<&str>,
    ) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            binding_identity: Some(binding.as_bytes().to_vec()),
            observed_label: observed_label.map(str::to_owned),
            context: PaneContext::Supported {
                agent: "pi".into(),
                identity: identity.into(),
                display: DisplayState::Failed,
            },
        }
    }

    fn failed_without_identity(
        pane_id: &str,
        terminal_id: &str,
        binding: &str,
        observed_label: Option<&str>,
    ) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            binding_identity: Some(binding.as_bytes().to_vec()),
            observed_label: observed_label.map(str::to_owned),
            context: PaneContext::Failed { agent: "pi".into() },
        }
    }

    fn unsupported(pane_id: &str, terminal_id: &str, observed_label: Option<&str>) -> PaneSnapshot {
        PaneSnapshot {
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            binding_identity: None,
            observed_label: observed_label.map(str::to_owned),
            context: PaneContext::Unsupported,
        }
    }

    #[test]
    fn unnamed_pane_acquires_generated_label_and_disable_clears_to_null() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager =
            PaneNameManager::load(temporary.path(), Path::new("/tmp/herdr-pane-unnamed.sock"))
                .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved("pane-a", "terminal-a", "session-a", "Alpha", None)],
            )
            .unwrap();

        assert_eq!(acquired.len(), 1);
        assert_eq!(acquired[0].pane_id(), "pane-a");
        assert_eq!(acquired[0].label(), Some("Alpha"));
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        let released = manager
            .reconcile(
                false,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("Alpha"),
                )],
            )
            .unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].label(), None);
    }

    #[test]
    fn pre_named_pane_restores_its_exact_baseline() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-pre-named.sock"),
        )
        .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("manual baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        let released = manager
            .reconcile(
                false,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert_eq!(released[0].label(), Some("manual baseline"));
    }

    #[test]
    fn manual_override_for_a_does_not_suppress_b_and_returns_with_a() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-manual-a-b.sock"),
        )
        .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        assert!(
            manager
                .reconcile(
                    true,
                    &[resolved(
                        "pane-a",
                        "terminal-a",
                        "session-a",
                        "Alpha",
                        Some("manual A"),
                    )],
                )
                .unwrap()
                .is_empty()
        );

        let acquired_b = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-b",
                    "Beta",
                    Some("manual A"),
                )],
            )
            .unwrap();
        assert_eq!(acquired_b[0].label(), Some("Beta"));
        manager
            .complete_rename(acquired_b[0].token(), RenameCompletion::Applied)
            .unwrap();

        let restored_a = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("Beta"),
                )],
            )
            .unwrap();
        assert_eq!(restored_a[0].label(), Some("manual A"));
    }

    #[test]
    fn manual_clear_is_a_real_override_scoped_to_identity_a() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-manual-clear.sock"),
        )
        .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        assert!(
            manager
                .reconcile(
                    true,
                    &[resolved("pane-a", "terminal-a", "session-a", "Alpha", None,)],
                )
                .unwrap()
                .is_empty()
        );
        let acquired_b = manager
            .reconcile(
                true,
                &[resolved("pane-a", "terminal-a", "session-b", "Beta", None)],
            )
            .unwrap();
        assert_eq!(acquired_b[0].label(), Some("Beta"));
        manager
            .complete_rename(acquired_b[0].token(), RenameCompletion::Applied)
            .unwrap();

        let restored_a = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("Beta"),
                )],
            )
            .unwrap();
        assert_eq!(restored_a[0].label(), None);
    }

    #[test]
    fn untitled_identity_uses_baseline_until_a_title_is_available() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager =
            PaneNameManager::load(temporary.path(), Path::new("/tmp/herdr-pane-untitled.sock"))
                .unwrap();

        assert!(
            manager
                .reconcile(
                    true,
                    &[unresolved(
                        "pane-a",
                        "terminal-a",
                        "session-a",
                        Some("baseline"),
                    )],
                )
                .unwrap()
                .is_empty()
        );
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();

        assert_eq!(acquired[0].label(), Some("Alpha"));
    }

    #[test]
    fn known_read_failure_retains_the_current_owned_label() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-known-failure.sock"),
        )
        .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        let effects = manager
            .reconcile(
                true,
                &[failed(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "binding-session-a",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert!(effects.is_empty());
    }

    #[test]
    fn unsupported_and_unbound_panes_restore_the_exact_baseline() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager =
            PaneNameManager::load(temporary.path(), Path::new("/tmp/herdr-pane-unbound.sock"))
                .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        let restored = manager
            .reconcile(true, &[unsupported("pane-a", "terminal-a", Some("Alpha"))])
            .unwrap();
        assert_eq!(restored[0].label(), Some("baseline"));
        manager
            .complete_rename(restored[0].token(), RenameCompletion::Applied)
            .unwrap();

        let acquired_again = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired_again[0].token(), RenameCompletion::Applied)
            .unwrap();
        let mut unbound = failed("pane-a", "terminal-a", "session-a", "unused", Some("Alpha"));
        unbound.binding_identity = None;
        let restored = manager.reconcile(true, &[unbound]).unwrap();
        assert_eq!(restored[0].label(), Some("baseline"));
    }

    #[test]
    fn missing_pane_completes_pending_cleanup_and_removes_state() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager =
            PaneNameManager::load(temporary.path(), Path::new("/tmp/herdr-pane-missing.sock"))
                .unwrap();
        let pending = manager
            .reconcile(
                true,
                &[resolved("pane-a", "terminal-a", "session-a", "Alpha", None)],
            )
            .unwrap();
        manager
            .complete_rename(pending[0].token(), RenameCompletion::MissingPane)
            .unwrap();
        assert!(!manager.needs_snapshot(false));
        assert!(!manager.state_file.path().exists());

        let acquired = manager
            .reconcile(
                true,
                &[resolved("pane-a", "terminal-a", "session-a", "Alpha", None)],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();
        assert!(manager.reconcile(true, &[]).unwrap().is_empty());
        assert!(!manager.needs_snapshot(false));
        assert!(!manager.state_file.path().exists());
    }

    #[test]
    fn terminal_replacement_does_not_retain_the_old_owned_label() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-terminal-replacement.sock"),
        )
        .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        let restored = manager
            .reconcile(
                true,
                &[failed(
                    "pane-a",
                    "terminal-replacement",
                    "session-a",
                    "binding-session-a",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label(), Some("baseline"));
    }

    #[test]
    fn binding_generation_replacement_does_not_retain_failed_ownership() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-binding-replacement.sock"),
        )
        .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();

        let restored = manager
            .reconcile(
                true,
                &[failed(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "replacement-binding",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label(), Some("baseline"));
    }

    #[test]
    fn restart_retries_when_crash_happened_before_the_rpc() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pane-before-rpc.sock");
        let mut manager = PaneNameManager::load(temporary.path(), socket).unwrap();
        let pending = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        assert_eq!(pending[0].label(), Some("Alpha"));
        drop(manager);

        let mut restarted = PaneNameManager::load(temporary.path(), socket).unwrap();
        let retried = restarted
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();

        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].label(), Some("Alpha"));
    }

    #[test]
    fn restart_finalizes_when_crash_happened_after_the_rpc() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pane-after-rpc.sock");
        let mut manager = PaneNameManager::load(temporary.path(), socket).unwrap();
        manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        drop(manager);

        let mut restarted = PaneNameManager::load(temporary.path(), socket).unwrap();
        let effects = restarted
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert!(effects.is_empty());
        assert!(
            restarted
                .document
                .panes
                .values()
                .all(|pane| pane.pending.is_none())
        );
    }

    #[test]
    fn restart_retries_persisted_baseline_target_when_new_identity_read_fails() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pane-failed-baseline-retry.sock");
        let mut manager = PaneNameManager::load(temporary.path(), socket).unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();
        let pending_baseline = manager
            .reconcile(
                true,
                &[unresolved(
                    "pane-a",
                    "terminal-a",
                    "session-b",
                    Some("Alpha"),
                )],
            )
            .unwrap();
        assert_eq!(pending_baseline[0].label(), Some("baseline"));
        drop(manager);

        let mut restarted = PaneNameManager::load(temporary.path(), socket).unwrap();
        let retried = restarted
            .reconcile(
                true,
                &[failed(
                    "pane-a",
                    "terminal-a",
                    "session-b",
                    "binding-session-b",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].label(), Some("baseline"));
    }

    #[test]
    fn restart_retries_baseline_when_known_failure_lacks_session_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pane-failed-binding-retry.sock");
        let mut manager = PaneNameManager::load(temporary.path(), socket).unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();
        let pending_baseline = manager
            .reconcile(
                true,
                &[unresolved(
                    "pane-a",
                    "terminal-a",
                    "session-b",
                    Some("Alpha"),
                )],
            )
            .unwrap();
        assert_eq!(pending_baseline[0].label(), Some("baseline"));
        drop(manager);

        let mut restarted = PaneNameManager::load(temporary.path(), socket).unwrap();
        let retried = restarted
            .reconcile(
                true,
                &[failed_without_identity(
                    "pane-a",
                    "terminal-a",
                    "binding-session-b",
                    Some("Alpha"),
                )],
            )
            .unwrap();

        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].label(), Some("baseline"));
    }

    #[test]
    fn restart_classifies_external_pending_observation_as_the_identity_override() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pane-external-rpc.sock");
        let mut manager = PaneNameManager::load(temporary.path(), socket).unwrap();
        manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        drop(manager);

        let mut restarted = PaneNameManager::load(temporary.path(), socket).unwrap();
        assert!(
            restarted
                .reconcile(
                    true,
                    &[resolved(
                        "pane-a",
                        "terminal-a",
                        "session-a",
                        "Alpha",
                        Some("manual A"),
                    )],
                )
                .unwrap()
                .is_empty()
        );
        let acquired_b = restarted
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-b",
                    "Beta",
                    Some("manual A"),
                )],
            )
            .unwrap();
        restarted
            .complete_rename(acquired_b[0].token(), RenameCompletion::Applied)
            .unwrap();
        let restored_a = restarted
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("Beta"),
                )],
            )
            .unwrap();
        assert_eq!(restored_a[0].label(), Some("manual A"));
    }

    #[test]
    fn persistence_failure_returns_no_effect_and_finalization_remains_retryable() {
        use state::StateError;

        let temporary = tempfile::tempdir().unwrap();
        let mut manager = PaneNameManager::load(
            temporary.path(),
            Path::new("/tmp/herdr-pane-persist-failure.sock"),
        )
        .unwrap();
        manager.state_file.fail_next_persist(StateError::Write);
        let result = manager.reconcile(
            true,
            &[resolved(
                "pane-a",
                "terminal-a",
                "session-a",
                "Alpha",
                Some("baseline"),
            )],
        );
        assert!(matches!(result, Err(PaneNameError::State)));
        assert!(!manager.needs_snapshot(false));

        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("baseline"),
                )],
            )
            .unwrap();
        manager.state_file.fail_next_persist(StateError::Sync);
        assert!(matches!(
            manager.complete_rename(acquired[0].token(), RenameCompletion::Applied),
            Err(PaneNameError::State)
        ));
        manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();
    }

    #[test]
    fn pane_manual_override_does_not_touch_tab_state_or_component_source() {
        use std::{fs, time::Instant};

        use crate::tab_name::{
            DisplayState as TabDisplayState, PaneContext as TabPaneContext,
            PaneSnapshot as TabPaneSnapshot, TabNameManager, TabSnapshot,
        };

        let temporary = tempfile::tempdir().unwrap();
        let socket = Path::new("/tmp/herdr-pane-tab-isolation.sock");
        let mut tab_manager = TabNameManager::load(temporary.path(), socket).unwrap();
        let tab_snapshot = [TabSnapshot {
            tab_id: "tab-a".into(),
            workspace_id: "workspace-a".into(),
            position: 1,
            observed_label: "tab baseline".into(),
        }];
        let tab_panes = [TabPaneSnapshot {
            pane_id: "pane-a".into(),
            terminal_id: "terminal-a".into(),
            binding_identity: Some(b"binding-session-a".to_vec()),
            tab_id: "tab-a".into(),
            context: TabPaneContext::Supported {
                agent: "pi".into(),
                identity: "session-a".into(),
                display: TabDisplayState::Resolved("Alpha".into()),
            },
        }];
        let tab_effects = tab_manager
            .reconcile(true, &tab_snapshot, &tab_panes, Instant::now())
            .unwrap();
        assert_eq!(tab_effects[0].label(), "Alpha");
        drop(tab_manager);
        let tab_path = fs::read_dir(temporary.path().join("tab-name"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let tab_before = fs::read(&tab_path).unwrap();

        let mut pane_manager = PaneNameManager::load(temporary.path(), socket).unwrap();
        let acquired = pane_manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "Alpha",
                    Some("pane baseline"),
                )],
            )
            .unwrap();
        pane_manager
            .complete_rename(acquired[0].token(), RenameCompletion::Applied)
            .unwrap();
        assert!(
            pane_manager
                .reconcile(
                    true,
                    &[resolved(
                        "pane-a",
                        "terminal-a",
                        "session-a",
                        "Alpha",
                        Some("manual pane label"),
                    )],
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(&tab_path).unwrap(), tab_before);

        let mut restarted_tab = TabNameManager::load(temporary.path(), socket).unwrap();
        let retried = restarted_tab
            .reconcile(true, &tab_snapshot, &tab_panes, Instant::now())
            .unwrap();
        assert_eq!(retried[0].label(), "Alpha");
    }

    #[test]
    fn generated_labels_use_the_twenty_column_context_contract() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager =
            PaneNameManager::load(temporary.path(), Path::new("/tmp/herdr-pane-width.sock"))
                .unwrap();
        let acquired = manager
            .reconcile(
                true,
                &[resolved(
                    "pane-a",
                    "terminal-a",
                    "session-a",
                    "a generated context title that is long",
                    None,
                )],
            )
            .unwrap();

        assert_eq!(acquired[0].label(), Some("a generated context…"));
    }
}
