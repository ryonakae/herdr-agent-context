use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub const DEFAULT_POLL_INTERVAL_MS: u64 = 2_000;
pub const DEFAULT_METADATA_TTL_MS: u64 = 10_000;
pub const MAX_METADATA_TTL_MS: u64 = 86_400_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub metadata_ttl_ms: u64,
    pub pi_session_dirs: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            metadata_ttl_ms: DEFAULT_METADATA_TTL_MS,
            pi_session_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    poll_interval_ms: Option<u64>,
    metadata_ttl_ms: Option<u64>,
    pi_session_dirs: Option<Vec<PathBuf>>,
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
    #[error("a configured Pi session directory is not absolute after expansion")]
    RelativeSessionDir,
}

impl Config {
    pub fn from_toml(input: &str, home: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(input).map_err(ConfigError::Parse)?;
        let config = Self {
            poll_interval_ms: raw.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS),
            metadata_ttl_ms: raw.metadata_ttl_ms.unwrap_or(DEFAULT_METADATA_TTL_MS),
            pi_session_dirs: raw
                .pi_session_dirs
                .unwrap_or_default()
                .into_iter()
                .map(|path| normalize_config_path(&path, home))
                .collect::<Result<Vec<_>, _>>()?,
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

pub fn resolve_session_roots(
    env: &HashMap<String, String>,
    home: &Path,
    additional: &[PathBuf],
) -> Vec<PathBuf> {
    let primary = env
        .get("PI_CODING_AGENT_SESSION_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env.get("PI_CODING_AGENT_DIR")
                .filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join("sessions"))
        })
        .unwrap_or_else(|| home.join(".pi/agent/sessions"));

    let mut roots = Vec::new();
    for path in std::iter::once(primary).chain(additional.iter().cloned()) {
        let path = canonical_or_normalized(path);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
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
    fn resolves_primary_root_precedence_and_deduplicates_additions() {
        let home = Path::new("/home/me");
        let additions = vec![PathBuf::from("/extra"), PathBuf::from("/extra")];
        let mut env = HashMap::new();
        assert_eq!(
            resolve_session_roots(&env, home, &additions),
            vec![
                PathBuf::from("/home/me/.pi/agent/sessions"),
                PathBuf::from("/extra")
            ]
        );

        env.insert("PI_CODING_AGENT_DIR".into(), "/agent".into());
        assert_eq!(
            resolve_session_roots(&env, home, &[]),
            vec![PathBuf::from("/agent/sessions")]
        );

        env.insert("PI_CODING_AGENT_SESSION_DIR".into(), "/sessions".into());
        assert_eq!(
            resolve_session_roots(&env, home, &[]),
            vec![PathBuf::from("/sessions")]
        );
    }
}
