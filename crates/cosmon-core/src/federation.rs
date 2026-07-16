// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-galaxy federation provenance — ADR-105 (I9') machinery.
//!
//! ADR-105 ([`docs/adr/105-i9-prime-federation-provenance.md`]) names the
//! **second Gödel sentence of cosmon**: any cross-galaxy merge into
//! `cosmon-main` must be traceable back to its source galaxy's ledger.
//! The doctrine without machinery is exactly the failure pattern the
//! cosmon CLAUDE.md warns against — cosmon defines the rule and then
//! relies on operator memory to enforce it.
//!
//! [`FederationLineage`] is the typed witness that closes the gap. It
//! is attached as an `Option<FederationLineage>` to cross-galaxy event
//! variants (today: [`crate::event_v2::EventV2::MergeDispatched`] and
//! [`crate::event_v2::EventV2::MergeCompleted`]), and `cs verify
//! --federation` scans the fleet-wide event log for cross-galaxy
//! events whose `federation_provenance` is `None`.
//!
//! The field is `Option<...>` rather than mandatory because:
//!
//! 1. **Backward compatibility.** Existing `events.jsonl` lines pre-date
//!    the field; flipping it to a required `T` would break replay.
//! 2. **Detection, not prevention** ([ADR-052], [ADR-105] §D2).
//!    The cosmon discipline is `cs verify` reports drift; the gate
//!    *logs distinctly*, it does not block. Mandatory at compile time
//!    is a future move once provenance has been observed in the field
//!    for one cycle (ADR-105 §"Out of scope" — *making the field
//!    mandatory at compile time is a major bump on `EventV2`; do it
//!    once provenance has been observed for one cycle*).
//!
//! See also [`crate::event_v2`] for the event envelope schema.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Typed witness that a cross-galaxy artefact crossed the federation
/// boundary at a known source-galaxy commit.
///
/// One [`FederationLineage`] is attached per cross-galaxy event
/// (today: [`crate::event_v2::EventV2::MergeDispatched`] /
/// [`crate::event_v2::EventV2::MergeCompleted`]; tomorrow:
/// `DelegationDispatched` per ADR-105 §D3 Oracle B'' once the
/// `cs delegate` verb lands).
///
/// # Invariants
///
/// - `source_galaxy` is a finite, explicit, versioned alias — one of the
///   peers enumerated in `.cosmon/config.toml [provenance_federation]
///   trusted_galaxies`. Adding a galaxy is a deliberate configuration
///   change (ADR-105 §D5). The type does **not** validate the alias —
///   the gate does, so the lineage remains expressible even when the
///   federation list rejects it (which is the audit-trail value).
/// - `source_commit` is the SHA in the source galaxy's git repo at the
///   moment the artefact crossed. It is informational: the cosmon gate
///   may best-effort verify it via `git -C /srv/cosmon/<source_galaxy>
///   rev-parse <commit>`, but a stale source repo does not invalidate
///   the lineage — the federation is detect-only, not enforce-only.
/// - `source_path` is the path relative to the source galaxy's root.
/// - `crossed_at` is the UTC timestamp at which the cosmon writer
///   stamped the lineage (i.e. *when the boundary was crossed*, not
///   when the source artefact was authored).
///
/// # Schema stability
///
/// Newly-added cross-galaxy events serialise `federation_provenance` as
/// `null` when absent, courtesy of `#[serde(default,
/// skip_serializing_if = "Option::is_none")]` on each variant's field.
/// Legacy events without the field deserialize to `None` — that is the
/// expected legacy-tolerate behaviour. `cs verify --federation` reports
/// missing provenance as a hard FAIL on cross-galaxy events; legacy
/// non-cross-galaxy events are unaffected.
///
/// # Example
///
/// ```
/// use cosmon_core::federation::FederationLineage;
/// use std::path::PathBuf;
///
/// let lineage = FederationLineage {
///     source_galaxy: "smithy".to_owned(),
///     source_commit: "195ff5aa".to_owned(),
///     source_path: PathBuf::from("docs/adr/0042-rpp-binding.md"),
///     crossed_at: chrono::Utc::now(),
/// };
/// assert_eq!(lineage.source_galaxy, "smithy");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationLineage {
    /// Source galaxy alias (`"cosmon"`, `"smithy"`, `"mailroom"`, …).
    ///
    /// The alias is matched against
    /// `.cosmon/config.toml [provenance_federation] trusted_galaxies`
    /// at gate time by `cs verify --federation`. An unknown alias is
    /// recorded but reported as a *federation-membership* failure
    /// rather than a *missing-provenance* failure — the audit trail
    /// distinguishes the two so the operator can act (either add the
    /// galaxy to the trusted list, or refuse the inheritance).
    pub source_galaxy: String,
    /// Commit hash in the source galaxy's repo at citation time.
    ///
    /// Free-form string (short or long SHA accepted) — the gate may
    /// best-effort verify it via `git rev-parse`, but a stale source
    /// repo does not invalidate the lineage.
    pub source_commit: String,
    /// Path to the source artefact within the source galaxy.
    ///
    /// Relative to the source galaxy's root (e.g. `docs/adr/0042.md`).
    /// Absolute paths are accepted but discouraged — they leak the
    /// operator's filesystem layout into the audit trail.
    pub source_path: PathBuf,
    /// When the citation crossed the federation boundary.
    ///
    /// UTC. This is the timestamp at which the cosmon writer attached
    /// the lineage to the event — *not* the source artefact's authoring
    /// time.
    pub crossed_at: DateTime<Utc>,
}

impl FederationLineage {
    /// True iff the lineage's `source_galaxy` is in the trusted set.
    ///
    /// Helper for `cs verify --federation` and downstream gates that
    /// project the membership check from
    /// `.cosmon/config.toml [provenance_federation] trusted_galaxies`.
    ///
    /// The check is intentionally O(N) — `trusted_galaxies` is bounded
    /// by the Tarski-hierarchy argument of ADR-105 §D5 (finite,
    /// explicit, versioned).
    #[must_use]
    pub fn is_trusted(&self, trusted_galaxies: &[String]) -> bool {
        trusted_galaxies.iter().any(|g| g == &self.source_galaxy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_all_fields() {
        let lineage = FederationLineage {
            source_galaxy: "smithy".to_owned(),
            source_commit: "abc123".to_owned(),
            source_path: PathBuf::from("docs/adr/0042.md"),
            crossed_at: chrono::DateTime::<Utc>::from_naive_utc_and_offset(
                chrono::DateTime::<Utc>::from_timestamp(0, 0)
                    .unwrap()
                    .naive_utc(),
                Utc,
            ),
        };
        let s = serde_json::to_string(&lineage).unwrap();
        assert!(s.contains("\"source_galaxy\":\"smithy\""));
        assert!(s.contains("\"source_commit\":\"abc123\""));
    }

    #[test]
    fn roundtrips_through_serde() {
        let lineage = FederationLineage {
            source_galaxy: "mailroom".to_owned(),
            source_commit: "deadbeef".to_owned(),
            source_path: PathBuf::from("docs/lore/2026-05-19.md"),
            crossed_at: chrono::Utc::now(),
        };
        let s = serde_json::to_string(&lineage).unwrap();
        let back: FederationLineage = serde_json::from_str(&s).unwrap();
        assert_eq!(lineage, back);
    }

    #[test]
    fn is_trusted_matches_alias() {
        let lineage = FederationLineage {
            source_galaxy: "smithy".to_owned(),
            source_commit: "x".to_owned(),
            source_path: PathBuf::from("a"),
            crossed_at: chrono::Utc::now(),
        };
        let trusted: Vec<String> = vec!["cosmon".to_owned(), "smithy".to_owned()];
        assert!(lineage.is_trusted(&trusted));
        let untrusted: Vec<String> = vec!["cosmon".to_owned()];
        assert!(!lineage.is_trusted(&untrusted));
    }
}
