// SPDX-License-Identifier: AGPL-3.0-only

//! Golden-fixture tests for the realized-model parsers (round-3 / F-04).
//!
//! The fixtures under `tests/fixtures/realized_model/` are **real captured
//! producer bytes** (anonymized — see the sibling `README.md` for provenance
//! and procedure), not synthetic `concat!` strings: they carry the full field
//! surface Claude Code and codex actually write, so a producer schema drift
//! breaks these tests instead of passing silently against an invented shape.

use cosmon_core::model_realization::{
    realized_models_from_claude_jsonl, realized_models_from_codex_session,
};

/// Real Claude Code 2.1.195 session-log lines: a non-`init` `system` line
/// (`turn_duration` — must contribute nothing) followed by a real `assistant`
/// turn whose `message.model` is the realization.
#[test]
fn golden_claude_session_yields_assistant_model_only() {
    let content = include_str!("fixtures/realized_model/claude-session.jsonl");
    let models = realized_models_from_claude_jsonl(content);
    assert_eq!(models.len(), 1, "exactly the assistant turn's model");
    assert_eq!(models[0].as_str(), "claude-opus-4-8");
}

/// Real `codex_cli_rs` 0.36.0 rollout lines: `session_meta` carries no model
/// (config, not realization) and `turn_context.payload.model` names what ran.
#[test]
fn golden_codex_session_yields_turn_context_model() {
    let content = include_str!("fixtures/realized_model/codex-session.jsonl");
    let models = realized_models_from_codex_session(content);
    assert_eq!(models.len(), 1, "exactly the turn_context model");
    assert_eq!(models[0].as_str(), "gpt-5-codex");
}

/// The codex `session_meta` line alone must yield nothing — it names the cwd
/// and CLI version, never a realized model (the F-04 config-vs-realization
/// counter-example, on real bytes).
#[test]
fn golden_codex_session_meta_alone_is_not_a_realization() {
    let content = include_str!("fixtures/realized_model/codex-session.jsonl");
    let meta_line = content
        .lines()
        .find(|l| l.contains("\"session_meta\""))
        .expect("fixture carries a session_meta line");
    assert!(realized_models_from_codex_session(meta_line).is_empty());
}
