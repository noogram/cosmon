// SPDX-License-Identifier: AGPL-3.0-only

//! [`PilotCheckpoint`] — the compact, explicitly published hand-over record.
//!
//! # Why a checkpoint is not a summary
//!
//! A summary is prose, and prose can only be compared by another model's
//! opinion. The mission forbids that: a finding cites two assertions and their
//! evidence, or it is `INCONCLUSIVE` (ADR-168 D3.4). So the record is built out
//! of parts a comparison can *decide* on:
//!
//! - a [`Scope`] is two sets of strings, so a perimeter change is a set
//!   difference;
//! - a [`Claim`] carries a `subject` both pilots address and a [`Stance`] on
//!   it, so a contradiction is `subject_a == subject_b && stance_a != stance_b`;
//! - evidence is a list of [`EvidenceRef`], so "asserted without evidence" is
//!   `evidence.is_empty()`.
//!
//! The pilot writing the checkpoint is the one who names the subjects. That is
//! the price of a decidable comparison, and it is the honest one: the co-pilot
//! is not guessing what the primary meant, it is reading what the primary
//! declared.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{CheckpointId, ClaimId, MissionId, SessionId};

/// A pointer to something that can be re-read: a file, a molecule, a commit, a
/// URL.
///
/// The locator is opaque to this crate on purpose — resolving it would be I/O,
/// and the comparison is I/O-free. What the comparison uses is only whether an
/// evidence ref *exists* for a claim and whether the two sides cite the same
/// one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Where the evidence lives — a path, a molecule id, a commit sha, a URL.
    pub locator: String,
    /// Optional content digest, so a citation can be checked to still point at
    /// what it pointed at when the checkpoint was published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

impl EvidenceRef {
    /// A reference to `locator`, with no digest pinned.
    #[must_use]
    pub fn new(locator: impl Into<String>) -> Self {
        Self {
            locator: locator.into(),
            digest: None,
        }
    }

    /// A reference to `locator`, pinned to `digest`.
    #[must_use]
    pub fn pinned(locator: impl Into<String>, digest: impl Into<String>) -> Self {
        Self {
            locator: locator.into(),
            digest: Some(digest.into()),
        }
    }
}

/// Which way a claim points on its subject.
///
/// Two values, not three: "I don't know" is not a stance, it is an entry in
/// [`PilotCheckpoint::unresolved_questions`]. Keeping uncertainty out of the
/// stance is what stops the comparison from having to weigh confidences — the
/// thing the mission calls an opaque psychological score.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    /// The pilot holds that the subject is the case, or intends to do it.
    Affirm,
    /// The pilot holds that the subject is not the case, or intends not to do
    /// it.
    Deny,
}

impl Stance {
    /// The opposite stance.
    #[must_use]
    pub fn negate(self) -> Self {
        match self {
            Self::Affirm => Self::Deny,
            Self::Deny => Self::Affirm,
        }
    }
}

/// One assertion a pilot commits to, on a named subject, with its evidence.
///
/// `statement` is what a finding quotes back to the operator; `subject` is what
/// makes two claims comparable at all. Two pilots writing about the same thing
/// under different subject keys are simply not compared — the comparison says
/// so rather than inventing a match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// Stable id within the checkpoint.
    pub id: ClaimId,
    /// The shared key both pilots address — e.g. `merge-strategy`,
    /// `cursor-is-byte-offset`. Compared verbatim.
    pub subject: String,
    /// Which way this claim points on `subject`.
    pub stance: Stance,
    /// The assertion in the pilot's own words. Quoted verbatim by a finding.
    pub statement: String,
    /// What the pilot read to hold it. Empty is legal to write and is exactly
    /// what [`crate::DriftClass::MissingEvidence`] reports.
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
}

impl Claim {
    /// A claim on `subject` with no evidence attached yet.
    ///
    /// # Errors
    ///
    /// [`crate::CheckpointError::InvalidIdentifier`] if `id` is not a valid
    /// claim id.
    pub fn new(
        id: impl Into<String>,
        subject: impl Into<String>,
        stance: Stance,
        statement: impl Into<String>,
    ) -> Result<Self, crate::CheckpointError> {
        Ok(Self {
            id: ClaimId::new(id)?,
            subject: subject.into(),
            stance,
            statement: statement.into(),
            evidence: Vec::new(),
        })
    }

    /// The same claim, citing `evidence`.
    #[must_use]
    pub fn with_evidence(mut self, evidence: impl IntoIterator<Item = EvidenceRef>) -> Self {
        self.evidence.extend(evidence);
        self
    }
}

/// The perimeter of the mission as one pilot understands it.
///
/// Two sets rather than one paragraph, because *"changement de périmètre"* is
/// then a set difference and not a judgement. `excludes` is not decoration: a
/// pilot that drops an explicit exclusion has widened the mission just as
/// surely as one that adds an inclusion, and only an explicit set can show it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// What this mission covers.
    #[serde(default)]
    pub includes: BTreeSet<String>,
    /// What this mission explicitly does not cover.
    #[serde(default)]
    pub excludes: BTreeSet<String>,
}

impl Scope {
    /// A scope from its two sets.
    #[must_use]
    pub fn new(
        includes: impl IntoIterator<Item = String>,
        excludes: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            includes: includes.into_iter().collect(),
            excludes: excludes.into_iter().collect(),
        }
    }

    /// Whether the pilot declared any perimeter at all.
    ///
    /// An undeclared perimeter is not an agreed one: the comparison reports it
    /// as nothing-to-compare rather than as agreement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.includes.is_empty() && self.excludes.is_empty()
    }
}

/// The hand-over record: what a relief pilot resumes from, instead of
/// re-reading the whole transcript.
///
/// Invariant CHECKPOINT-NOT-SCROLLBACK in one struct. Everything a takeover
/// needs is here and is compact; nothing here is conversation content, which
/// stays with the provider (ADR-168 D3.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotCheckpoint {
    /// Identifies this checkpoint within the mission.
    pub id: CheckpointId,
    /// The mission being flown.
    pub mission_id: MissionId,
    /// The cosmon session that published it.
    pub session_id: SessionId,
    /// The authority epoch the publisher believed it was under. M4 wires the
    /// guard; M3 records the number so a later reader can tell two checkpoints
    /// written across a transfer apart.
    pub lease_epoch: u64,
    /// The perimeter as this pilot understands it.
    #[serde(default)]
    pub scope: Scope,
    /// What the pilot currently holds to be true.
    #[serde(default)]
    pub current_hypotheses: Vec<Claim>,
    /// Evidence cited by the checkpoint as a whole, beyond per-claim citations.
    #[serde(default)]
    pub evidence_refs: Vec<EvidenceRef>,
    /// What has already been done, in the pilot's words. Narrative, not
    /// compared.
    #[serde(default)]
    pub completed_actions: Vec<String>,
    /// What the pilot intends to do next. Compared, because two pilots
    /// intending opposite next moves is the finding a co-pilot exists to
    /// surface.
    #[serde(default)]
    pub intended_next_actions: Vec<Claim>,
    /// Known risks, in the pilot's words.
    #[serde(default)]
    pub open_risks: Vec<String>,
    /// Questions the pilot could not answer. This is where uncertainty lives —
    /// deliberately not inside a [`Stance`].
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
    /// When it was published.
    pub created_at: DateTime<Utc>,
}

impl PilotCheckpoint {
    /// An empty checkpoint for `mission`, published by `session` under
    /// `lease_epoch` at `created_at`.
    ///
    /// The clock is a parameter: this crate's core is I/O-free, and "what time
    /// is it" is I/O.
    ///
    /// # Errors
    ///
    /// [`crate::CheckpointError::InvalidIdentifier`] if any id is invalid.
    pub fn new(
        id: impl Into<String>,
        mission_id: impl Into<String>,
        session_id: impl Into<String>,
        lease_epoch: u64,
        created_at: DateTime<Utc>,
    ) -> Result<Self, crate::CheckpointError> {
        Ok(Self {
            id: CheckpointId::new(id)?,
            mission_id: MissionId::new(mission_id)?,
            session_id: SessionId::new(session_id)?,
            lease_epoch,
            scope: Scope::default(),
            current_hypotheses: Vec::new(),
            evidence_refs: Vec::new(),
            completed_actions: Vec::new(),
            intended_next_actions: Vec::new(),
            open_risks: Vec::new(),
            unresolved_questions: Vec::new(),
            created_at,
        })
    }

    /// Every subject this checkpoint takes a position on, in either list.
    #[must_use]
    pub fn subjects(&self) -> BTreeSet<&str> {
        self.current_hypotheses
            .iter()
            .chain(&self.intended_next_actions)
            .map(|c| c.subject.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("in range")
    }

    #[test]
    fn a_checkpoint_round_trips_through_json() {
        let mut cp = PilotCheckpoint::new("cp-1", "task-1", "sess-a", 3, at(1_000)).unwrap();
        cp.scope = Scope::new(["the port".to_owned()], ["the cockpit".to_owned()]);
        cp.current_hypotheses.push(
            Claim::new(
                "h1",
                "cursor-is-byte-offset",
                Stance::Affirm,
                "byte offsets",
            )
            .unwrap()
            .with_evidence([EvidenceRef::new("docs/adr/168.md")]),
        );

        let json = serde_json::to_string(&cp).unwrap();
        let back: PilotCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(cp, back);
    }

    #[test]
    fn subjects_are_collected_from_both_claim_lists() {
        let mut cp = PilotCheckpoint::new("cp-1", "task-1", "sess-a", 0, at(0)).unwrap();
        cp.current_hypotheses
            .push(Claim::new("h1", "alpha", Stance::Affirm, "…").unwrap());
        cp.intended_next_actions
            .push(Claim::new("i1", "beta", Stance::Deny, "…").unwrap());
        assert_eq!(cp.subjects(), ["alpha", "beta"].into_iter().collect());
    }

    #[test]
    fn an_undeclared_scope_is_empty_not_agreed() {
        let cp = PilotCheckpoint::new("cp-1", "task-1", "sess-a", 0, at(0)).unwrap();
        assert!(cp.scope.is_empty());
    }
}
