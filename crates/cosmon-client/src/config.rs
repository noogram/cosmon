// SPDX-License-Identifier: Apache-2.0

//! Client configuration — loaded from `~/.config/cosmon-client/config.toml`
//! with env-var and flag overrides.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// On-disk representation of the client config (TOML).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FileConfig {
    pub server: Option<String>,
    pub api_key: Option<String>,
    /// Local directory where artifacts are downloaded (default `./cosmon-artifacts`).
    pub artifacts_dir: Option<PathBuf>,
}

/// Resolved, ready-to-use config.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server: String,
    pub api_key: String,
    pub artifacts_dir: PathBuf,
}

/// Overrides from CLI flags (highest priority).
#[derive(Debug, Default)]
pub struct ConfigOverrides {
    pub server: Option<String>,
    pub api_key: Option<String>,
    pub artifacts_dir: Option<PathBuf>,
}

impl ClientConfig {
    pub fn load(overrides: ConfigOverrides) -> anyhow::Result<Self> {
        let file = load_file_config()?;

        let server = overrides
            .server
            .or_else(|| std::env::var("COSMON_CLIENT_SERVER").ok())
            .or(file.server)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "server URL not set. Provide --server, COSMON_CLIENT_SERVER, \
                     or server in ~/.config/cosmon-client/config.toml"
                )
            })?;

        let api_key = overrides
            .api_key
            .or_else(|| std::env::var("COSMON_CLIENT_API_KEY").ok())
            .or(file.api_key)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "API key not set. Provide --api-key, COSMON_CLIENT_API_KEY, \
                     or api_key in ~/.config/cosmon-client/config.toml"
                )
            })?;

        let artifacts_dir = overrides
            .artifacts_dir
            .or_else(|| {
                std::env::var("COSMON_CLIENT_ARTIFACTS")
                    .ok()
                    .map(PathBuf::from)
            })
            .or(file.artifacts_dir)
            .unwrap_or_else(|| PathBuf::from("./cosmon-artifacts"));

        Ok(ClientConfig {
            server,
            api_key,
            artifacts_dir,
        })
    }
}

fn load_file_config() -> anyhow::Result<FileConfig> {
    let Some(base) = dirs::config_dir() else {
        return Ok(FileConfig::default());
    };
    let path = base.join("cosmon-client").join("config.toml");
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {path:?}: {e}"))?;
    toml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid config {path:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_config_partial_ok() {
        let c: FileConfig = toml::from_str(r#"server = "https://x""#).unwrap();
        assert_eq!(c.server.as_deref(), Some("https://x"));
        assert!(c.api_key.is_none());
    }
}
