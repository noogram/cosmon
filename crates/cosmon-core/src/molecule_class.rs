// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule class — operational tier of a molecule.
//!
//! Orthogonal to `MoleculeKind` (the
//! cognitive nature — *what* a molecule represents) and to
//! `Formula` (the execution recipe — *how*
//! it runs). Class names the *audit posture* under which the molecule
//! is dispatched.
//!
//! See [ADR-085 — Stress-test seal
//! mechanism](../../../docs/adr/085-stress-test-seal-mechanism.md).

use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::id::MoleculeId;

/// The operational class of a molecule, chosen at nucleation.
///
/// A `deep-think` deliberation may be a tactical exploration ([`Standard`])
/// or a stress-test of a pre-committed prior ([`StressTest`]); the same
/// formula runs under different audit postures depending on this flag.
///
/// Defaults to `Standard` for legacy molecules and any
/// nucleation that does not pass `--class`.
///
/// [`Standard`]: Self::Standard
/// [`StressTest`]: Self::StressTest
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MoleculeClass {
    /// Tactical, exploratory work — the typical case. No seal required;
    /// subject to autopilot drain like any other tagged molecule.
    #[default]
    Standard,
    /// Stress-test of a pre-committed prior (ADR-085).
    ///
    /// Triggers the two-layer seal at dispatch:
    /// 1. Runtime precondition — `prior.md` + `prior.b3` exist on disk.
    /// 2. Witness-quorum — `cs witness attest` fired in a separate session.
    ///
    /// Opted out of autopilot drain by class declaration: a `temp:hot`
    /// tag alone cannot lift a stress-test molecule into auto-tackle.
    StressTest,
    /// Infrastructure / housekeeping work that should not be confused
    /// with cognitive deliberation. Reserved namespace; no gating today,
    /// preserved so the schema does not break when the runtime later
    /// distinguishes infra-class events from operator-cognitive ones.
    Infra,
}

impl MoleculeClass {
    /// Does this class require the ADR-085 stress-test seal at dispatch?
    #[must_use]
    pub const fn requires_seal(self) -> bool {
        matches!(self, Self::StressTest)
    }

    /// Is this class opted out of autopilot drain?
    #[must_use]
    pub const fn opts_out_of_autopilot(self) -> bool {
        matches!(self, Self::StressTest)
    }
}

impl FromStr for MoleculeClass {
    type Err = ParseEnumError;

    /// Parse the kebab-case CLI form (`standard`, `stress-test`, `infra`).
    /// Mirrors the serde rename so the same string roundtrips through
    /// `--class` and `state.json`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "standard" => Ok(Self::Standard),
            "stress-test" => Ok(Self::StressTest),
            "infra" => Ok(Self::Infra),
            _ => Err(ParseEnumError {
                type_name: "MoleculeClass",
                value: s.to_owned(),
            }),
        }
    }
}

/// Typed audit artefact produced when an operator bypasses the
/// stress-test seal at nucleation (ADR-085 §3.5).
///
/// Replaces the historical free-text `dispatch-decision.md` with a
/// structured record linked to the event log via
/// `EventV2::SealBypassed`.
/// Persisted at `<molecule_dir>/bypass-receipt.json`.
///
/// The receipt is **permanent** — re-running a molecule whose lineage
/// contains a bypass receipt triggers cross-galaxy escalation (Layer 3,
/// ADR-085 §4). It is a *trace*, not a lock, in the same sense as
/// `BriefingSeal`: no chmod, no PKI. The
/// audit value comes from the event-log chain, not from the JSON file
/// alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassReceipt {
    /// The molecule the receipt was issued for.
    pub molecule_id: MoleculeId,
    /// The actor who authored the bypass — the operator's nucleon-id
    /// projection (`"operator"` for trusted-CLI flows; JWT-mapped for
    /// remote pilots, kept stringly for wire stability).
    pub actor: String,
    /// One-line free-text reason supplied via `--bypass-reason`.
    pub reason: String,
    /// Wall-clock time the bypass was recorded.
    pub bypassed_at: DateTime<Utc>,
    /// BLAKE3 hash (64-char lowercase hex) of `frame.md` at bypass time.
    /// Distinguishes a bypass that knew the framing from one that
    /// pre-dated framing entirely.
    pub frame_hash: String,
    /// Identifier of the layer-1 precondition that was bypassed
    /// (e.g. `"prior-seal-missing"`). Stringly-typed for the same
    /// wire-stability rationale as
    /// `MergeResult::Other`.
    pub bypassed_condition: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    #[test]
    fn class_parses_kebab_case_strings() {
        assert_eq!(
            "standard".parse::<MoleculeClass>().unwrap(),
            MoleculeClass::Standard
        );
        assert_eq!(
            "stress-test".parse::<MoleculeClass>().unwrap(),
            MoleculeClass::StressTest
        );
        assert_eq!(
            "infra".parse::<MoleculeClass>().unwrap(),
            MoleculeClass::Infra
        );
        assert!("Standard".parse::<MoleculeClass>().is_err());
        assert!("nonsense".parse::<MoleculeClass>().is_err());
    }

    #[test]
    fn class_default_is_standard() {
        assert_eq!(MoleculeClass::default(), MoleculeClass::Standard);
    }

    #[test]
    fn only_stress_test_requires_seal() {
        assert!(!MoleculeClass::Standard.requires_seal());
        assert!(MoleculeClass::StressTest.requires_seal());
        assert!(!MoleculeClass::Infra.requires_seal());
    }

    #[test]
    fn only_stress_test_opts_out_of_autopilot() {
        assert!(!MoleculeClass::Standard.opts_out_of_autopilot());
        assert!(MoleculeClass::StressTest.opts_out_of_autopilot());
        assert!(!MoleculeClass::Infra.opts_out_of_autopilot());
    }

    #[test]
    fn class_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&MoleculeClass::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(
            serde_json::to_string(&MoleculeClass::StressTest).unwrap(),
            "\"stress-test\""
        );
        assert_eq!(
            serde_json::to_string(&MoleculeClass::Infra).unwrap(),
            "\"infra\""
        );
    }

    #[test]
    fn class_roundtrip_through_json() {
        for class in [
            MoleculeClass::Standard,
            MoleculeClass::StressTest,
            MoleculeClass::Infra,
        ] {
            let json = serde_json::to_string(&class).unwrap();
            let back: MoleculeClass = serde_json::from_str(&json).unwrap();
            assert_eq!(back, class);
        }
    }

    #[test]
    fn bypass_receipt_roundtrip() {
        let receipt = BypassReceipt {
            molecule_id: mid("delib-20260503-5a74"),
            actor: "operator".to_owned(),
            reason: "emergency dispatch — incident triage".to_owned(),
            bypassed_at: DateTime::parse_from_rfc3339("2026-05-03T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            frame_hash: "0".repeat(64),
            bypassed_condition: "prior-seal-missing".to_owned(),
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let back: BypassReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, receipt);
    }
}
