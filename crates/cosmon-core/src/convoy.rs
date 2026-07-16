// SPDX-License-Identifier: AGPL-3.0-only

//! Convoy — an ordered group of molecules.
//!
//! A convoy represents a coordinated batch of work: multiple molecules that
//! belong together and execute in a defined order. In the Gas Town pattern,
//! convoys are dispatched as units and tracked to completion as a group.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::convoy::{Convoy, ConvoyStatus};
//! use cosmon_core::id::ConvoyId;
//!
//! let id = ConvoyId::new("convoy-alpha").unwrap();
//! let convoy = Convoy::new(id, "Alpha batch".to_owned());
//! assert_eq!(convoy.status(), ConvoyStatus::Pending);
//! assert!(convoy.molecules().is_empty());
//! ```

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::id::{ConvoyId, MoleculeId};

// ---------------------------------------------------------------------------
// ConvoyStatus
// ---------------------------------------------------------------------------

/// Lifecycle status of a convoy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvoyStatus {
    /// Convoy created but no molecules have started.
    Pending,
    /// At least one molecule is active.
    Active,
    /// All molecules completed successfully.
    Completed,
    /// One or more molecules collapsed; convoy cannot proceed.
    Failed,
}

impl fmt::Display for ConvoyStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => f.write_str("pending"),
            Self::Active => f.write_str("active"),
            Self::Completed => f.write_str("completed"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

impl FromStr for ConvoyStatus {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(ParseEnumError {
                type_name: "ConvoyStatus",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Convoy
// ---------------------------------------------------------------------------

/// An ordered group of molecules that are tracked and dispatched together.
///
/// The molecule list preserves insertion order — molecules earlier in the
/// vector are logically upstream of later ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Convoy {
    id: ConvoyId,
    name: String,
    molecules: Vec<MoleculeId>,
    status: ConvoyStatus,
}

impl Convoy {
    /// Create a new convoy in the Pending state with no molecules.
    #[must_use]
    pub fn new(id: ConvoyId, name: String) -> Self {
        Self {
            id,
            name,
            molecules: Vec::new(),
            status: ConvoyStatus::Pending,
        }
    }

    /// The convoy's unique identifier.
    #[must_use]
    pub fn id(&self) -> &ConvoyId {
        &self.id
    }

    /// Human-readable name for this convoy.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The ordered list of molecule IDs in this convoy.
    #[must_use]
    pub fn molecules(&self) -> &[MoleculeId] {
        &self.molecules
    }

    /// The current lifecycle status.
    #[must_use]
    pub fn status(&self) -> ConvoyStatus {
        self.status
    }

    /// Append a molecule to the end of the convoy's ordered list.
    pub fn push_molecule(&mut self, molecule_id: MoleculeId) {
        self.molecules.push(molecule_id);
    }

    /// Transition to the Active status.
    pub fn activate(&mut self) {
        self.status = ConvoyStatus::Active;
    }

    /// Transition to the Completed status.
    pub fn complete(&mut self) {
        self.status = ConvoyStatus::Completed;
    }

    /// Transition to the Failed status.
    pub fn fail(&mut self) {
        self.status = ConvoyStatus::Failed;
    }
}

impl fmt::Display for Convoy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Convoy({}, \"{}\", {} molecules, {})",
            self.id,
            self.name,
            self.molecules.len(),
            self.status
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_convoy_id() -> ConvoyId {
        ConvoyId::new("convoy-alpha").unwrap()
    }

    fn test_mol_id(suffix: &str) -> MoleculeId {
        MoleculeId::new(format!("cs-20260401-{suffix}")).unwrap()
    }

    #[test]
    fn test_convoy_new_is_pending_and_empty() {
        let convoy = Convoy::new(test_convoy_id(), "Alpha batch".to_owned());
        assert_eq!(convoy.id().as_str(), "convoy-alpha");
        assert_eq!(convoy.name(), "Alpha batch");
        assert!(convoy.molecules().is_empty());
        assert_eq!(convoy.status(), ConvoyStatus::Pending);
    }

    #[test]
    fn test_convoy_push_preserves_order() {
        let mut convoy = Convoy::new(test_convoy_id(), "ordered".to_owned());
        convoy.push_molecule(test_mol_id("aaaa"));
        convoy.push_molecule(test_mol_id("bbbb"));
        convoy.push_molecule(test_mol_id("cccc"));

        assert_eq!(convoy.molecules().len(), 3);
        assert_eq!(convoy.molecules()[0].suffix(), "aaaa");
        assert_eq!(convoy.molecules()[1].suffix(), "bbbb");
        assert_eq!(convoy.molecules()[2].suffix(), "cccc");
    }

    #[test]
    fn test_convoy_status_transitions() {
        let mut convoy = Convoy::new(test_convoy_id(), "lifecycle".to_owned());
        assert_eq!(convoy.status(), ConvoyStatus::Pending);

        convoy.activate();
        assert_eq!(convoy.status(), ConvoyStatus::Active);

        convoy.complete();
        assert_eq!(convoy.status(), ConvoyStatus::Completed);
    }

    #[test]
    fn test_convoy_fail_transition() {
        let mut convoy = Convoy::new(test_convoy_id(), "will-fail".to_owned());
        convoy.activate();
        convoy.fail();
        assert_eq!(convoy.status(), ConvoyStatus::Failed);
    }

    #[test]
    fn test_convoy_status_display_roundtrip() {
        for status in [
            ConvoyStatus::Pending,
            ConvoyStatus::Active,
            ConvoyStatus::Completed,
            ConvoyStatus::Failed,
        ] {
            let s = status.to_string();
            let parsed: ConvoyStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_convoy_status_parse_invalid() {
        assert!("bogus".parse::<ConvoyStatus>().is_err());
    }

    #[test]
    fn test_convoy_display() {
        let mut convoy = Convoy::new(test_convoy_id(), "Alpha batch".to_owned());
        convoy.push_molecule(test_mol_id("aaaa"));
        convoy.push_molecule(test_mol_id("bbbb"));
        let display = convoy.to_string();
        assert!(display.contains("convoy-alpha"));
        assert!(display.contains("Alpha batch"));
        assert!(display.contains("2 molecules"));
        assert!(display.contains("pending"));
    }

    #[test]
    fn test_convoy_serde_roundtrip() {
        let mut convoy = Convoy::new(test_convoy_id(), "serde test".to_owned());
        convoy.push_molecule(test_mol_id("aaaa"));
        convoy.activate();

        let json = serde_json::to_string(&convoy).unwrap();
        let back: Convoy = serde_json::from_str(&json).unwrap();
        assert_eq!(convoy, back);
    }
}
