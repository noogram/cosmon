// SPDX-License-Identifier: AGPL-3.0-only

//! Pure worker-host selection. Client UI and model adapter choices are separate.

use serde::{Deserialize, Serialize};

/// Optional configuration preserves the difference between absence and an explicit tmux pin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Worker host requested by the operator; validation happens before dispatch effects.
    pub backend: Option<String>,
}

/// Qualified worker hosts. Adding a value requires a working adapter and conformance evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// The existing local tmux worker host.
    Tmux,
    /// Herdr protocol 22, restricted to supervised Codex Exec workers.
    Herdr,
}

/// Which configuration supplied the choice; explicit tmux must remain distinguishable from default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportSource {
    /// A project explicitly selected this host.
    Project,
    /// The isolated or operator-wide configuration selected this host.
    Global,
    /// Neither configuration specified a host.
    Default,
}

/// Durable selection for one dispatch, independent of future configuration changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportSelection {
    /// Qualified implementation to compose.
    pub backend: TransportKind,
    /// Configuration provenance at launch time.
    pub source: TransportSource,
}

/// A rejected configuration is never interpreted as permission to use the default.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportSelectionError {
    /// A recognised future host has no qualified implementation.
    #[error(
        "transport backend `{0}` is not implemented; `tmux` and supervised `herdr` are available"
    )]
    Unavailable(String),
    /// An unrecognised name is usually a typo and must be surfaced.
    #[error("unknown transport backend `{0}`; `tmux` and supervised `herdr` are available")]
    Unknown(String),
}

/// Resolve project > global > tmux without ambient environment or I/O.
///
/// # Errors
/// Refuses unknown and unimplemented hosts, including explicit empty values.
pub fn resolve_transport(
    project: &TransportConfig,
    global: &TransportConfig,
) -> Result<TransportSelection, TransportSelectionError> {
    let (name, source) = if let Some(name) = project.backend.as_deref() {
        (name, TransportSource::Project)
    } else if let Some(name) = global.backend.as_deref() {
        (name, TransportSource::Global)
    } else {
        ("tmux", TransportSource::Default)
    };
    let backend = match name {
        "tmux" => TransportKind::Tmux,
        "herdr" => TransportKind::Herdr,
        "zellij" => return Err(TransportSelectionError::Unavailable(name.into())),
        _ => return Err(TransportSelectionError::Unknown(name.into())),
    };
    Ok(TransportSelection { backend, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str) -> TransportConfig {
        TransportConfig {
            backend: Some(name.into()),
        }
    }

    #[test]
    fn absence_and_explicit_values_keep_their_provenance() -> Result<(), TransportSelectionError> {
        let absent = TransportConfig::default();
        assert_eq!(
            resolve_transport(&absent, &absent)?.source,
            TransportSource::Default
        );
        assert_eq!(
            resolve_transport(&absent, &config("tmux"))?.source,
            TransportSource::Global
        );
        assert_eq!(
            resolve_transport(&config("tmux"), &config("herdr"))?.source,
            TransportSource::Project
        );
        Ok(())
    }

    #[test]
    fn explicit_refusals_never_fall_through() {
        for name in ["zellij", "typo", "", "TMUX"] {
            assert!(resolve_transport(&config(name), &config("tmux")).is_err());
            assert!(resolve_transport(&TransportConfig::default(), &config(name)).is_err());
        }
    }

    #[test]
    fn missing_section_and_missing_key_both_inherit() -> Result<(), Box<dyn std::error::Error>> {
        for text in ["", "[transport]\n", "[worker]\non_complete='commit'\n"] {
            let parsed = crate::config::ProjectConfig::parse(text)?;
            assert_eq!(parsed.transport.backend, None);
            assert_eq!(
                resolve_transport(&parsed.transport, &config("tmux"))?.source,
                TransportSource::Global
            );
        }
        Ok(())
    }
}
