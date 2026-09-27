// SPDX-License-Identifier: AGPL-3.0-only

//! Characterize the Claude observation boundary audited for issue #73.
//!
//! These fixtures are synthetic, shaped after a measured inter-turn change;
//! no conversation content or private log identifiers are reproduced.

use cosmon_core::model_realization::realized_models_from_claude_jsonl;

#[test]
fn claude_model_change_between_turns_needs_no_bootstrap_record() {
    // Disk logs carry the model on assistant records, including repeated
    // records for one response. A turn boundary does not carry the new model.
    let content = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","model":"model-a"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","model":"model-a"}}"#,
        "\n",
        r#"{"type":"system","subtype":"turn_duration","durationMs":1,"messageCount":2}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","model":"model-b"}}"#,
    );
    let models = realized_models_from_claude_jsonl(content);
    let names: Vec<_> = models.iter().map(|model| model.as_str()).collect();
    assert_eq!(names, ["model-a", "model-b"]);
}

#[test]
fn claude_trajectory_alone_cannot_distinguish_missing_model_evidence() {
    // This pins a LIMITATION, not a freshness guarantee. A future assessment
    // must retain record coverage before trajectory deduplication loses it.
    let initial = r#"{"type":"assistant","message":{"model":"model-a"}}"#;
    let complete = format!("{initial}\n{initial}\n{initial}");
    let missing = concat!(
        r#"{"type":"assistant","message":{"model":"model-a"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant"}}"#,
    );
    let observed = realized_models_from_claude_jsonl(missing);
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].as_str(), "model-a");
    assert_eq!(observed, realized_models_from_claude_jsonl(&complete));
}
