// SPDX-License-Identifier: AGPL-3.0-only

//! M3 acceptance, clause by clause.
//!
//! The mission states the acceptance criterion for M3 in one sentence:
//!
//! > *changement de périmètre, intention contradictoire et preuve manquante
//! > détectés sur fixtures ; absence de checkpoint = `INCONCLUSIVE`, jamais
//! > `AGREE` ; aucun score psychologique opaque.*
//!
//! Each test below is one clause of that sentence, named after it. They read as
//! usage examples on purpose: this crate has no CLI yet, so these tests are the
//! only place a reader can see the whole flow — publish, load, compare, read the
//! citations.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_pilot_checkpoint::{
    compare, CheckpointId, CheckpointStore, DriftClass, DriftFinding, DriftReport, MissionId,
    PilotCheckpoint, SessionId, Side, Verdict,
};

fn fixture(name: &str) -> PilotCheckpoint {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_785_000_000, 0).expect("in range")
}

/// Every finding of `class` in the report.
fn of_class(report: &DriftReport, class: DriftClass) -> Vec<&DriftFinding> {
    report
        .findings
        .iter()
        .filter(|f| f.class == class)
        .collect()
}

/// The full drifted comparison, reused by most clauses.
fn drifted_report() -> DriftReport {
    compare(
        Some(&fixture("primary-claude.json")),
        Some(&fixture("copilot-codex-drifted.json")),
        now(),
    )
}

// ---------------------------------------------------------------------------
// Clause 1 — "changement de périmètre … détecté sur fixtures"
// ---------------------------------------------------------------------------

#[test]
fn a_scope_change_is_detected_and_names_the_item_that_moved() {
    let report = drifted_report();
    let findings = of_class(&report, DriftClass::ScopeChange);
    assert_eq!(findings.len(), 1, "{report:#?}");

    let finding = findings[0];
    assert_eq!(finding.verdict, Verdict::Finding);

    // The co-pilot widened the perimeter by one item, and the finding says
    // which item and which side holds it — not merely that "the scopes differ".
    let cited: Vec<(&str, &str)> = finding
        .cited_claims
        .iter()
        .map(|c| (c.subject.as_str(), c.statement.as_str()))
        .collect();
    assert_eq!(cited, vec![("scope.includes", "presence mailbox")]);
    assert_eq!(finding.cited_claims[0].side, Side::B);
}

// ---------------------------------------------------------------------------
// Clause 2 — "intention contradictoire … détectée sur fixtures"
// ---------------------------------------------------------------------------

#[test]
fn a_contradictory_intent_is_detected_and_quotes_both_pilots() {
    let report = drifted_report();
    let findings = of_class(&report, DriftClass::ContradictoryIntent);
    assert_eq!(findings.len(), 1, "{report:#?}");

    let finding = findings[0];
    assert_eq!(finding.cited_claims.len(), 2, "a finding cites both sides");
    assert_eq!(finding.cited_claims[0].side, Side::A);
    assert_eq!(finding.cited_claims[1].side, Side::B);
    assert!(finding.cited_claims[0]
        .statement
        .contains("Merge this branch"));
    assert!(finding.cited_claims[1]
        .statement
        .contains("Do not merge yet"));

    // ADVISORY-DRIFT: both assertions AND their evidence travel with the
    // finding, so the operator can check it rather than believe it.
    let locators: Vec<&str> = finding
        .evidence_refs
        .iter()
        .map(|e| e.locator.as_str())
        .collect();
    assert!(locators.contains(&"justfile"), "{locators:?}");
}

#[test]
fn a_contradictory_hypothesis_is_a_separate_class_from_a_contradictory_intent() {
    // The same subject-and-stance test, applied to a different list. Folding
    // the two together would tell an operator that the pilots disagree without
    // saying whether they disagree about the world or about the next move.
    let report = drifted_report();
    assert_eq!(
        of_class(&report, DriftClass::ContradictoryHypothesis).len(),
        1
    );
    let finding = of_class(&report, DriftClass::ContradictoryHypothesis)[0];
    assert_eq!(finding.cited_claims[0].subject, "rotation-restarts-read");
}

// ---------------------------------------------------------------------------
// Clause 3 — "preuve manquante … détectée sur fixtures"
// ---------------------------------------------------------------------------

#[test]
fn a_claim_on_a_shared_subject_with_no_evidence_is_detected() {
    let report = drifted_report();
    let findings = of_class(&report, DriftClass::MissingEvidence);
    assert_eq!(findings.len(), 1, "{report:#?}");

    let finding = findings[0];
    let offender = &finding.cited_claims[0];
    assert_eq!(offender.side, Side::B);
    assert_eq!(offender.subject, "third-provider-needs-adapter-only");
    assert!(offender.evidence.is_empty());

    // And the counterpart is cited too, which is what makes the finding
    // actionable: the other pilot did read something on this subject.
    let counterpart = &finding.cited_claims[1];
    assert_eq!(counterpart.side, Side::A);
    assert_eq!(
        counterpart.evidence[0].locator,
        "crates/cosmon-session-probe/src/port.rs"
    );
}

#[test]
fn all_three_acceptance_classes_fire_on_one_pair_of_fixtures() {
    // Not three separate fixtures: a real drifted hand-over carries several
    // defects at once, and a comparison that only finds one at a time would
    // pass three isolated tests and fail the first real relief.
    let report = drifted_report();
    assert_eq!(report.verdict, Verdict::Finding);
    for class in [
        DriftClass::ScopeChange,
        DriftClass::ContradictoryIntent,
        DriftClass::MissingEvidence,
    ] {
        assert!(
            report.has_class(class),
            "{class:?} missing from {report:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Clause 4 — "absence de checkpoint = INCONCLUSIVE, jamais AGREE"
// ---------------------------------------------------------------------------

#[test]
fn a_missing_checkpoint_is_inconclusive_and_never_agree() {
    let primary = fixture("primary-claude.json");

    for (a, b) in [(Some(&primary), None), (None, Some(&primary)), (None, None)] {
        let report = compare(a, b, now());
        assert_eq!(report.verdict, Verdict::Inconclusive);
        assert!(report.has_class(DriftClass::MissingCheckpoint));
        assert!(
            report.findings.iter().all(|f| f.verdict != Verdict::Agree),
            "no record may claim agreement when a side is absent: {report:#?}"
        );
    }
}

#[test]
fn a_copilot_that_never_published_is_inconclusive_through_the_store() {
    // The realistic shape of clause 4: the co-pilot session exists, is being
    // followed, and simply has not checkpointed yet. Reading "no record" out of
    // the store must not become "the two pilots agree".
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = CheckpointStore::new(tmp.path());
    let primary = fixture("primary-claude.json");
    store.publish(&primary).expect("publish");

    let mission = MissionId::new("task-20260731-67f2").expect("valid");
    let copilot = store
        .latest_for(&mission, &SessionId::new("sess-codex").expect("valid"))
        .expect("list");
    assert!(copilot.is_none());

    let report = compare(Some(&primary), copilot.as_ref(), now());
    assert_eq!(report.verdict, Verdict::Inconclusive);
}

#[test]
fn two_checkpoints_about_different_missions_are_inconclusive_not_a_finding() {
    // FAIL-CLOSED-AUTHORITY applied to comparison: an input we cannot make
    // sense of is "unknown", not "they disagree". Reporting a finding here
    // would manufacture a disagreement out of an operator's typo.
    let primary = fixture("primary-claude.json");
    let mut stranger = fixture("copilot-codex-aligned.json");
    stranger.mission_id = MissionId::new("task-20260101-0000").expect("valid");

    let report = compare(Some(&primary), Some(&stranger), now());
    assert_eq!(report.verdict, Verdict::Inconclusive);
    assert!(report.has_class(DriftClass::MissionMismatch));
}

#[test]
fn two_empty_checkpoints_are_inconclusive_not_agree() {
    // The subtlest way falsifier 8 could come back: both checkpoints exist, so
    // the missing-checkpoint guard does not fire, but neither says anything.
    // Nothing compared is not agreement.
    let now = now();
    let a = PilotCheckpoint::new("cp-a", "task-1", "sess-a", 1, now).expect("valid");
    let b = PilotCheckpoint::new("cp-b", "task-1", "sess-b", 1, now).expect("valid");

    let report = compare(Some(&a), Some(&b), now);
    assert_eq!(report.verdict, Verdict::Inconclusive);
    assert!(report.has_class(DriftClass::NoComparableClaim));
}

// ---------------------------------------------------------------------------
// Clause 5 — "aucun score psychologique opaque"
// ---------------------------------------------------------------------------

#[test]
fn no_report_carries_a_score_a_confidence_or_any_fractional_number() {
    // Structural, not a promise in a doc comment: walk the serialised report
    // and assert that nothing in it is the kind of value an operator could
    // mistake for a measurement of another pilot's mind.
    const FORBIDDEN_KEY_FRAGMENTS: [&str; 7] = [
        "score",
        "confidence",
        "similarity",
        "probability",
        "percent",
        "weight",
        "ratio",
    ];

    fn walk(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    let lower = key.to_lowercase();
                    for fragment in FORBIDDEN_KEY_FRAGMENTS {
                        assert!(
                            !lower.contains(fragment),
                            "{path}.{key} looks like an opaque score"
                        );
                    }
                    walk(child, &format!("{path}.{key}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{i}]"));
                }
            }
            serde_json::Value::Number(n) => {
                assert!(n.as_f64().is_none_or(|f| f.fract() == 0.0), "{path} = {n}");
            }
            _ => {}
        }
    }

    for report in [
        drifted_report(),
        compare(
            Some(&fixture("primary-claude.json")),
            Some(&fixture("copilot-codex-aligned.json")),
            now(),
        ),
        compare(Some(&fixture("primary-claude.json")), None, now()),
    ] {
        let json = serde_json::to_value(&report).expect("serialise");
        walk(&json, "report");
    }
}

// ---------------------------------------------------------------------------
// The comparison must also be able to say yes
// ---------------------------------------------------------------------------

#[test]
fn two_aligned_checkpoints_agree_and_say_what_was_compared() {
    let report = compare(
        Some(&fixture("primary-claude.json")),
        Some(&fixture("copilot-codex-aligned.json")),
        now(),
    );
    assert_eq!(report.verdict, Verdict::Agree, "{report:#?}");
    assert!(report.findings_only().is_empty());
    assert!(report.has_class(DriftClass::ScopeAgreement));

    // Four subjects agreed on: two hypotheses and two intents. An `AGREE` that
    // did not enumerate them would be indistinguishable from an `AGREE` that
    // compared nothing.
    assert_eq!(of_class(&report, DriftClass::SubjectAgreement).len(), 4);
}

// ---------------------------------------------------------------------------
// The hand-over path, end to end
// ---------------------------------------------------------------------------

#[test]
fn a_relief_pilot_resumes_from_one_record_not_from_the_transcript() {
    // CHECKPOINT-NOT-SCROLLBACK, demonstrated: everything the relief needs is
    // in a single loaded record — scope, beliefs, next moves, risks, questions.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = CheckpointStore::new(tmp.path());
    let published = store
        .publish(&fixture("primary-claude.json"))
        .expect("publish");
    assert!(published.exists());

    let mission = MissionId::new("task-20260731-67f2").expect("valid");
    let resumed = store
        .load(
            &mission,
            &CheckpointId::new("cp-claude-001").expect("valid"),
        )
        .expect("load")
        .expect("published above");

    assert_eq!(resumed.scope.includes.len(), 2);
    assert_eq!(resumed.current_hypotheses.len(), 2);
    assert_eq!(resumed.intended_next_actions.len(), 2);
    assert_eq!(resumed.open_risks.len(), 1);
    assert_eq!(resumed.unresolved_questions.len(), 1);

    // And nothing that would make it a transcript: the record carries no
    // conversation field to put one in.
    let json = serde_json::to_value(&resumed).expect("serialise");
    let keys: Vec<&String> = json.as_object().expect("object").keys().collect();
    for forbidden in ["messages", "transcript", "conversation", "scrollback"] {
        assert!(
            !keys.iter().any(|k| k.as_str() == forbidden),
            "a checkpoint must not carry {forbidden}"
        );
    }
}

#[test]
fn a_published_checkpoint_is_append_only() {
    // A finding cites a checkpoint by id. If republishing could rewrite that
    // record, every citation would be a dangling reference to whatever the file
    // says now.
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = CheckpointStore::new(tmp.path());
    let checkpoint = fixture("primary-claude.json");

    store.publish(&checkpoint).expect("first publish");
    assert!(store.publish(&checkpoint).is_err(), "second publish");
}

#[test]
fn the_same_comparison_run_twice_yields_the_same_finding_ids() {
    // An operator re-running a comparison must be able to tell a repeat from a
    // new finding. Ids are content-addressed and exclude the clock.
    let later = DateTime::from_timestamp(1_785_099_999, 0).expect("in range");
    let a = fixture("primary-claude.json");
    let b = fixture("copilot-codex-drifted.json");

    let first = compare(Some(&a), Some(&b), now());
    let second = compare(Some(&a), Some(&b), later);

    let ids =
        |r: &DriftReport| -> Vec<String> { r.findings.iter().map(|f| f.id.to_string()).collect() };
    assert_eq!(ids(&first), ids(&second));
    assert_ne!(first.created_at, second.created_at);
}
