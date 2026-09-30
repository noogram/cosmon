// SPDX-License-Identifier: AGPL-3.0-only

//! Executable examples of provider-specific, pure model-evidence assessment.

use cosmon_core::model_realization::{
    assess_claude_model_evidence, assess_codex_model_evidence, LatestModelEvidence,
    ModelEvidenceAccumulator, ModelEvidenceCoverage, ModelEvidenceGrammar, ModelEvidenceInputLoss,
    ModelEvidenceReason, ModelEvidenceStats,
};

fn names(assessment: &cosmon_core::model_realization::ModelEvidenceAssessment) -> Vec<&str> {
    assessment.trajectory.iter().map(|id| id.as_str()).collect()
}

#[test]
fn claude_counts_records_before_collapsing_a_stable_trajectory() {
    let complete = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
    );
    let assessment = assess_claude_model_evidence(complete.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(assessment.coverage, ModelEvidenceCoverage::CompleteRecords);
    assert_eq!(
        assessment.latest,
        LatestModelEvidence::ModelReported(assessment.trajectory[0].clone())
    );
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Claude {
            assistant_records: 3,
            usable_model_records: 3,
            placeholder_records: 0,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
    assert_eq!(assessment.complete_bytes, complete.len() as u64);
    assert_eq!(assessment.last_usable_model_at, Some(complete.len() as u64));
}

#[test]
fn a_later_good_response_recovers_latest_evidence_but_not_the_gap() {
    let input = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"assistant\",\"message\":{}}\n",
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
    );
    let assessment = assess_claude_model_evidence(input.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(
        assessment.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::MissingAssistantModel])
    );
    assert_eq!(
        assessment.latest,
        LatestModelEvidence::ModelReported(assessment.trajectory[0].clone())
    );
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Claude {
            assistant_records: 3,
            usable_model_records: 2,
            placeholder_records: 0,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
}

#[test]
fn bootstrap_is_historical_but_not_response_confirmation() {
    let input = "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"model-a\"}\n";
    let assessment = assess_claude_model_evidence(input.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(
        assessment.coverage,
        ModelEvidenceCoverage::NoResponseEvidence
    );
    assert_eq!(assessment.latest, LatestModelEvidence::NoResponse);
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Claude {
            assistant_records: 0,
            usable_model_records: 0,
            placeholder_records: 0,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
}

#[test]
fn bootstrap_without_its_model_is_degraded_not_confirmed() {
    let input = b"{\"type\":\"system\",\"subtype\":\"init\"}\n";
    let assessment = assess_claude_model_evidence(input, true);
    assert!(assessment.trajectory.is_empty());
    assert_eq!(
        assessment.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::InvalidModel])
    );
}

#[test]
fn placeholder_is_not_usable_response_evidence() {
    let input = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\"}}\n",
    );
    let assessment = assess_claude_model_evidence(input.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(
        assessment.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::PlaceholderModel])
    );
    assert_eq!(assessment.latest, LatestModelEvidence::ModelMissing);
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Claude {
            assistant_records: 2,
            usable_model_records: 1,
            placeholder_records: 1,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
}

#[test]
fn malformed_complete_record_and_final_torn_tail_are_distinct() {
    let input = b"{broken}\n{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n{\"type\":\"assistant\"";
    let live = assess_claude_model_evidence(input, false);
    assert_eq!(
        live.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::MalformedRecord])
    );
    assert!(live.trailing_bytes > 0);
    let final_assessment = assess_claude_model_evidence(input, true);
    assert_eq!(
        final_assessment.coverage,
        ModelEvidenceCoverage::Degraded(vec![
            ModelEvidenceReason::MalformedRecord,
            ModelEvidenceReason::UnassessedTail,
        ])
    );
    assert_eq!(final_assessment.latest, LatestModelEvidence::Indeterminate);
}

#[test]
fn empty_and_undelimited_input_never_claim_complete_coverage() {
    let empty = assess_claude_model_evidence(b"", true);
    assert_eq!(empty.coverage, ModelEvidenceCoverage::NoResponseEvidence);
    assert_eq!(empty.complete_bytes, 0);
    let torn = b"{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}";
    assert_eq!(
        assess_claude_model_evidence(torn, false).coverage,
        ModelEvidenceCoverage::NoResponseEvidence
    );
    assert_eq!(
        assess_claude_model_evidence(torn, true).coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::UnassessedTail])
    );
}

#[test]
fn invalid_encoding_and_invalid_model_type_degrade_without_inventing_a_model() {
    let malformed = assess_claude_model_evidence(b"\xff\n", true);
    assert_eq!(
        malformed.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::MalformedRecord])
    );
    assert_eq!(
        malformed.stats,
        ModelEvidenceStats::Claude {
            assistant_records: 0,
            usable_model_records: 0,
            placeholder_records: 0,
            malformed_records: 1,
            unclassified_records: 0,
        }
    );
    let invalid = assess_codex_model_evidence(
        b"{\"type\":\"turn_context\",\"payload\":{\"model\":1}}\n",
        true,
    );
    assert_eq!(
        invalid.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::InvalidModel])
    );
    assert!(invalid.trajectory.is_empty());
}

#[test]
fn unknown_shape_degrades_but_known_ordinary_and_additive_fields_do_not() {
    let ordinary = concat!(
        "{\"type\":\"user\",\"extra\":1}\n",
        "{\"type\":\"system\",\"subtype\":\"turn_duration\"}\n",
        "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\",\"extra\":1}}\n",
    );
    assert_eq!(
        assess_claude_model_evidence(ordinary.as_bytes(), true).coverage,
        ModelEvidenceCoverage::CompleteRecords
    );
    let unknown = format!("{ordinary}{{\"type\":\"new_switch\"}}\n");
    assert_eq!(
        assess_claude_model_evidence(unknown.as_bytes(), true).coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::UnclassifiedRecord])
    );
}

#[test]
fn codex_sparse_settings_do_not_become_per_response_confirmation() {
    let input = concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
    );
    let assessment = assess_codex_model_evidence(input.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(assessment.coverage, ModelEvidenceCoverage::SparseSettings);
    assert_eq!(assessment.latest, LatestModelEvidence::Indeterminate);
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Codex {
            response_records: 2,
            settings_records: 1,
            usable_model_records: 1,
            placeholder_records: 0,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
}

#[test]
fn codex_switch_and_effort_only_settings_preserve_model_trajectory() {
    let input = concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"reasoning_effort\":\"high\"}}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"model-b\"}}}\n",
    );
    let assessment = assess_codex_model_evidence(input.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a", "model-b"]);
    assert_eq!(assessment.coverage, ModelEvidenceCoverage::SparseSettings);
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Codex {
            response_records: 0,
            settings_records: 3,
            usable_model_records: 2,
            placeholder_records: 0,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
}

#[test]
fn codex_unknown_event_subtype_degrades_but_effort_only_change_does_not() {
    let ordinary = b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"reasoning_effort\":\"high\"}}}\n";
    assert_eq!(
        assess_codex_model_evidence(ordinary, true).coverage,
        ModelEvidenceCoverage::NoResponseEvidence
    );
    let unknown = b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"new_settings_shape\"}}\n";
    assert_eq!(
        assess_codex_model_evidence(unknown, true).coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::UnclassifiedRecord])
    );
}

#[test]
fn codex_placeholder_settings_do_not_leave_old_settings_as_latest() {
    let input = concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"<synthetic>\"}}}\n",
    );
    let assessment = assess_codex_model_evidence(input.as_bytes(), true);
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(assessment.latest, LatestModelEvidence::Indeterminate);
    assert_eq!(
        assessment.coverage,
        ModelEvidenceCoverage::Degraded(vec![ModelEvidenceReason::PlaceholderModel])
    );
    assert_eq!(
        assessment.stats,
        ModelEvidenceStats::Codex {
            response_records: 0,
            settings_records: 2,
            usable_model_records: 1,
            placeholder_records: 1,
            malformed_records: 0,
            unclassified_records: 0,
        }
    );
}

#[test]
fn an_ignored_shape_can_hide_a_switch_without_our_detecting_it() {
    let stable = concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
    );
    let hidden = concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"model-a\"}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"renamed_switch\":\"model-b\"}}\n",
    );
    let a = assess_codex_model_evidence(stable.as_bytes(), true);
    let b = assess_codex_model_evidence(hidden.as_bytes(), true);
    assert_eq!(a.trajectory, b.trajectory);
    assert_eq!(a.coverage, b.coverage);
    assert_eq!(a.stats, b.stats);
    assert_eq!(a.latest, b.latest);
}

#[test]
fn chunk_partition_does_not_change_assessed_records() {
    let input = b"{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n{broken}\n{\"type\":\"assistant\",\"message\":{}}\n";
    let expected = assess_claude_model_evidence(input, true);
    for split in 0..=input.len() {
        let mut accumulator = ModelEvidenceAccumulator::new(ModelEvidenceGrammar::Claude);
        accumulator.push(&input[..split]);
        accumulator.push(&input[split..]);
        assert_eq!(accumulator.finish(), expected, "split at byte {split}");
    }
}

#[test]
fn explicit_reader_loss_qualifies_prior_model_without_erasing_it() {
    let mut accumulator = ModelEvidenceAccumulator::new(ModelEvidenceGrammar::Claude);
    accumulator.push(b"{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n");
    accumulator.note_input_loss(ModelEvidenceInputLoss::ReadFailure);
    accumulator.note_input_loss(ModelEvidenceInputLoss::OversizeRecord);
    accumulator.note_input_loss(ModelEvidenceInputLoss::ContinuityLost);
    let assessment = accumulator.finish();
    assert_eq!(names(&assessment), ["model-a"]);
    assert_eq!(assessment.latest, LatestModelEvidence::Indeterminate);
    assert_eq!(
        assessment.coverage,
        ModelEvidenceCoverage::Degraded(vec![
            ModelEvidenceReason::ReadFailure,
            ModelEvidenceReason::OversizeRecord,
            ModelEvidenceReason::ContinuityLost,
        ])
    );
}
