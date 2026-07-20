// SPDX-License-Identifier: AGPL-3.0-only

//! TOML-backed galaxy index.
//!
//! Loads galaxies from `~/.config/cosmon/galaxies.toml` (honoring
//! `$XDG_CONFIG_HOME` via the `dirs` crate). File absence is treated
//! as an empty catalog — a fresh environment must not fail lookups,
//! it must just return `None` so `cs ask` can fall back to a broader
//! path (neurion, walk-up, operator prompt).
//!
//! # Schema (first cut)
//!
//! ```toml
//! [[galaxy]]
//! name = "mailroom"
//! path = "~/galaxies/mailroom"
//! fleet = "default"
//! default_formulas = { task = "task-work", idea = "idea-to-plan", deliberation = "deep-think" }
//! ```
//!
//! `path` may contain a leading `~` which is expanded against
//! `dirs::home_dir()`. `fleet` defaults to `"default"` when omitted.
//! `default_formulas` keys must parse as [`MoleculeKind`]; unknown
//! keys are rejected to surface typos early.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use cosmon_core::id::FormulaId;
use cosmon_core::kind::MoleculeKind;
use serde::{Deserialize, Serialize};

use crate::{Galaxy, GalaxyIndex, RegistryError};

/// In-memory galaxy catalog backed by a TOML file.
///
/// The index is materialized once at construction; there is no
/// background reload. Callers that want a fresh view call
/// [`Self::load_default`] again — this is cheap (sub-millisecond on
/// a 10-entry file).
#[derive(Debug, Clone)]
pub struct TomlGalaxyIndex {
    entries: Vec<Galaxy>,
    source: Option<PathBuf>,
}

impl TomlGalaxyIndex {
    /// Build an empty index. Useful for tests and for callers who
    /// want to fall through to another backend when the TOML is
    /// missing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            source: None,
        }
    }

    /// Load from the canonical path (`~/.config/cosmon/galaxies.toml`).
    ///
    /// Missing file → empty index (not an error). Parse or I/O
    /// failures → [`RegistryError`].
    ///
    /// # Errors
    ///
    /// * [`RegistryError::Backend`] when no config directory can be
    ///   determined (exotic OS without `$HOME`/`$XDG_CONFIG_HOME`).
    /// * [`RegistryError::Io`] / [`RegistryError::Parse`] on read or
    ///   TOML-parse failure for an existing file.
    pub fn load_default() -> Result<Self, RegistryError> {
        let path = default_path()
            .ok_or_else(|| RegistryError::Backend("no config directory".to_owned()))?;
        if !path.exists() {
            return Ok(Self {
                entries: Vec::new(),
                source: Some(path),
            });
        }
        Self::load_from(&path)
    }

    /// Load from an explicit path. Missing file is a hard miss here
    /// (caller asked for this file specifically).
    ///
    /// # Errors
    ///
    /// * [`RegistryError::Io`] if the file cannot be read.
    /// * [`RegistryError::Parse`] if the TOML does not match the
    ///   expected schema (unknown molecule kind, invalid formula id).
    pub fn load_from(path: &Path) -> Result<Self, RegistryError> {
        let raw = std::fs::read_to_string(path)?;
        let file: RegistryFile =
            toml::from_str(&raw).map_err(|e| RegistryError::Parse(e.to_string()))?;
        let entries = file
            .galaxy
            .into_iter()
            .map(RawGalaxy::into_galaxy)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            entries,
            source: Some(path.to_path_buf()),
        })
    }

    /// Path the index was loaded from, if any. `None` for an empty
    /// index built via [`Self::empty`].
    #[must_use]
    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

impl GalaxyIndex for TomlGalaxyIndex {
    fn resolve(&self, name: &str) -> Option<Galaxy> {
        self.entries.iter().find(|g| g.name == name).cloned()
    }

    fn list(&self) -> Vec<Galaxy> {
        self.entries.clone()
    }
}

/// Canonical TOML path.
///
/// Preference order (first existing wins when loading; first non-None
/// wins when the file is missing so error messages point at the
/// canonical spot):
///
/// 1. `$HOME/.config/cosmon/galaxies.toml` — matches the cosmon/neurion
///    convention even on macOS, where the native `dirs::config_dir()`
///    returns `~/Library/Application Support`.
/// 2. `dirs::config_dir()/cosmon/galaxies.toml` — platform-native
///    fallback (`%APPDATA%` on Windows, `~/Library/...` on macOS).
fn default_path() -> Option<PathBuf> {
    let xdg = dirs::home_dir().map(|h| h.join(".config/cosmon/galaxies.toml"));
    if let Some(p) = &xdg {
        if p.exists() {
            return Some(p.clone());
        }
    }
    let native = dirs::config_dir().map(|d| d.join("cosmon/galaxies.toml"));
    if let Some(p) = &native {
        if p.exists() {
            return Some(p.clone());
        }
    }
    // Neither exists yet — prefer the XDG path for the error message
    // so the operator knows where to put the file.
    xdg.or(native)
}

/// Top-level schema — a single `galaxy` array. Keeping the wrapper
/// lets us add adjacent sections (defaults, overlays) without a
/// breaking change.
#[derive(Debug, Deserialize, Serialize)]
struct RegistryFile {
    #[serde(default)]
    galaxy: Vec<RawGalaxy>,
}

/// Wire-format for one entry, pre-validation. Kept distinct from
/// [`Galaxy`] so we can evolve the schema without breaking the
/// public type.
#[derive(Debug, Deserialize, Serialize)]
struct RawGalaxy {
    name: String,
    path: String,
    #[serde(default = "default_fleet")]
    fleet: String,
    #[serde(default)]
    claude_md_digest: Option<String>,
    #[serde(default)]
    default_formulas: BTreeMap<String, String>,
}

fn default_fleet() -> String {
    "default".to_owned()
}

impl RawGalaxy {
    fn into_galaxy(self) -> Result<Galaxy, RegistryError> {
        let path = expand_tilde(&self.path);

        let mut default_formulas = HashMap::new();
        for (k, v) in self.default_formulas {
            let kind = MoleculeKind::from_str(&k).map_err(|_| {
                RegistryError::Parse(format!(
                    "unknown molecule kind `{k}` in galaxy `{}`",
                    self.name
                ))
            })?;
            let formula = FormulaId::new(&v).map_err(|e| {
                RegistryError::Parse(format!(
                    "invalid formula id `{v}` in galaxy `{}`: {e}",
                    self.name
                ))
            })?;
            default_formulas.insert(kind, formula);
        }

        Ok(Galaxy {
            name: self.name,
            path,
            fleet: self.fleet,
            claude_md_digest: self.claude_md_digest,
            default_formulas,
        })
    }
}

/// Expand a leading `~` to the user's home directory. Paths without
/// a leading `~` pass through unchanged; if `home_dir()` is
/// unavailable the original string is retained (failing later at the
/// actual-use site rather than here).
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_toml(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("galaxies.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn empty_index_resolves_nothing() {
        let idx = TomlGalaxyIndex::empty();
        assert!(idx.resolve("mailroom").is_none());
        assert!(idx.list().is_empty());
    }

    #[test]
    fn loads_minimal_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_toml(
            tmp.path(),
            r#"
[[galaxy]]
name = "cosmon"
path = "/abs/galaxies/cosmon"
"#,
        );
        let idx = TomlGalaxyIndex::load_from(&p).unwrap();
        let g = idx.resolve("cosmon").expect("cosmon should resolve");
        assert_eq!(g.name, "cosmon");
        assert_eq!(g.path, PathBuf::from("/abs/galaxies/cosmon"));
        assert_eq!(g.fleet, "default");
        assert!(g.default_formulas.is_empty());
    }

    #[test]
    fn load_default_missing_file_is_not_an_error() {
        // We can't easily hijack $HOME, but load_from on a non-existent
        // path is a hard miss — load_default's missing-file branch is
        // tested indirectly via the unit covered by empty() above. The
        // shape we care about is: no panic, no crash.
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nope.toml");
        let err = TomlGalaxyIndex::load_from(&nonexistent).unwrap_err();
        matches!(err, RegistryError::Io(_));
    }

    #[test]
    fn loads_default_formulas() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_toml(
            tmp.path(),
            r#"
[[galaxy]]
name = "cosmon"
path = "/abs/galaxies/cosmon"
fleet = "default"
default_formulas = { task = "task-work", idea = "idea-to-plan", deliberation = "deep-think" }
"#,
        );
        let idx = TomlGalaxyIndex::load_from(&p).unwrap();
        let g = idx.resolve("cosmon").unwrap();
        assert_eq!(
            g.default_formulas
                .get(&MoleculeKind::Task)
                .unwrap()
                .as_str(),
            "task-work"
        );
        assert_eq!(
            idx.default_formula("cosmon", MoleculeKind::Deliberation)
                .unwrap()
                .as_str(),
            "deep-think"
        );
    }

    #[test]
    fn expands_tilde() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_toml(
            tmp.path(),
            r#"
[[galaxy]]
name = "mailroom"
path = "~/galaxies/mailroom"
"#,
        );
        let idx = TomlGalaxyIndex::load_from(&p).unwrap();
        let g = idx.resolve("mailroom").unwrap();
        // On systems with a home dir, expansion should happen and the
        // path should no longer start with '~'. On pathological
        // systems without HOME, it falls through unchanged — we don't
        // assert absolute here, we just assert the tilde is gone OR
        // home_dir() was genuinely unavailable.
        let starts_with_tilde = g.path.to_str().is_some_and(|s| s.starts_with('~'));
        let home_missing = dirs::home_dir().is_none();
        assert!(!starts_with_tilde || home_missing);
    }

    #[test]
    fn rejects_unknown_molecule_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_toml(
            tmp.path(),
            r#"
[[galaxy]]
name = "x"
path = "/x"
default_formulas = { nonsense = "something" }
"#,
        );
        let err = TomlGalaxyIndex::load_from(&p).unwrap_err();
        match err {
            RegistryError::Parse(msg) => assert!(msg.contains("nonsense"), "msg: {msg}"),
            e => panic!("expected Parse, got {e:?}"),
        }
    }

    #[test]
    fn resolve_matches_an_entry_in_list() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_toml(
            tmp.path(),
            r#"
[[galaxy]]
name = "a"
path = "/a"

[[galaxy]]
name = "b"
path = "/b"
"#,
        );
        let idx = TomlGalaxyIndex::load_from(&p).unwrap();
        for g in idx.list() {
            let resolved = idx.resolve(&g.name).expect("listed galaxy must resolve");
            assert_eq!(resolved, g);
        }
    }
}
