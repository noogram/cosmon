// SPDX-License-Identifier: AGPL-3.0-only

//! Galaxy-alias resolution and cross-galaxy molecule lookup.
//!
//! Phase 1 of [ADR-035](../../../../docs/adr/035-cross-galaxy-edges.md).
//! The CLI accepts cross-galaxy molecule references in the form
//! `<alias>:<molecule-id>` (or `<alias>@<molecule-id>`) on flags like
//! `--blocked-by`. This module owns the *physical* side of those
//! references:
//!
//! 1. Map a galaxy alias to a filesystem path. The lookup chain is:
//!    a. [`cosmon_registry::TomlGalaxyIndex`] (the canonical
//!    `~/.config/cosmon/galaxies.toml` registry).
//!    b. The `~/.cosmon/galaxy-aliases.toml` override file (a small
//!    per-user fallback that does not require touching the canonical
//!    registry — useful for ad-hoc aliasing during cross-galaxy
//!    sessions).
//!    c. The convention path `/srv/cosmon/<alias>/` if it contains a
//!    `.cosmon/` directory.
//! 2. Walk to the remote galaxy's state store, attempt to load the
//!    referenced molecule, and surface a [`CrossGalaxyResolution`]
//!    that downstream commands (`cs deps`, `cs observe`, `cs wait`)
//!    can render without panicking when the galaxy is offline.
//!
//! The resolver is deliberately *advisory*: a missing galaxy or a
//! missing molecule yields a degraded resolution, never a hard error.
//! That mirrors ADR-035 §6 ("network partition / galaxy offline →
//! `StaleEdge` event") — the local DAG must keep moving even when the
//! remote galaxy is unreachable.

// Read-side surface (`resolve_cross_galaxy_ref`, `is_resolved`,
// `is_terminal`, override-file helpers) is consumed incrementally by
// `cs deps` / `cs observe` / `cs wait`. The `cs nucleate` warning path
// already calls `resolve_cross_galaxy_ref`. Allow(dead_code) keeps the
// surface stable while the read-side wires up.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cosmon_core::interaction::CrossGalaxyRef;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_registry::{GalaxyIndex, TomlGalaxyIndex};
use serde::{Deserialize, Serialize};

/// Outcome of resolving a [`CrossGalaxyRef`] against the local file
/// system.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CrossGalaxyResolution {
    /// The galaxy and the molecule were both found, and the molecule's
    /// status was read.
    Resolved {
        /// Filesystem path to the resolved galaxy's `.cosmon/` directory.
        galaxy_path: PathBuf,
        /// The molecule's current status (terminal or live).
        status: MoleculeStatus,
    },
    /// The galaxy was located on disk but the molecule could not be
    /// loaded (missing, mistyped, or removed).
    MoleculeMissing {
        /// Filesystem path to the resolved galaxy's `.cosmon/` directory.
        galaxy_path: PathBuf,
    },
    /// The galaxy alias could not be mapped to a filesystem path. The
    /// edge is recorded but cannot be checked locally.
    GalaxyUnknown,
}

impl CrossGalaxyResolution {
    /// Did the resolver successfully locate a molecule, regardless of
    /// its terminal state?
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved { .. })
    }

    /// Is the referenced molecule in a terminal state (`Completed`,
    /// `Collapsed`, …)? Returns `false` when the resolution is a miss.
    /// Used by `cs wait` to decide whether the edge is satisfied.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Resolved { status, .. } => status.is_terminal(),
            _ => false,
        }
    }
}

/// Resolve a galaxy alias to its `.cosmon/` directory.
///
/// Returns `None` when no source on disk knows about the alias. The
/// caller is expected to treat that as an "unknown" resolution rather
/// than a fatal error — same pattern as `git` falling through to the
/// next remote when one is unreachable.
#[must_use]
pub fn resolve_galaxy_path(alias: &str) -> Option<PathBuf> {
    if let Ok(idx) = TomlGalaxyIndex::load_default() {
        if let Some(galaxy) = idx.resolve(alias) {
            let cosmon_dir = galaxy.path.join(".cosmon");
            if cosmon_dir.is_dir() {
                return Some(cosmon_dir);
            }
        }
    }
    if let Some(p) = lookup_override_alias(alias) {
        let cosmon_dir = p.join(".cosmon");
        if cosmon_dir.is_dir() {
            return Some(cosmon_dir);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let convention = home.join("galaxies").join(alias);
        let cosmon_dir = convention.join(".cosmon");
        if cosmon_dir.is_dir() {
            return Some(cosmon_dir);
        }
    }
    None
}

/// Resolve a [`CrossGalaxyRef`] end-to-end: map the alias, walk to the
/// remote `.cosmon/state/`, and read the target molecule's status.
///
/// Never returns an error — every failure path collapses into a variant
/// of [`CrossGalaxyResolution`] so the calling command can render the
/// edge with a `[unknown]` / `[missing]` hint rather than crashing.
#[must_use]
pub fn resolve_cross_galaxy_ref(reference: &CrossGalaxyRef) -> CrossGalaxyResolution {
    let Some(galaxy_path) = resolve_galaxy_path(&reference.galaxy) else {
        return CrossGalaxyResolution::GalaxyUnknown;
    };
    // The state store lives under `<galaxy>/.cosmon/state/`. We pass
    // the `state/` subdirectory to `FileStore::new` to mirror the
    // walk-up discovery used by the rest of the CLI.
    let state_dir = galaxy_path.join("state");
    if !state_dir.is_dir() {
        return CrossGalaxyResolution::MoleculeMissing { galaxy_path };
    }
    let store = super::open_store(&state_dir);
    match store.load_molecule(&reference.mol_id) {
        Ok(mol) => CrossGalaxyResolution::Resolved {
            galaxy_path,
            status: mol.status,
        },
        Err(_) => CrossGalaxyResolution::MoleculeMissing { galaxy_path },
    }
}

// ---------------------------------------------------------------------------
// Override file: `~/.cosmon/galaxy-aliases.toml`
// ---------------------------------------------------------------------------

/// TOML schema for the per-user override file.
///
/// ```toml
/// [aliases]
/// mailroom = "~/work/mailroom"
/// tenant-demo = "/abs/path/to/tenant-demo"
/// ```
#[derive(Debug, Deserialize, Serialize, Default)]
struct AliasOverrideFile {
    #[serde(default)]
    aliases: HashMap<String, String>,
}

fn lookup_override_alias(alias: &str) -> Option<PathBuf> {
    let path = override_file_path()?;
    if !path.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: AliasOverrideFile = toml::from_str(&raw).ok()?;
    let raw_path = parsed.aliases.get(alias)?;
    Some(expand_tilde(raw_path))
}

fn override_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cosmon").join("galaxy-aliases.toml"))
}

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

/// Test-only resolver that bypasses the user's home directory and
/// reads only the override-file path passed in. Used by integration
/// tests to avoid depending on whatever the developer happens to have
/// in `~/.config/cosmon/galaxies.toml`.
#[doc(hidden)]
#[must_use]
pub fn resolve_galaxy_path_with_override_file(
    alias: &str,
    override_file: &Path,
) -> Option<PathBuf> {
    if !override_file.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(override_file).ok()?;
    let parsed: AliasOverrideFile = toml::from_str(&raw).ok()?;
    let raw_path = parsed.aliases.get(alias)?;
    let candidate = expand_tilde(raw_path);
    let cosmon_dir = candidate.join(".cosmon");
    if cosmon_dir.is_dir() {
        Some(cosmon_dir)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::MoleculeId;

    #[test]
    fn galaxy_unknown_when_alias_has_no_source() {
        let cgr = CrossGalaxyRef::new(
            "definitely-not-a-real-galaxy-alias",
            MoleculeId::new("task-20260425-aaaa").unwrap(),
        );
        let res = resolve_cross_galaxy_ref(&cgr);
        // Either GalaxyUnknown (no registry, no convention path) or
        // MoleculeMissing (some operator created `/srv/cosmon/...` for
        // an unrelated reason). Both are valid "miss" outcomes.
        assert!(!res.is_resolved(), "expected miss, got {res:?}");
    }

    #[test]
    fn override_file_resolves_alias_to_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let galaxy_root = tmp.path().join("test-galaxy");
        std::fs::create_dir_all(galaxy_root.join(".cosmon/state/fleets/default/molecules"))
            .unwrap();

        let override_path = tmp.path().join("aliases.toml");
        std::fs::write(
            &override_path,
            format!("[aliases]\ntest-galaxy = \"{}\"\n", galaxy_root.display()),
        )
        .unwrap();

        let resolved =
            resolve_galaxy_path_with_override_file("test-galaxy", &override_path).unwrap();
        assert!(resolved.ends_with(".cosmon"));
        assert!(resolved.is_dir());
    }

    #[test]
    fn override_file_misses_when_alias_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().join("aliases.toml");
        std::fs::write(&override_path, "[aliases]\nsomething-else = \"/tmp\"\n").unwrap();
        let resolved = resolve_galaxy_path_with_override_file("not-here", &override_path);
        assert!(resolved.is_none());
    }

    #[test]
    fn override_file_misses_when_path_has_no_cosmon_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().join("aliases.toml");
        std::fs::write(
            &override_path,
            format!(
                "[aliases]\ntest = \"{}\"\n",
                tmp.path().join("no-cosmon").display()
            ),
        )
        .unwrap();
        // Path exists implicitly under tmp but `.cosmon/` does not.
        let resolved = resolve_galaxy_path_with_override_file("test", &override_path);
        assert!(resolved.is_none());
    }

    #[test]
    fn missing_override_file_is_a_none_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nope.toml");
        let resolved = resolve_galaxy_path_with_override_file("anything", &nonexistent);
        assert!(resolved.is_none());
    }
}
