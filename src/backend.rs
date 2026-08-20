use crate::claude::ClaudeBackend;
use crate::config::Config;
use crate::pi::PiBackend;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const CLAUDE_AGENT: &str = "claude";
pub const PI_AGENT: &str = "pi";

#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct PaneKey {
    pub pane_id: String,
    pub terminal_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionReference {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ProcessCommand {
    pub name: String,
    pub argv: Option<Vec<String>>,
    pub argv0: Option<String>,
    pub cmdline: Option<String>,
}

impl ProcessCommand {
    pub fn observable_args(&self) -> impl Iterator<Item = &str> {
        self.argv
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(self.argv0.iter().map(String::as_str))
            .chain(self.cmdline.iter().map(String::as_str))
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PaneInput {
    pub key: PaneKey,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub agent: String,
    pub cwd: PathBuf,
    pub terminal_title: Option<String>,
    pub authoritative_session: Option<SessionReference>,
    pub processes: Vec<ProcessCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub size: u64,
    pub modified_at: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingEvidence {
    Official { source: String },
    ExactIdentityHint,
    LocalFallback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    pub path: PathBuf,
    pub evidence: BindingEvidence,
}

impl Binding {
    pub fn is_official(&self) -> bool {
        matches!(self.evidence, BindingEvidence::Official { .. })
    }

    pub fn applies_to_source(&self) -> Option<&str> {
        match &self.evidence {
            BindingEvidence::Official { source } => Some(source),
            BindingEvidence::ExactIdentityHint | BindingEvidence::LocalFallback => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayView {
    pub session_identity: String,
    pub session_name: Option<String>,
    pub tab_name_source: Option<String>,
    pub last_message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendOutcome {
    Unbound,
    Failed,
    Resolved {
        agent: &'static str,
        binding: Binding,
        view: DisplayView,
    },
}

#[derive(Default)]
pub struct BackendRegistry {
    pi: PiBackend,
    claude: ClaudeBackend,
}

impl BackendRegistry {
    pub fn supports_agent(&self, agent: Option<&str>) -> bool {
        agent.is_some_and(|agent| {
            agent.eq_ignore_ascii_case(PI_AGENT) || agent.eq_ignore_ascii_case(CLAUDE_AGENT)
        })
    }

    pub fn reconcile(
        &mut self,
        config: &Config,
        home: &Path,
        env: &HashMap<String, String>,
        panes: &[PaneInput],
    ) -> HashMap<PaneKey, BackendOutcome> {
        let mut outcomes = self.pi.reconcile(config, home, env, panes);
        outcomes.extend(self.claude.reconcile(config, home, env, panes));
        outcomes
    }

    pub(crate) fn authoritative_binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.pi.authoritative_binding(key)
    }
}
