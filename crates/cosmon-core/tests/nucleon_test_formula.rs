// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the `nucleon-test` formula + its Rust helpers.
//!
//! Two integration tests cover the admission probe end to end:
//!
//! 1. *Happy path* — a known-good candidate (the operator) scores
//!    `k/7 >= 5` on the probe.
//! 2. *Failure path* — a synthetic candidate with a rotated
//!    `nucleon_id` gets a `DEFERRED` decision AND the report names the
//!    violation.
//!
//! Both tests exercise the pure helpers in `cosmon_core::nucleon` and
//! assert against the rendered markdown so a regression in the probe
//! or in the report layer fails here loudly.
//!
//! In addition, the test loads `.cosmon/formulas/nucleon-test.formula.toml`
//! via `cosmon_core::formula::Formula::parse` and asserts that its
//! three-step shape (scan → probe → report) is preserved.

use std::collections::BTreeSet;

use cosmon_core::formula::Formula;
use cosmon_core::nucleon::{
    decide_admission, probe, AdmissionDecision, GuaranteeId, NucleonReport, NucleonScan, TestId,
    Verdict,
};

fn known_good_operator() -> NucleonScan {
    let mut observed = BTreeSet::new();
    observed.insert("you".to_string());
    NucleonScan {
        candidate: "you".to_string(),
        substrate: Some("human-operator".to_string()),
        window_days: 30,
        pilot_sessions: 7,
        authored_molecules: 23,
        sparked_edges: 23,
        carnet_entries_with_cause: 54,
        carnet_entries_total: 54,
        identity_file_present: true,
        identity_file_sealed: true,
        carnets_append_only_sealed: true,
        observed_ids: observed,
        sparked_by_complete: true,
        peer_corruption_detected: false,
        prose_readable: true,
        requires_admission_boundary: false,
        admission_boundary_present: false,
        session_overlaps: Vec::new(),
        cross_ancestry_writes: 0,
        illegible_edges: 0,
        duplicate_nucleations: 0,
        unbounded_metacognition: false,
        reference_human_carnet_present: true,
        candidate_distinguishable_from_human: true,
    }
}

#[test]
fn happy_path_known_good_candidate_scores_seven_of_seven() {
    let scan = known_good_operator();
    let probe_out = probe(&scan);

    assert!(
        probe_out.passing_count() >= 5,
        "known-good candidate must score k/7 >= 5 (got {})",
        probe_out.passing_count()
    );
    assert_eq!(
        probe_out.passing_count(),
        7,
        "known-good candidate should pass every T1..T7, not just the load-bearing subset"
    );
    for g in &probe_out.guarantees {
        assert_eq!(
            g.verdict,
            Verdict::Pass,
            "G{} not pass: {}",
            g.id,
            g.evidence
        );
    }
    assert_eq!(decide_admission(&probe_out), AdmissionDecision::Admitted);

    let report = NucleonReport::from_probe(&scan, probe_out);
    let md = report.render_markdown();
    assert!(
        md.contains("ADMITTED"),
        "rendered report must show ADMITTED"
    );
    assert!(
        md.contains("`you`"),
        "rendered report must name the candidate"
    );
    assert!(
        report.smallest_fix.is_empty(),
        "ADMITTED must have no smallest-fix entries"
    );
}

#[test]
fn failure_path_rotated_nucleon_id_is_named_in_report() {
    let mut scan = known_good_operator();
    // Synthetic T2 violation: a second id appears in the carnet.
    scan.observed_ids.insert("you-v2-rotated".to_string());

    let probe_out = probe(&scan);
    let t2 = probe_out
        .tests
        .iter()
        .find(|r| r.id == TestId::T2)
        .expect("T2 result missing");
    assert_eq!(t2.verdict, Verdict::Fail);
    assert!(
        t2.evidence.contains("you-v2-rotated"),
        "T2 evidence must name the rotated id verbatim, got: {}",
        t2.evidence
    );

    let decision = decide_admission(&probe_out);
    assert_eq!(
        decision,
        AdmissionDecision::Deferred,
        "T2 failure in the load-bearing subset must yield DEFERRED"
    );

    let report = NucleonReport::from_probe(&scan, probe_out);
    let md = report.render_markdown();
    assert!(
        md.contains("you-v2-rotated"),
        "rendered report must name the rotated id"
    );
    assert!(
        md.contains("DEFERRED"),
        "rendered report must show DEFERRED"
    );
    assert!(
        md.contains("stabilise the nucleon_id"),
        "smallest-fix must propose id stabilisation; full report:\n{md}",
    );
}

#[test]
fn formula_toml_parses_with_three_step_shape() {
    // The briefing mandates three steps: scan → probe → report.
    // Any later refactor that drops scan or renames the final step
    // without updating the briefing would break this assertion.
    let source = include_str!("../../../.cosmon/formulas/nucleon-test.formula.toml");
    let formula = Formula::parse(source).expect("nucleon-test formula must parse");
    assert_eq!(formula.name.as_str(), "nucleon-test");
    let step_ids: Vec<_> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(step_ids, vec!["scan", "probe", "report"]);
}

#[test]
fn all_guarantees_inconclusive_on_empty_scan() {
    // A scan with zero corpus must never produce Fail — only
    // Inconclusive — so the telescope-not-gate discipline holds.
    let scan = NucleonScan {
        candidate: "fresh-candidate".to_string(),
        ..NucleonScan::default()
    };
    let probe_out = probe(&scan);
    for g in &probe_out.guarantees {
        assert_eq!(
            g.verdict,
            Verdict::Inconclusive,
            "G{} must be inconclusive on empty scan",
            g.id
        );
    }
    // Load-bearing tests on empty corpus are inconclusive too —
    // which means admission defaults to DEFERRED, never REFUSED or
    // ADMITTED.
    assert_eq!(decide_admission(&probe_out), AdmissionDecision::Deferred);

    // And all three guarantee ids must appear in the probe output.
    let ids: BTreeSet<GuaranteeId> = probe_out.guarantees.iter().map(|g| g.id).collect();
    assert_eq!(ids, GuaranteeId::all().iter().copied().collect());
}
