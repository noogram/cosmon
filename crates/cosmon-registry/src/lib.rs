// SPDX-License-Identifier: AGPL-3.0-only

//! cosmon-registry — stateless galaxy-name registry.
//!
//! This crate resolves a galaxy *name* to a concrete [`Galaxy`] entry
//! (path, fleet, default formulas). It is the implicit prerequisite
//! behind the conversational ingress verb `cs ask "<free text>"`: that
//! verb needs to turn a galaxy name parsed from free text into a
//! concrete galaxy entry before it can dispatch any work.
//!
//! # Why not a daemon?
//!
//! The alternative — a long-lived process that watches `$HOME` and
//! caches galaxies in memory — was rejected by the panel (architect,
//! ADR-054). Cosmon stays on the stateless side of the fence:
//!
//! * One source of truth on disk (`~/.config/cosmon/galaxies.toml`).
//! * In-memory index rebuilt at each invocation; cold-load is
//!   sub-millisecond on a 10-entry TOML.
//! * Optional neurion fallback for the case where the TOML is missing.
//!
//! # What the registry is not
//!
//! * It is **not** a write API. The operator edits the TOML by hand;
//!   a future `cs galaxies register <name>` verb is a separate molecule.
//! * It is **not** a presence or auth layer. Identity is out of scope.
//! * It is **not** a coupling path for cross-galaxy messaging. Names
//!   resolve to paths; nothing else flows through here.
//!
//! See [ADR-070](../../../docs/adr/070-cosmon-registry.md) for the TOML
//! schema and neurion-mirror relationship.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::HashMap;
use std::path::PathBuf;

use cosmon_core::id::FormulaId;
use cosmon_core::kind::MoleculeKind;
use serde::{Deserialize, Serialize};

pub mod toml_backend;

#[cfg(feature = "neurion-fallback")]
pub mod neurion_backend;

pub use toml_backend::TomlGalaxyIndex;

#[cfg(feature = "neurion-fallback")]
pub use neurion_backend::NeurionBackedGalaxyIndex;

/// A resolved galaxy entry.
///
/// All fields describe *where* the galaxy lives and *how* cosmon should
/// talk to it. Name-resolution is the only job; nothing in this struct
/// touches the galaxy's mutable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Galaxy {
    /// Short canonical name (`mailroom`, `cosmon`, `earshot`, …).
    /// This is the key operators type into `cs ask "<name>: …"`.
    pub name: String,

    /// Absolute path to the galaxy's root directory (worktree root or
    /// bare repo). Symlinks are not resolved here — the consumer can
    /// `canonicalize` if needed.
    pub path: PathBuf,

    /// Fleet name this galaxy dispatches into. Defaults to `"default"`
    /// when the TOML entry omits the field — cosmon's own convention.
    pub fleet: String,

    /// Optional informational BLAKE3 digest of the galaxy's `CLAUDE.md`.
    /// Purely advisory — UI can show staleness against the actual file;
    /// it is never consulted on the load path.
    pub claude_md_digest: Option<String>,

    /// Default formula per molecule kind. Lets `cs ask` pick a sane
    /// pipeline when the operator did not name one explicitly (e.g.
    /// "I want a deep-think on X" → `deep-think`).
    pub default_formulas: HashMap<MoleculeKind, FormulaId>,
}

/// Read-only view over the galaxy catalog.
///
/// Implementations are stateless and cheap to construct — the contract
/// is that every call re-materializes the catalog (or a cached slice
/// of it). No background threads, no interior mutability, no I/O
/// beyond construction.
pub trait GalaxyIndex {
    /// Resolve a short name to its full entry. Returns `None` if the
    /// name is unknown. Name matching is case-sensitive; canonicalizing
    /// the input is the caller's responsibility.
    fn resolve(&self, name: &str) -> Option<Galaxy>;

    /// Enumerate every galaxy in the catalog. Order is arbitrary but
    /// stable across two calls on the same backend instance.
    fn list(&self) -> Vec<Galaxy>;

    /// Look up the default formula for `(galaxy, kind)`. Convenience
    /// shortcut for the `cs ask` ingress.
    fn default_formula(&self, galaxy: &str, kind: MoleculeKind) -> Option<FormulaId> {
        self.resolve(galaxy)
            .and_then(|g| g.default_formulas.get(&kind).cloned())
    }
}

/// Errors surfaced by a registry backend.
///
/// The intent is that *missing* sources (the TOML file not existing
/// yet in a fresh environment) are not errors — callers should see
/// an empty index, not a failure. Genuine I/O or parse failures are
/// modeled here.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The registry source file exists but could not be read.
    #[error("registry I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The registry source file exists but could not be parsed as TOML.
    #[error("registry parse error: {0}")]
    Parse(String),

    /// The underlying storage medium (neurion `SQLite`, …) failed.
    #[error("registry backend error: {0}")]
    Backend(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub backend — no data, sanity-check `default_formula()`'s default impl.
    struct Empty;

    impl GalaxyIndex for Empty {
        fn resolve(&self, _name: &str) -> Option<Galaxy> {
            None
        }

        fn list(&self) -> Vec<Galaxy> {
            Vec::new()
        }
    }

    #[test]
    fn default_formula_on_empty_index_is_none() {
        let idx = Empty;
        assert!(idx
            .default_formula("mailroom", MoleculeKind::Task)
            .is_none());
    }
}
