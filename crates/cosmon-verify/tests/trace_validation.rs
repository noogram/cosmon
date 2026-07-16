// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end validator tests against disk fixtures.
//!
//! These fixtures are hand-written examples of the shapes we expect to see
//! in real `.cosmon/state/events.jsonl` logs. If the real wire format drifts,
//! these tests will fail before the validator silently rots.

use std::path::PathBuf;

use cosmon_verify::{baseline_invariants, TraceValidator, ValidationOutcome};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn validator() -> TraceValidator {
    TraceValidator::new(baseline_invariants())
}

#[test]
fn ok_minimal_fixture_certifies() {
    let outcome = validator()
        .validate_path(&fixture("ok-minimal.jsonl"))
        .unwrap();
    match outcome {
        ValidationOutcome::Ok {
            events_replayed,
            molecules_seen,
            skipped_unknown,
        } => {
            assert_eq!(events_replayed, 5);
            assert_eq!(molecules_seen, 1);
            assert_eq!(skipped_unknown, 0);
        }
        other @ ValidationOutcome::Violation { .. } => {
            panic!("expected certification, got {other:?}")
        }
    }
}

#[test]
fn orphan_merge_fixture_is_caught() {
    let outcome = validator()
        .validate_path(&fixture("violation-orphan-merge.jsonl"))
        .unwrap();
    match outcome {
        ValidationOutcome::Violation { violation, .. } => {
            assert_eq!(violation.invariant, "merge_completion_pairs_dispatch");
        }
        other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
    }
}
