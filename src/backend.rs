use crate::claude::ClaudeBackend;
use crate::codex::CodexBackend;
use crate::config::Config;
use crate::pi::PiBackend;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const CLAUDE_AGENT: &str = "claude";
pub const CODEX_AGENT: &str = "codex";
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
    FailedBinding {
        agent: &'static str,
        binding: Binding,
    },
    FailedIdentity {
        agent: &'static str,
        session_identity: String,
    },
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
    codex: CodexBackend,
}

impl BackendRegistry {
    pub fn supports_agent(&self, agent: Option<&str>) -> bool {
        agent.is_some_and(|agent| {
            agent.eq_ignore_ascii_case(PI_AGENT)
                || agent.eq_ignore_ascii_case(CLAUDE_AGENT)
                || agent.eq_ignore_ascii_case(CODEX_AGENT)
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
        outcomes.extend(self.codex.reconcile(config, home, env, panes));
        outcomes
    }

    pub(crate) fn binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.pi
            .binding(key)
            .or_else(|| self.claude.binding(key))
            .or_else(|| self.codex.binding(key))
    }

    pub(crate) fn authoritative_binding(&self, key: &PaneKey) -> Option<&Binding> {
        self.pi
            .authoritative_binding(key)
            .or_else(|| self.codex.authoritative_binding(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn registry_dispatches_codex_without_claiming_unknown_agents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let day = root.join("2026/08/28");
        fs::create_dir_all(&day).unwrap();
        let identity = "10000000-0000-4000-8000-000000000001";
        fs::write(
            day.join(format!("rollout-2026-08-28T00-00-00-{identity}.jsonl")),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{identity}\",\"cwd\":\"/synthetic/project\",\"source\":\"cli\"}}}}\n"
            ),
        )
        .unwrap();
        let config = Config {
            codex_session_dirs: vec![root],
            ..Config::default()
        };
        let pane = PaneInput {
            key: PaneKey {
                pane_id: "p1".into(),
                terminal_id: "t1".into(),
            },
            workspace_id: None,
            tab_id: None,
            agent: CODEX_AGENT.into(),
            cwd: PathBuf::from("/synthetic/project"),
            terminal_title: None,
            authoritative_session: None,
            processes: vec![ProcessCommand {
                name: "codex".into(),
                argv: Some(vec!["codex".into(), "resume".into(), identity.into()]),
                argv0: Some("codex".into()),
                cmdline: None,
            }],
        };

        let outcomes = BackendRegistry::default().reconcile(
            &config,
            Path::new("/no-home"),
            &HashMap::new(),
            std::slice::from_ref(&pane),
        );
        assert!(matches!(
            outcomes.get(&pane.key),
            Some(BackendOutcome::Resolved { agent, .. }) if *agent == CODEX_AGENT
        ));
    }

    #[test]
    fn registry_supports_only_the_three_static_agents() {
        let registry = BackendRegistry::default();
        for agent in ["pi", "claude", "codex", "CODEX"] {
            assert!(registry.supports_agent(Some(agent)));
        }
        for agent in [None, Some(""), Some("unknown")] {
            assert!(!registry.supports_agent(agent));
        }
    }
}
