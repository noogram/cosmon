// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule view.
//!
//! The observability view of a molecule carries only the fields both the
//! TUI and the HTTP dashboard need — identity, status, kind, and the
//! worker/session that currently hosts it. Domain logic (typestate
//! transitions) stays in `cosmon-core`; this is a read-only projection.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Newtype wrapper for a molecule identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MoleculeId(pub String);

impl std::fmt::Display for MoleculeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for MoleculeId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Observed status of a molecule — re-exported from the domain core.
///
/// This crate used to declare its own six-variant copy of this enum, and the
/// TUI bridged the two with a `map_status` function. The bridge was lossy in
/// both directions: it renamed `Starved` (a *live* status whose whole purpose
/// is to summon the operator, ADR-062) to a `Stuck` variant with no referent
/// in the core, and it laundered `Queued` — plus every future
/// `#[non_exhaustive]` variant — into `Pending` through a `_ =>` arm. That
/// turned an *erasure* (detectable, recoverable) into a *substitution error*
/// (undetectable, propagating as confident data), and it silently defeated
/// the unknown-status passthrough that [`crate::render`]'s consumers rely on.
///
/// There is one alphabet now. An observability projection may drop *fields*
/// the operator does not need; it must never re-letter the values, because no
/// downstream partition can recover a bit that was destroyed here.
pub use cosmon_core::molecule::MoleculeStatus;

/// The observability projection of a molecule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Molecule {
    /// Stable molecule id (e.g. `task-20260412-a22b`).
    pub id: MoleculeId,
    /// Human-readable summary or title.
    pub title: String,
    /// Molecule kind marker (e.g. `"task"`, `"issue"`, `"decision"`).
    pub kind: String,
    /// Current status.
    pub status: MoleculeStatus,
    /// Project root the molecule belongs to.
    pub project_root: String,
    /// Tmux session id hosting the worker, if any.
    pub session: Option<String>,
    /// Last observed update timestamp.
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_from_str_roundtrip() {
        let id: MoleculeId = "mol-1".into();
        assert_eq!(id.to_string(), "mol-1");
    }

    #[test]
    fn status_serializes_snake_case() {
        let j = serde_json::to_string(&MoleculeStatus::Completed).unwrap();
        assert_eq!(j, "\"completed\"");
    }
}
