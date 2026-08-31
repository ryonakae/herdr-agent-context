use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

pub const DEFAULT_POLL_INTERVAL_MS: u64 = 2_000;
pub const DEFAULT_METADATA_TTL_MS: u64 = 10_000;
pub const MAX_METADATA_TTL_MS: u64 = 86_400_000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TabNameConfig {
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PaneNameConfig {
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub metadata_ttl_ms: u64,
    pub pi_session_dirs: Vec<PathBuf>,
    pub claude_session_dirs: Vec<PathBuf>,
    pub codex_session_dirs: Vec<PathBuf>,
    pub opencode_database_paths: Vec<PathBuf>,
    pub tab_name: TabNameConfig,
    pub pane_name: PaneNameConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            metadata_ttl_ms: DEFAULT_METADATA_TTL_MS,
            pi_session_dirs: Vec::new(),
            claude_session_dirs: Vec::new(),
            codex_session_dirs: Vec::new(),
            opencode_database_paths: Vec::new(),
            tab_name: TabNameConfig::default(),
            pane_name: PaneNameConfig::default(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    poll_interval_ms: Option<u64>,
    metadata_ttl_ms: Option<u64>,
    pi_session_dirs: Option<Vec<PathBuf>>,
    agents: Option<RawAgents>,
    tab_name: Option<RawTabNameConfig>,
    pane_name: Option<RawPaneNameConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTabNameConfig {
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPaneNameConfig {
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgents {
    pi: Option<RawAgentConfig>,
    claude: Option<RawAgentConfig>,
    codex: Option<RawAgentConfig>,
    opencode: Option<RawOpenCodeConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentConfig {
    session_dirs: Option<Vec<PathBuf>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOpenCodeConfig {
    database_paths: Option<Vec<PathBuf>>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read plugin config")]
    Read(#[source] std::io::Error),
    #[error("failed to parse plugin config")]
    Parse(#[source] toml::de::Error),
    #[error("poll_interval_ms must be greater than zero")]
    ZeroPollInterval,
    #[error("metadata_ttl_ms must be greater than poll_interval_ms")]
    TtlNotGreaterThanPoll,
    #[error("metadata_ttl_ms exceeds the Herdr API limit")]
    TtlTooLarge,
    #[error("a configured session directory is not absolute after expansion")]
    RelativeSessionDir,
    #[error("pi_session_dirs conflicts with agents.pi.session_dirs")]
    ConflictingPiSessionDirs,
}

#[derive(Debug)]
pub enum ConfigReload {
    Unchanged,
    Updated(Config),
    Invalid,
}

pub struct ConfigWatcher {
    path: PathBuf,
    home: PathBuf,
    initialized: bool,
    identity: Option<(u64, SystemTime)>,
}

impl ConfigWatcher {
    pub fn new(config_dir: &Path, home: &Path) -> Self {
        Self {
            path: config_dir.join("config.toml"),
            home: home.to_owned(),
            initialized: false,
            identity: None,
        }
    }

    pub fn poll(&mut self) -> ConfigReload {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let changed = !self.initialized || self.identity.is_some();
                self.initialized = true;
                self.identity = None;
                return if changed {
                    ConfigReload::Updated(Config::default())
                } else {
                    ConfigReload::Unchanged
                };
            }
            Err(_) => return ConfigReload::Invalid,
        };
        let identity = (
            metadata.len(),
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        );
        if self.initialized && self.identity == Some(identity) {
            return ConfigReload::Unchanged;
        }
        self.initialized = true;
        self.identity = Some(identity);
        match fs::read_to_string(&self.path)
            .map_err(ConfigError::Read)
            .and_then(|input| Config::from_toml(&input, &self.home))
        {
            Ok(config) => ConfigReload::Updated(config),
            Err(_) => ConfigReload::Invalid,
        }
    }
}

impl Config {
    pub fn from_toml(input: &str, home: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(input).map_err(ConfigError::Parse)?;
        let agents = raw.agents.unwrap_or_default();
        if raw.pi_session_dirs.is_some() && agents.pi.is_some() {
            return Err(ConfigError::ConflictingPiSessionDirs);
        }
        let pi_session_dirs = normalize_config_paths(
            raw.pi_session_dirs
                .or_else(|| agents.pi.and_then(|pi| pi.session_dirs))
                .unwrap_or_default(),
            home,
        )?;
        let claude_session_dirs = normalize_config_paths(
            agents
                .claude
                .and_then(|claude| claude.session_dirs)
                .unwrap_or_default(),
            home,
        )?;
        let codex_session_dirs = normalize_config_paths(
            agents
                .codex
                .and_then(|codex| codex.session_dirs)
                .unwrap_or_default(),
            home,
        )?;
        let opencode_database_paths = normalize_config_paths(
            agents
                .opencode
                .and_then(|opencode| opencode.database_paths)
                .unwrap_or_default(),
            home,
        )?;
        let config = Self {
            poll_interval_ms: raw.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS),
            metadata_ttl_ms: raw.metadata_ttl_ms.unwrap_or(DEFAULT_METADATA_TTL_MS),
            pi_session_dirs,
            claude_session_dirs,
            codex_session_dirs,
            opencode_database_paths,
            tab_name: TabNameConfig {
                enabled: raw
                    .tab_name
                    .and_then(|tab_name| tab_name.enabled)
                    .unwrap_or(false),
            },
            pane_name: PaneNameConfig {
                enabled: raw
                    .pane_name
                    .and_then(|pane_name| pane_name.enabled)
                    .unwrap_or(false),
            },
        };
        config.validate()?;
        Ok(config)
    }

    pub fn load(config_dir: &Path, home: &Path) -> Result<Self, ConfigError> {
        let path = config_dir.join("config.toml");
        match fs::read_to_string(path) {
            Ok(input) => Self::from_toml(&input, home),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ConfigError::Read(error)),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.poll_interval_ms == 0 {
            return Err(ConfigError::ZeroPollInterval);
        }
        if self.metadata_ttl_ms <= self.poll_interval_ms {
            return Err(ConfigError::TtlNotGreaterThanPoll);
        }
        if self.metadata_ttl_ms > MAX_METADATA_TTL_MS {
            return Err(ConfigError::TtlTooLarge);
        }
        Ok(())
    }
}

pub fn resolve_pi_agent_dir(env: &HashMap<String, String>, home: &Path) -> PathBuf {
    env.get("PI_CODING_AGENT_DIR")
        .filter(|value| !value.is_empty())
        .and_then(|value| normalize_env_path(Path::new(value), home))
        .unwrap_or_else(|| home.join(".pi/agent"))
}

pub fn resolve_session_roots(
    env: &HashMap<String, String>,
    home: &Path,
    additional: &[PathBuf],
) -> Vec<PathBuf> {
    let primary = env
        .get("PI_CODING_AGENT_SESSION_DIR")
        .filter(|value| !value.is_empty())
        .and_then(|value| normalize_env_path(Path::new(value), home))
        .unwrap_or_else(|| resolve_pi_agent_dir(env, home).join("sessions"));
    merge_roots(primary, additional)
}

pub fn resolve_codex_session_roots(
    env: &HashMap<String, String>,
    home: &Path,
    additional: &[PathBuf],
) -> Vec<PathBuf> {
    let primary = env
        .get("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .and_then(|value| normalize_env_path(Path::new(value), home))
        .map(|path| path.join("sessions"))
        .unwrap_or_else(|| home.join(".codex/sessions"));
    merge_roots(primary, additional)
}

pub fn resolve_claude_project_roots(
    env: &HashMap<String, String>,
    home: &Path,
    additional: &[PathBuf],
) -> Vec<PathBuf> {
    let primary = env
        .get("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .and_then(|value| normalize_env_path(Path::new(value), home))
        .map(|path| path.join("projects"))
        .unwrap_or_else(|| home.join(".claude/projects"));
    merge_roots(primary, additional)
}

pub fn resolve_opencode_database_paths(
    env: &HashMap<String, String>,
    home: &Path,
    additional: &[PathBuf],
) -> Vec<PathBuf> {
    let data_directory = env
        .get("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(|path| canonical_or_normalized(path.to_owned()).join("opencode"))
        .unwrap_or_else(|| home.join(".local/share/opencode"));
    let primary = env
        .get("OPENCODE_DB")
        .filter(|value| !value.is_empty())
        .map(Path::new)
        .map(|path| {
            if path.is_absolute() {
                path.to_owned()
            } else {
                data_directory.join(path)
            }
        })
        .unwrap_or_else(|| data_directory.join("opencode.db"));
    merge_roots(primary, additional)
}

fn merge_roots(primary: PathBuf, additional: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for path in std::iter::once(primary).chain(additional.iter().cloned()) {
        let path = canonical_or_normalized(path);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

fn normalize_env_path(path: &Path, home: &Path) -> Option<PathBuf> {
    let expanded = if let Ok(rest) = path.strip_prefix("~") {
        home.join(rest)
    } else {
        path.to_owned()
    };
    expanded
        .is_absolute()
        .then(|| canonical_or_normalized(expanded))
}

fn normalize_config_paths(paths: Vec<PathBuf>, home: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    paths
        .into_iter()
        .map(|path| normalize_config_path(&path, home))
        .collect()
}

fn normalize_config_path(path: &Path, home: &Path) -> Result<PathBuf, ConfigError> {
    let expanded = if let Ok(rest) = path.strip_prefix("~") {
        home.join(rest)
    } else {
        path.to_owned()
    };
    if !expanded.is_absolute() {
        return Err(ConfigError::RelativeSessionDir);
    }
    Ok(canonical_or_normalized(expanded))
}

fn canonical_or_normalized(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or_else(|_| normalize_components(&path))
}

fn normalize_components(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_config_are_valid() {
        assert_eq!(
            Config::from_toml("", Path::new("/home/me")).unwrap(),
            Config::default()
        );
        let config = Config::from_toml("poll_interval_ms = 3000", Path::new("/home/me")).unwrap();
        assert_eq!(config.poll_interval_ms, 3_000);
        assert_eq!(config.metadata_ttl_ms, 10_000);
    }

    #[test]
    fn label_sync_is_strict_independent_and_disabled_by_default() {
        let home = Path::new("/home/me");
        let defaults = Config::from_toml("", home).unwrap();
        assert!(!defaults.tab_name.enabled);
        assert!(!defaults.pane_name.enabled);

        let tab_only = Config::from_toml("[tab_name]\nenabled = true", home).unwrap();
        assert!(tab_only.tab_name.enabled);
        assert!(!tab_only.pane_name.enabled);

        let pane_only = Config::from_toml("[pane_name]\nenabled = true", home).unwrap();
        assert!(!pane_only.tab_name.enabled);
        assert!(pane_only.pane_name.enabled);

        assert!(Config::from_toml("[tab_name]\nwidth = 20", home).is_err());
        assert!(Config::from_toml("[pane_name]\nwidth = 20", home).is_err());
    }

    #[test]
    fn rejects_unknown_and_invalid_timing_values() {
        assert!(Config::from_toml("poll_interval_ms = 0", Path::new("/home/me")).is_err());
        assert!(
            Config::from_toml(
                "poll_interval_ms = 2000\nmetadata_ttl_ms = 2000",
                Path::new("/home/me")
            )
            .is_err()
        );
        assert!(Config::from_toml("unknown_key = true", Path::new("/home/me")).is_err());
    }

    #[test]
    fn expands_tilde_and_rejects_relative_directories() {
        let config =
            Config::from_toml("pi_session_dirs = [\"~/sessions\"]", Path::new("/home/me")).unwrap();
        assert_eq!(
            config.pi_session_dirs,
            vec![PathBuf::from("/home/me/sessions")]
        );
        assert!(
            Config::from_toml("pi_session_dirs = [\"relative\"]", Path::new("/home/me")).is_err()
        );
    }

    #[test]
    fn structured_pi_directories_match_legacy_config() {
        let config = Config::from_toml(
            "[agents.pi]\nsession_dirs = [\"~/sessions\", \"/extra\"]",
            Path::new("/home/me"),
        )
        .unwrap();
        assert_eq!(
            config.pi_session_dirs,
            vec![PathBuf::from("/home/me/sessions"), PathBuf::from("/extra")]
        );
    }

    #[test]
    fn structured_claude_directories_are_normalized() {
        let config = Config::from_toml(
            "[agents.claude]\nsession_dirs = [\"~/claude-projects\", \"/extra\"]",
            Path::new("/home/me"),
        )
        .unwrap();
        assert_eq!(
            config.claude_session_dirs,
            vec![
                PathBuf::from("/home/me/claude-projects"),
                PathBuf::from("/extra")
            ]
        );
    }

    #[test]
    fn rejects_conflicting_or_unknown_structured_agent_config() {
        let home = Path::new("/home/me");
        assert!(
            Config::from_toml("pi_session_dirs = []\n[agents.pi]\nsession_dirs = []", home)
                .is_err()
        );
        assert!(Config::from_toml("[agents.unknown]\nsession_dirs = []", home).is_err());
        assert!(Config::from_toml("[agents.pi]\nunknown = true", home).is_err());
        assert!(Config::from_toml("[agents.pi]\nsession_dirs = [\"relative\"]", home).is_err());
    }

    #[test]
    fn resolves_strict_codex_config_and_listener_home_roots() {
        let home = Path::new("/home/me");
        let config = Config::from_toml(
            "[agents.codex]\nsession_dirs = [\"~/extra/sessions\", \"/shared/sessions\", \"/shared/sessions\"]",
            home,
        )
        .unwrap();
        assert_eq!(
            config.codex_session_dirs,
            vec![
                PathBuf::from("/home/me/extra/sessions"),
                PathBuf::from("/shared/sessions"),
                PathBuf::from("/shared/sessions")
            ]
        );

        let mut env = HashMap::new();
        assert_eq!(
            resolve_codex_session_roots(&env, home, &config.codex_session_dirs),
            vec![
                PathBuf::from("/home/me/.codex/sessions"),
                PathBuf::from("/home/me/extra/sessions"),
                PathBuf::from("/shared/sessions")
            ]
        );
        env.insert("CODEX_HOME".into(), "~/codex-work".into());
        assert_eq!(
            resolve_codex_session_roots(&env, home, &[]),
            vec![PathBuf::from("/home/me/codex-work/sessions")]
        );
        env.insert("CODEX_HOME".into(), "relative".into());
        assert_eq!(
            resolve_codex_session_roots(&env, home, &[]),
            vec![PathBuf::from("/home/me/.codex/sessions")]
        );

        assert!(Config::from_toml("[agents.codex]\nunknown = true", home).is_err());
        assert!(Config::from_toml("[agents.codex]\nsession_dirs = [\"relative\"]", home).is_err());
    }

    #[test]
    fn resolves_strict_opencode_database_config_and_environment_precedence() {
        let home = Path::new("/home/me");
        let config = Config::from_toml(
            "[agents.opencode]\ndatabase_paths = [\"~/extra/../extra/opencode.db\", \"/shared/opencode.db\", \"/shared/opencode.db\"]",
            home,
        )
        .unwrap();
        assert_eq!(
            config.opencode_database_paths,
            vec![
                PathBuf::from("/home/me/extra/opencode.db"),
                PathBuf::from("/shared/opencode.db"),
                PathBuf::from("/shared/opencode.db"),
            ]
        );

        let mut env = HashMap::new();
        assert_eq!(
            resolve_opencode_database_paths(&env, home, &config.opencode_database_paths),
            vec![
                PathBuf::from("/home/me/.local/share/opencode/opencode.db"),
                PathBuf::from("/home/me/extra/opencode.db"),
                PathBuf::from("/shared/opencode.db"),
            ]
        );
        env.insert("XDG_DATA_HOME".into(), "/xdg/data".into());
        assert_eq!(
            resolve_opencode_database_paths(&env, home, &[]),
            vec![PathBuf::from("/xdg/data/opencode/opencode.db")]
        );
        env.insert("OPENCODE_DB".into(), "work/custom.db".into());
        assert_eq!(
            resolve_opencode_database_paths(&env, home, &[]),
            vec![PathBuf::from("/xdg/data/opencode/work/custom.db")]
        );
        env.insert("OPENCODE_DB".into(), "/absolute/custom.db".into());
        assert_eq!(
            resolve_opencode_database_paths(&env, home, &[]),
            vec![PathBuf::from("/absolute/custom.db")]
        );
    }

    #[test]
    fn opencode_environment_empty_or_nonabsolute_xdg_falls_back_to_home() {
        let home = Path::new("/home/me");
        for xdg in ["", "relative"] {
            let mut env = HashMap::from([("XDG_DATA_HOME".into(), xdg.into())]);
            assert_eq!(
                resolve_opencode_database_paths(&env, home, &[]),
                vec![PathBuf::from("/home/me/.local/share/opencode/opencode.db")]
            );
            env.insert("OPENCODE_DB".into(), "relative.db".into());
            assert_eq!(
                resolve_opencode_database_paths(&env, home, &[]),
                vec![PathBuf::from("/home/me/.local/share/opencode/relative.db")]
            );
            env.insert("OPENCODE_DB".into(), String::new());
            assert_eq!(
                resolve_opencode_database_paths(&env, home, &[]),
                vec![PathBuf::from("/home/me/.local/share/opencode/opencode.db")]
            );
        }
    }

    #[test]
    fn rejects_invalid_opencode_and_unknown_agent_config_atomically() {
        let home = Path::new("/home/me");
        for input in [
            "[agents.opencode]\ndatabase_paths = [\"relative.db\"]",
            "[agents.opencode]\nsession_dirs = [\"/sessions\"]",
            "[agents.opencode]\nunknown = true",
            "[agents.unknown]\ndatabase_paths = [\"/database.db\"]",
        ] {
            assert!(Config::from_toml(input, home).is_err(), "{input}");
        }
    }

    #[test]
    fn watcher_reports_each_changed_invalid_file_once() {
        let temp = tempfile::tempdir().unwrap();
        let mut watcher = ConfigWatcher::new(temp.path(), Path::new("/home/me"));
        assert!(matches!(watcher.poll(), ConfigReload::Updated(_)));
        assert!(matches!(watcher.poll(), ConfigReload::Unchanged));

        fs::write(temp.path().join("config.toml"), "unknown = true").unwrap();
        assert!(matches!(watcher.poll(), ConfigReload::Invalid));
        assert!(matches!(watcher.poll(), ConfigReload::Unchanged));

        fs::write(
            temp.path().join("config.toml"),
            "poll_interval_ms = 3000\n[tab_name]\nenabled = true",
        )
        .unwrap();
        let ConfigReload::Updated(config) = watcher.poll() else {
            panic!("expected valid reload");
        };
        assert_eq!(config.poll_interval_ms, 3_000);
        assert!(config.tab_name.enabled);

        fs::remove_file(temp.path().join("config.toml")).unwrap();
        let ConfigReload::Updated(config) = watcher.poll() else {
            panic!("expected defaults after config removal");
        };
        assert!(!config.tab_name.enabled);
    }

    #[test]
    fn resolves_primary_root_precedence_and_deduplicates_additions() {
        let home = Path::new("/home/me");
        let additions = vec![PathBuf::from("/extra"), PathBuf::from("/extra")];
        let mut env = HashMap::new();
        assert_eq!(
            resolve_pi_agent_dir(&env, home),
            PathBuf::from("/home/me/.pi/agent")
        );
        assert_eq!(
            resolve_session_roots(&env, home, &additions),
            vec![
                PathBuf::from("/home/me/.pi/agent/sessions"),
                PathBuf::from("/extra")
            ]
        );

        env.insert("PI_CODING_AGENT_DIR".into(), "/agent".into());
        assert_eq!(resolve_pi_agent_dir(&env, home), PathBuf::from("/agent"));
        assert_eq!(
            resolve_session_roots(&env, home, &[]),
            vec![PathBuf::from("/agent/sessions")]
        );

        env.insert(
            "PI_CODING_AGENT_SESSION_DIR".into(),
            "~/.pi-sessions".into(),
        );
        assert_eq!(
            resolve_session_roots(&env, home, &[]),
            vec![PathBuf::from("/home/me/.pi-sessions")]
        );

        env.insert("PI_CODING_AGENT_SESSION_DIR".into(), "relative".into());
        assert_eq!(
            resolve_session_roots(&env, home, &[]),
            vec![PathBuf::from("/agent/sessions")]
        );
    }

    #[test]
    fn resolves_claude_project_roots_from_listener_environment() {
        let home = Path::new("/home/me");
        let additions = vec![PathBuf::from("/extra"), PathBuf::from("/extra")];
        let mut env = HashMap::new();
        assert_eq!(
            resolve_claude_project_roots(&env, home, &additions),
            vec![
                PathBuf::from("/home/me/.claude/projects"),
                PathBuf::from("/extra")
            ]
        );

        env.insert("CLAUDE_CONFIG_DIR".into(), "~/.claude-work".into());
        assert_eq!(
            resolve_claude_project_roots(&env, home, &[]),
            vec![PathBuf::from("/home/me/.claude-work/projects")]
        );
    }
}
