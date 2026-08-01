// SPDX-License-Identifier: AGPL-3.0-only

//! Tri-valued comparison of two checkpoints.
//!
//! # The whole design in one rule
//!
//! **A verdict is a consequence of a class, and a class is a consequence of a
//! decidable test.** Nothing in this module weighs, scores or estimates. Every
//! [`DriftFinding`] it emits names the test that fired
//! ([`DriftClass`]), quotes the assertions that made it fire
//! ([`CitedClaim`]), and carries the verdict that class *always* carries
//! ([`DriftClass::verdict`]). There is no code path that assigns a verdict
//! independently of a class, which is why the report cannot say `AGREE` about
//! something it never compared.
//!
//! | Test | Class | Verdict |
//! |---|---|---|
//! | one side has no checkpoint | [`DriftClass::MissingCheckpoint`] | `INCONCLUSIVE` |
//! | the two checkpoints are about different missions | [`DriftClass::MissionMismatch`] | `INCONCLUSIVE` |
//! | both present, nothing addressed by both | [`DriftClass::NoComparableClaim`] | `INCONCLUSIVE` |
//! | the declared perimeters differ | [`DriftClass::ScopeChange`] | `FINDING` |
//! | same subject, opposite stance, in `intended_next_actions` | [`DriftClass::ContradictoryIntent`] | `FINDING` |
//! | same subject, opposite stance, in `current_hypotheses` | [`DriftClass::ContradictoryHypothesis`] | `FINDING` |
//! | a claim on a shared subject cites nothing | [`DriftClass::MissingEvidence`] | `FINDING` |
//! | the perimeters are identical and non-empty | [`DriftClass::ScopeAgreement`] | `AGREE` |
//! | same subject, same stance | [`DriftClass::SubjectAgreement`] | `AGREE` |
//!
//! # What is deliberately absent
//!
//! - **A score.** No percentage, no confidence, no distance. ADR-168 D3.4
//!   refuses it, and the falsifier is mechanical: no field of any type in this
//!   module is a float, and [`DriftReport`] serialises without one.
//! - **A semantic matcher.** Two claims are about the same thing when their
//!   `subject` strings are equal. A fuzzy match would let the co-pilot invent
//!   a disagreement its evidence cannot support.
//! - **A command.** ADVISORY-DRIFT: a finding is a citation handed to the
//!   operator. This module returns a report; it mutates nothing, and it has no
//!   API that could.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checkpoint::{Claim, EvidenceRef, PilotCheckpoint};
use crate::id::{CheckpointId, ClaimId, FindingId, MissionId};

/// The three values a comparison can take.
///
/// `INCONCLUSIVE` is a first-class answer, not an error. The mission's
/// falsifier 8 is *"a missing checkpoint is rendered as `AGREE`"*; the way to
/// make that unfalsifiable is to give "I could not compare" a verdict of its
/// own rather than folding it into either of the other two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    /// The two checkpoints hold the same position on what was compared.
    Agree,
    /// A decidable test fired, with both assertions cited.
    Finding,
    /// Not comparable. Never an implicit agreement.
    Inconclusive,
}

impl Verdict {
    /// Unix-style exit code, matching `cs diverge`: `0 | 1 | 2`.
    ///
    /// The alignment is not cosmetic — a caller that already branches on
    /// `cs diverge`'s codes must not have to learn a second convention, and
    /// `2` is what keeps "unknown" out of the "disagree" bucket.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Agree => 0,
            Self::Finding => 1,
            Self::Inconclusive => 2,
        }
    }
}

/// Which of the two compared checkpoints a citation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    /// The first checkpoint given to [`compare`].
    A,
    /// The second.
    B,
}

/// The decidable test a finding is the result of.
///
/// Each variant is a *test*, not a topic. That is what lets
/// [`DriftClass::verdict`] be a total function with no room for judgement: the
/// class already says whether the thing found is a disagreement, an agreement,
/// or an inability to compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DriftClass {
    /// The declared perimeters differ. Cites every item held by one side only.
    ScopeChange,
    /// The same subject appears in both `intended_next_actions` with opposite
    /// stances: the two pilots are about to do contrary things.
    ContradictoryIntent,
    /// The same subject appears in both `current_hypotheses` with opposite
    /// stances: the two pilots believe contrary things.
    ContradictoryHypothesis,
    /// A claim on a subject the other side also addresses cites no evidence.
    MissingEvidence,
    /// One or both sides published no checkpoint.
    MissingCheckpoint,
    /// The two checkpoints are about different missions and are not comparable.
    MissionMismatch,
    /// Both checkpoints exist but address nothing in common.
    NoComparableClaim,
    /// The declared perimeters are identical and non-empty.
    ScopeAgreement,
    /// The same subject is held with the same stance on both sides.
    SubjectAgreement,
}

impl DriftClass {
    /// The verdict this class always carries.
    ///
    /// Total and constant. A finding's verdict is never computed anywhere else,
    /// so no future edit can produce an `AGREE` from a class that means
    /// "unknown".
    #[must_use]
    pub fn verdict(self) -> Verdict {
        match self {
            Self::ScopeChange
            | Self::ContradictoryIntent
            | Self::ContradictoryHypothesis
            | Self::MissingEvidence => Verdict::Finding,
            Self::MissingCheckpoint | Self::MissionMismatch | Self::NoComparableClaim => {
                Verdict::Inconclusive
            }
            Self::ScopeAgreement | Self::SubjectAgreement => Verdict::Agree,
        }
    }

    /// Stable `snake_case` name, used both in output and in the content address
    /// of a finding.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScopeChange => "scope_change",
            Self::ContradictoryIntent => "contradictory_intent",
            Self::ContradictoryHypothesis => "contradictory_hypothesis",
            Self::MissingEvidence => "missing_evidence",
            Self::MissingCheckpoint => "missing_checkpoint",
            Self::MissionMismatch => "mission_mismatch",
            Self::NoComparableClaim => "no_comparable_claim",
            Self::ScopeAgreement => "scope_agreement",
            Self::SubjectAgreement => "subject_agreement",
        }
    }
}

/// One assertion, quoted from the checkpoint it came from.
///
/// ADVISORY-DRIFT in a struct: a finding that could not fill two of these has
/// nothing to advise about. `claim` is `None` only for scope items, which are
/// declarations of the checkpoint rather than of a claim inside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitedClaim {
    /// Which checkpoint this came from.
    pub side: Side,
    /// The checkpoint it came from.
    pub checkpoint: CheckpointId,
    /// The claim's id, when the citation is a claim rather than a scope item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimId>,
    /// The subject addressed — or `scope.includes` / `scope.excludes`.
    pub subject: String,
    /// The assertion, verbatim.
    pub statement: String,
    /// What this side cited for it. Empty is the point of
    /// [`DriftClass::MissingEvidence`].
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
}

/// One comparison result: a class, its verdict, and the assertions that
/// produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftFinding {
    /// Content-addressed id — the same comparison yields the same id.
    pub id: FindingId,
    /// The left checkpoint, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_a: Option<CheckpointId>,
    /// The right checkpoint, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_b: Option<CheckpointId>,
    /// The test that fired.
    pub class: DriftClass,
    /// The assertions cited, in the order the finding reads.
    pub cited_claims: Vec<CitedClaim>,
    /// The union of the evidence cited by `cited_claims`, deduplicated.
    pub evidence_refs: Vec<EvidenceRef>,
    /// Always `class.verdict()`.
    pub verdict: Verdict,
    /// One-line description of what fired, for a human reading the report.
    pub detail: String,
    /// When the comparison ran.
    pub created_at: DateTime<Utc>,
}

/// The result of comparing two checkpoints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftReport {
    /// The mission compared, when both sides agreed on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<MissionId>,
    /// The left checkpoint, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_a: Option<CheckpointId>,
    /// The right checkpoint, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_b: Option<CheckpointId>,
    /// Every finding, agreement and inconclusive record produced.
    pub findings: Vec<DriftFinding>,
    /// The overall answer. See [`DriftReport::roll_up`] for the rule.
    pub verdict: Verdict,
    /// When the comparison ran.
    pub created_at: DateTime<Utc>,
}

impl DriftReport {
    /// Fold the per-finding verdicts into the report's verdict.
    ///
    /// Order matters and is fail-closed: a `FINDING` outranks everything
    /// (something was actually shown), then `INCONCLUSIVE` (something could not
    /// be compared), and `AGREE` only survives when neither of the first two
    /// occurred *and* at least one agreement was actually established. An empty
    /// finding list is `INCONCLUSIVE`, never `AGREE` — nothing compared is not
    /// agreement.
    fn roll_up(findings: &[DriftFinding]) -> Verdict {
        if findings.iter().any(|f| f.verdict == Verdict::Finding) {
            Verdict::Finding
        } else if findings.iter().any(|f| f.verdict == Verdict::Inconclusive) {
            Verdict::Inconclusive
        } else if findings.iter().any(|f| f.verdict == Verdict::Agree) {
            Verdict::Agree
        } else {
            Verdict::Inconclusive
        }
    }

    /// Only the records that are actual findings.
    #[must_use]
    pub fn findings_only(&self) -> Vec<&DriftFinding> {
        self.findings
            .iter()
            .filter(|f| f.verdict == Verdict::Finding)
            .collect()
    }

    /// Whether any record carries `class`.
    #[must_use]
    pub fn has_class(&self, class: DriftClass) -> bool {
        self.findings.iter().any(|f| f.class == class)
    }
}

/// Compare two checkpoints, either of which may be absent.
///
/// `now` is a parameter because this crate's comparison is I/O-free, and
/// reading a clock is I/O. Passing a fixed instant is also what makes a report
/// byte-reproducible in a test.
///
/// This function cannot fail. An input it cannot compare produces an
/// `INCONCLUSIVE` record inside the report — returning `Err` would hand the
/// caller an "unknown" it is free to render as agreement, which is exactly the
/// mission's falsifier 8.
///
/// # Example
///
/// ```
/// use chrono::{DateTime, Utc};
/// use cosmon_pilot_checkpoint::{compare, PilotCheckpoint, Verdict};
///
/// let now = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
/// let a = PilotCheckpoint::new("cp-a", "task-1", "sess-a", 1, now)?;
///
/// // The co-pilot never published one. That is not agreement.
/// let report = compare(Some(&a), None, now);
/// assert_eq!(report.verdict, Verdict::Inconclusive);
/// # Ok::<(), cosmon_pilot_checkpoint::CheckpointError>(())
/// ```
#[must_use]
pub fn compare(
    a: Option<&PilotCheckpoint>,
    b: Option<&PilotCheckpoint>,
    now: DateTime<Utc>,
) -> DriftReport {
    let ids = (a.map(|c| c.id.clone()), b.map(|c| c.id.clone()));

    let (Some(a), Some(b)) = (a, b) else {
        let detail = match (a.is_some(), b.is_some()) {
            (true, false) => "side B published no checkpoint",
            (false, true) => "side A published no checkpoint",
            _ => "neither side published a checkpoint",
        };
        let finding = finding(
            DriftClass::MissingCheckpoint,
            &ids,
            Vec::new(),
            detail.to_owned(),
            now,
        );
        return report(None, ids, vec![finding], now);
    };

    if a.mission_id != b.mission_id {
        let detail = format!(
            "checkpoint {} is about mission {}, checkpoint {} about mission {}",
            a.id, a.mission_id, b.id, b.mission_id
        );
        let finding = finding(DriftClass::MissionMismatch, &ids, Vec::new(), detail, now);
        return report(None, ids, vec![finding], now);
    }

    let mut findings = Vec::new();
    findings.extend(compare_scope(a, b, now));
    findings.extend(compare_claims(
        a,
        b,
        ClaimKind::Hypothesis,
        DriftClass::ContradictoryHypothesis,
        now,
    ));
    findings.extend(compare_claims(
        a,
        b,
        ClaimKind::Intent,
        DriftClass::ContradictoryIntent,
        now,
    ));
    findings.extend(missing_evidence(a, b, now));

    if findings.is_empty() {
        findings.push(finding(
            DriftClass::NoComparableClaim,
            &ids,
            Vec::new(),
            "both checkpoints exist but address no common subject and declare no perimeter"
                .to_owned(),
            now,
        ));
    }

    report(Some(a.mission_id.clone()), ids, findings, now)
}

/// Which of a checkpoint's two claim lists is being compared.
#[derive(Clone, Copy)]
enum ClaimKind {
    Hypothesis,
    Intent,
}

impl ClaimKind {
    fn of(self, cp: &PilotCheckpoint) -> &[Claim] {
        match self {
            Self::Hypothesis => &cp.current_hypotheses,
            Self::Intent => &cp.intended_next_actions,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::Hypothesis => "hypothesis",
            Self::Intent => "intent",
        }
    }
}

/// Index a claim list by subject, keeping the first claim per subject.
///
/// First wins rather than last: a checkpoint that states a subject twice is
/// malformed, and picking the earlier statement is the one choice that does not
/// depend on how the pilot happened to append.
fn by_subject(claims: &[Claim]) -> BTreeMap<&str, &Claim> {
    let mut map = BTreeMap::new();
    for claim in claims {
        map.entry(claim.subject.as_str()).or_insert(claim);
    }
    map
}

fn compare_scope(
    a: &PilotCheckpoint,
    b: &PilotCheckpoint,
    now: DateTime<Utc>,
) -> Option<DriftFinding> {
    if a.scope.is_empty() || b.scope.is_empty() {
        // An undeclared perimeter is compared with nothing. It is not a
        // finding (the pilot may simply not have declared one) and it is
        // certainly not an agreement.
        return None;
    }

    let ids = (Some(a.id.clone()), Some(b.id.clone()));
    let mut cited = Vec::new();
    for (field, left, right) in [
        ("scope.includes", &a.scope.includes, &b.scope.includes),
        ("scope.excludes", &a.scope.excludes, &b.scope.excludes),
    ] {
        for item in left.difference(right) {
            cited.push(scope_citation(Side::A, &a.id, field, item));
        }
        for item in right.difference(left) {
            cited.push(scope_citation(Side::B, &b.id, field, item));
        }
    }

    if cited.is_empty() {
        return Some(finding(
            DriftClass::ScopeAgreement,
            &ids,
            Vec::new(),
            format!(
                "both checkpoints declare the same perimeter ({} included, {} excluded)",
                a.scope.includes.len(),
                a.scope.excludes.len()
            ),
            now,
        ));
    }

    let detail = format!("{} perimeter item(s) held by one side only", cited.len());
    Some(finding(DriftClass::ScopeChange, &ids, cited, detail, now))
}

fn scope_citation(side: Side, checkpoint: &CheckpointId, field: &str, item: &str) -> CitedClaim {
    CitedClaim {
        side,
        checkpoint: checkpoint.clone(),
        claim: None,
        subject: field.to_owned(),
        statement: item.to_owned(),
        evidence: Vec::new(),
    }
}

fn compare_claims(
    a: &PilotCheckpoint,
    b: &PilotCheckpoint,
    kind: ClaimKind,
    contradiction: DriftClass,
    now: DateTime<Utc>,
) -> Vec<DriftFinding> {
    let ids = (Some(a.id.clone()), Some(b.id.clone()));
    let left = by_subject(kind.of(a));
    let right = by_subject(kind.of(b));

    left.iter()
        .filter_map(|(subject, ca)| right.get(subject).map(|cb| (*subject, *ca, *cb)))
        .map(|(subject, ca, cb)| {
            let cited = vec![citation(Side::A, &a.id, ca), citation(Side::B, &b.id, cb)];
            if ca.stance == cb.stance {
                finding(
                    DriftClass::SubjectAgreement,
                    &ids,
                    cited,
                    format!(
                        "both {} claims on {subject:?} hold the same stance",
                        kind.noun()
                    ),
                    now,
                )
            } else {
                finding(
                    contradiction,
                    &ids,
                    cited,
                    format!(
                        "{} claims on {subject:?} hold opposite stances ({:?} vs {:?})",
                        kind.noun(),
                        ca.stance,
                        cb.stance
                    ),
                    now,
                )
            }
        })
        .collect()
}

/// A claim on a subject the other side also addresses, citing nothing.
///
/// Restricted to *shared* subjects on purpose. An unevidenced claim nobody else
/// addresses is a note to self; an unevidenced claim the other pilot is also
/// reasoning about is a hole in the hand-over, which is what the mission asks
/// to detect.
fn missing_evidence(
    a: &PilotCheckpoint,
    b: &PilotCheckpoint,
    now: DateTime<Utc>,
) -> Vec<DriftFinding> {
    let ids = (Some(a.id.clone()), Some(b.id.clone()));
    let (subjects_a, subjects_b) = (a.subjects(), b.subjects());
    let shared: Vec<&str> = subjects_a.intersection(&subjects_b).copied().collect();

    let mut out = Vec::new();
    for subject in shared {
        for (side, cp, other) in [(Side::A, a, b), (Side::B, b, a)] {
            for claim in cp
                .current_hypotheses
                .iter()
                .chain(&cp.intended_next_actions)
                .filter(|c| c.subject == subject && c.evidence.is_empty())
            {
                let mut cited = vec![citation(side, &cp.id, claim)];
                if let Some(counterpart) = other
                    .current_hypotheses
                    .iter()
                    .chain(&other.intended_next_actions)
                    .find(|c| c.subject == subject)
                {
                    let other_side = match side {
                        Side::A => Side::B,
                        Side::B => Side::A,
                    };
                    cited.push(citation(other_side, &other.id, counterpart));
                }
                out.push(finding(
                    DriftClass::MissingEvidence,
                    &ids,
                    cited,
                    format!(
                        "side {side:?} claim {} on {subject:?} cites no evidence, \
                         while the other side addresses the same subject",
                        claim.id
                    ),
                    now,
                ));
            }
        }
    }
    out
}

fn citation(side: Side, checkpoint: &CheckpointId, claim: &Claim) -> CitedClaim {
    CitedClaim {
        side,
        checkpoint: checkpoint.clone(),
        claim: Some(claim.id.clone()),
        subject: claim.subject.clone(),
        statement: claim.statement.clone(),
        evidence: claim.evidence.clone(),
    }
}

type CheckpointPairIds = (Option<CheckpointId>, Option<CheckpointId>);

fn finding(
    class: DriftClass,
    ids: &CheckpointPairIds,
    cited_claims: Vec<CitedClaim>,
    detail: String,
    now: DateTime<Utc>,
) -> DriftFinding {
    let mut evidence_refs: Vec<EvidenceRef> = cited_claims
        .iter()
        .flat_map(|c| c.evidence.iter().cloned())
        .collect();
    evidence_refs.sort();
    evidence_refs.dedup();

    DriftFinding {
        id: derive_id(class, ids, &cited_claims),
        checkpoint_a: ids.0.clone(),
        checkpoint_b: ids.1.clone(),
        class,
        verdict: class.verdict(),
        cited_claims,
        evidence_refs,
        detail,
        created_at: now,
    }
}

/// Content-address a finding from what it is about, never from when it ran.
///
/// `created_at` is excluded on purpose: re-running the same comparison an hour
/// later must yield the same id, or an operator cannot tell a repeat from a new
/// finding.
fn derive_id(class: DriftClass, ids: &CheckpointPairIds, cited: &[CitedClaim]) -> FindingId {
    #[derive(Serialize)]
    struct Seed<'a> {
        class: &'a str,
        a: Option<&'a str>,
        b: Option<&'a str>,
        cites: Vec<(u8, &'a str, Option<&'a str>)>,
    }

    let seed = Seed {
        class: class.as_str(),
        a: ids.0.as_ref().map(CheckpointId::as_str),
        b: ids.1.as_ref().map(CheckpointId::as_str),
        cites: cited
            .iter()
            .map(|c| {
                let side = match c.side {
                    Side::A => 0,
                    Side::B => 1,
                };
                (
                    side,
                    c.subject.as_str(),
                    c.claim.as_ref().map(ClaimId::as_str),
                )
            })
            .collect(),
    };

    // A digest failure here would mean a struct of `&str` and `u8` is not
    // canonically serialisable. Rather than make every caller handle an error
    // that cannot occur, fall back to the class name plus the cited arity —
    // still deterministic, still a valid id, and visibly degraded if it ever
    // happens.
    let digest = cosmon_hash::hash_value(&seed).map_or_else(
        |_| format!("undigested-{}", cited.len()),
        |h| h.to_hex()[..16].to_owned(),
    );

    FindingId::new(format!(
        "drift-{}-{digest}",
        class.as_str().replace('_', "-")
    ))
    .unwrap_or_else(|_| unreachable!("class names and hex digests are [a-z0-9-]"))
}

fn report(
    mission_id: Option<MissionId>,
    ids: CheckpointPairIds,
    findings: Vec<DriftFinding>,
    now: DateTime<Utc>,
) -> DriftReport {
    DriftReport {
        mission_id,
        checkpoint_a: ids.0,
        checkpoint_b: ids.1,
        verdict: DriftReport::roll_up(&findings),
        findings,
        created_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{Scope, Stance};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("in range")
    }

    fn cp(id: &str, session: &str) -> PilotCheckpoint {
        PilotCheckpoint::new(id, "task-20260731-67f2", session, 1, at(1_000)).expect("valid ids")
    }

    #[test]
    fn every_class_carries_exactly_one_verdict() {
        // The invariant the whole module rests on: a record's verdict is a
        // function of its class, never of anything else.
        for class in [
            DriftClass::ScopeChange,
            DriftClass::ContradictoryIntent,
            DriftClass::ContradictoryHypothesis,
            DriftClass::MissingEvidence,
            DriftClass::MissingCheckpoint,
            DriftClass::MissionMismatch,
            DriftClass::NoComparableClaim,
            DriftClass::ScopeAgreement,
            DriftClass::SubjectAgreement,
        ] {
            let f = finding(class, &(None, None), Vec::new(), String::new(), at(0));
            assert_eq!(f.verdict, class.verdict(), "{class:?}");
        }
    }

    #[test]
    fn an_absent_checkpoint_is_inconclusive_on_both_sides() {
        let a = cp("cp-a", "sess-a");
        for (left, right) in [(Some(&a), None), (None, Some(&a)), (None, None)] {
            let r = compare(left, right, at(0));
            assert_eq!(r.verdict, Verdict::Inconclusive);
            assert!(r.has_class(DriftClass::MissingCheckpoint));
        }
    }

    #[test]
    fn two_checkpoints_about_different_missions_are_inconclusive() {
        let a = cp("cp-a", "sess-a");
        let b = PilotCheckpoint::new("cp-b", "task-other", "sess-b", 1, at(1_000)).unwrap();
        let r = compare(Some(&a), Some(&b), at(0));
        assert_eq!(r.verdict, Verdict::Inconclusive);
        assert!(r.has_class(DriftClass::MissionMismatch));
    }

    #[test]
    fn identical_perimeters_agree() {
        let mut a = cp("cp-a", "sess-a");
        let mut b = cp("cp-b", "sess-b");
        let scope = Scope::new(["the port".to_owned()], ["the cockpit".to_owned()]);
        a.scope = scope.clone();
        b.scope = scope;

        let r = compare(Some(&a), Some(&b), at(0));
        assert_eq!(r.verdict, Verdict::Agree);
        assert!(r.has_class(DriftClass::ScopeAgreement));
    }

    #[test]
    fn the_finding_id_is_stable_across_runs_and_clocks() {
        let mut a = cp("cp-a", "sess-a");
        let mut b = cp("cp-b", "sess-b");
        a.scope = Scope::new(["the port".to_owned()], []);
        b.scope = Scope::new(["the cockpit".to_owned()], []);

        let first = compare(Some(&a), Some(&b), at(0));
        let second = compare(Some(&a), Some(&b), at(9_999));
        let ids_of = |r: &DriftReport| -> Vec<String> {
            r.findings.iter().map(|f| f.id.to_string()).collect()
        };
        assert_eq!(ids_of(&first), ids_of(&second));
    }

    #[test]
    fn a_repeated_subject_takes_the_first_statement() {
        let mut a = cp("cp-a", "sess-a");
        a.current_hypotheses.push(
            Claim::new("h1", "s", Stance::Affirm, "first")
                .unwrap()
                .with_evidence([EvidenceRef::new("e")]),
        );
        a.current_hypotheses.push(
            Claim::new("h2", "s", Stance::Deny, "second")
                .unwrap()
                .with_evidence([EvidenceRef::new("e")]),
        );
        let mut b = cp("cp-b", "sess-b");
        b.current_hypotheses.push(
            Claim::new("h3", "s", Stance::Affirm, "theirs")
                .unwrap()
                .with_evidence([EvidenceRef::new("e")]),
        );

        let r = compare(Some(&a), Some(&b), at(0));
        assert_eq!(r.verdict, Verdict::Agree, "{r:#?}");
    }

    #[test]
    fn exit_codes_match_cs_diverge() {
        assert_eq!(Verdict::Agree.exit_code(), 0);
        assert_eq!(Verdict::Finding.exit_code(), 1);
        assert_eq!(Verdict::Inconclusive.exit_code(), 2);
    }
}
