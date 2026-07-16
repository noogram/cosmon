// SPDX-License-Identifier: AGPL-3.0-only

//! Configuration for mindguard `surface_visual`.
//!
//! Loaded from `~/.config/cosmon/mindguard-surface.toml` when present,
//! falling back to compiled-in defaults so the gate is fail-closed
//! *even on a host that never declared the config file*. A missing
//! config does not mean "no gate" — it means "the same gate as the
//! default ships".
//!
//! Schema:
//! ```toml
//! [surface]
//! # Glob patterns (relative to project root) that mark a molecule as
//! # touching the visual surface. Any non-empty intersection with the
//! # molecule's diff promotes the molecule to surface=touched.
//! paths = [
//!     "poc/optix-modernization/lumen/web/**",
//!     "wiki/**",
//!     "**/*.html",
//!     "**/*.css",
//!     "**/*.js",
//! ]
//!
//! [verify]
//! # Maximum age of a verify-surface molecule's GREEN landing relative
//! # to the cs-complete claim, in minutes. Anti-drift; default 60.
//! t_max_minutes = 60
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Resolved configuration for the `surface_visual` gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceConfig {
    /// Glob patterns (relative to project root) that, if matched by any
    /// file in the molecule's diff, mark the molecule as touching the
    /// visual surface.
    pub paths: Vec<String>,

    /// Maximum age of a GREEN verify-surface landing relative to the
    /// `cs complete` claim.
    pub t_max: chrono::Duration,
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        Self {
            paths: default_paths().into_iter().map(ToOwned::to_owned).collect(),
            t_max: chrono::Duration::minutes(60),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    surface: RawSurfaceSection,
    #[serde(default)]
    verify: RawVerifySection,
}

#[derive(Debug, Deserialize, Default)]
struct RawSurfaceSection {
    #[serde(default)]
    paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawVerifySection {
    #[serde(default)]
    t_max_minutes: Option<i64>,
}

/// Default glob patterns for surface=touched detection.
///
/// Inscribed cosmon-ward so the same firebreak applies across every
/// galaxy that uses `cs complete`. Overridable per-host via the TOML
/// config, never silenceable.
fn default_paths() -> Vec<&'static str> {
    vec![
        "poc/optix-modernization/lumen/web/**",
        "wiki/**",
        "**/*.html",
        "**/*.css",
        "**/*.js",
    ]
}

/// Resolve the config file path.
///
/// Precedence:
/// 1. `$COSMON_MINDGUARD_SURFACE_CONFIG` (test/CI override).
/// 2. `$XDG_CONFIG_HOME/cosmon/mindguard-surface.toml`.
/// 3. `$HOME/.config/cosmon/mindguard-surface.toml`.
#[must_use]
pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("COSMON_MINDGUARD_SURFACE_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| {
            std::env::var("HOME")
                .map_or_else(|_| PathBuf::from("."), |h| Path::new(&h).join(".config"))
        },
        PathBuf::from,
    );
    base.join("cosmon").join("mindguard-surface.toml")
}

/// Load configuration from the default path, or return defaults.
///
/// A missing file is not an error — defaults apply. A *malformed*
/// file is an error: refusing rather than silently falling back keeps
/// operators honest about config drift.
///
/// # Errors
///
/// Returns the parse error if the TOML file exists but is invalid.
pub fn load() -> Result<SurfaceConfig, String> {
    load_from(&default_path())
}

/// Load configuration from a specific path.
///
/// # Errors
///
/// Returns the parse error if the TOML file exists but is invalid.
pub fn load_from(path: &Path) -> Result<SurfaceConfig, String> {
    if !path.exists() {
        return Ok(SurfaceConfig::default());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: RawConfig =
        toml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let defaults = SurfaceConfig::default();
    Ok(SurfaceConfig {
        paths: parsed.surface.paths.unwrap_or(defaults.paths),
        t_max: parsed
            .verify
            .t_max_minutes
            .map_or(defaults.t_max, chrono::Duration::minutes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn defaults_are_fail_closed_with_visual_globs() {
        let cfg = SurfaceConfig::default();
        assert!(cfg.paths.iter().any(|p| p.ends_with("*.html")));
        assert!(cfg.paths.iter().any(|p| p.ends_with("*.css")));
        assert!(cfg.paths.iter().any(|p| p.ends_with("*.js")));
        assert_eq!(cfg.t_max, chrono::Duration::minutes(60));
    }

    #[test]
    fn missing_file_returns_defaults() {
        let cfg = load_from(Path::new("/nonexistent/path/to/nowhere.toml")).unwrap();
        assert_eq!(cfg, SurfaceConfig::default());
    }

    #[test]
    fn explicit_config_overrides_defaults() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[surface]
paths = ["custom/**/*.html"]

[verify]
t_max_minutes = 30
"#
        )
        .unwrap();
        let cfg = load_from(f.path()).unwrap();
        assert_eq!(cfg.paths, vec!["custom/**/*.html".to_owned()]);
        assert_eq!(cfg.t_max, chrono::Duration::minutes(30));
    }

    #[test]
    fn partial_config_keeps_defaults_for_missing_sections() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "\n[verify]\nt_max_minutes = 15\n").unwrap();
        let cfg = load_from(f.path()).unwrap();
        // Defaults retained for [surface].
        assert_eq!(cfg.paths, SurfaceConfig::default().paths);
        // Override applied for [verify].
        assert_eq!(cfg.t_max, chrono::Duration::minutes(15));
    }

    #[test]
    fn malformed_file_errors_rather_than_silent_fallback() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "this is not = valid toml [[[").unwrap();
        let err = load_from(f.path()).unwrap_err();
        assert!(err.contains("parse"));
    }
}
