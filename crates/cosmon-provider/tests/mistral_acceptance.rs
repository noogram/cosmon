// SPDX-License-Identifier: AGPL-3.0-only

//! Acceptance test for the **EU-sovereign Mistral Large warm standby**
//! (a "diversify-now" posture: keep a second provider warm).
//!
//! # The structural claim
//!
//! Mistral Large is reached through the **existing** `openai` adapter with
//! `base_url = https://api.mistral.ai` — Mistral's API is OpenAI-compatible,
//! so there is **no new adapter, no new provider, no new spawn arm**. This
//! is the load-bearing claim of the deliberation's code correction
//! (torvalds, reading the actual 5-arm dispatch `match`): the only true code
//! add is the `cosmon-core::egress` Mistral row, the rest is config + this
//! test.
//!
//! # Why a *multi-step* round-trip
//!
//! The hedge has to survive the demanding verb class, not just a one-shot
//! completion. [`cosmon_provider::degradation::VerbClass::MultiStepAgentic`]
//! is the cosmon worker's real workload — read a file, act on it, synthesise
//! — and it is `Reliable` only on a `Frontier`-tier provider. Mistral Large
//! is that frontier tier, so this test scripts a **two-turn** exchange
//! through the Mistral-configured `OpenAIProvider`:
//!
//! 1. turn 1 → the model emits a `read_file` **tool call**;
//! 2. the harness spine executes the tool against `work_dir` and re-injects
//!    the result;
//! 3. turn 2 → the model **stops** with a one-line synthesis.
//!
//! Two POSTs hit `/v1/chat/completions`, proving the full agentic loop —
//! not merely a single `text → stop` — round-trips through the sovereign leg.
//!
//! # Assertions
//!
//! 1. the agent loop completes end-to-end against the Mistral-shaped mock;
//! 2. the chosen model identifier (`mistral-large-latest`) reaches the wire
//!    **verbatim** on every turn — cosmon must not silently rewrite it to
//!    `gpt-4o-mini` (the silent-failure trap a sovereign hedge cannot
//!    tolerate);
//! 3. exactly two POSTs landed — the tool round actually happened, so this
//!    is a `MultiStepAgentic` exchange and not a degenerate single turn.
//!
//! Structural, deterministic, no external dependency — runs on every
//! `cargo test --workspace`.

#![cfg(feature = "http")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cosmon_provider::openai::{run_agent_loop as openai_run, OpenAIProvider};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Mistral's flagship frontier model — the warm-standby default declared in
/// `[adapters.openai].default_model`. The identical literal must reach the
/// wire on every turn (assertion #2).
const MISTRAL_MODEL: &str = "mistral-large-latest";

/// A file the scripted `read_file` tool call targets, planted in `work_dir`
/// so the tool actually succeeds and the loop advances to the second turn.
const NOTE_NAME: &str = "note.txt";
const NOTE_BODY: &str = "sovereign-hedge-ok";

/// Two-turn responder emulating a Mistral Large worker: a `read_file` tool
/// call on turn 1, a `stop` synthesis on turn 2. Branches on the number of
/// requests already captured.
struct ScriptedMistral {
    captured: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Respond for ScriptedMistral {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: serde_json::Value =
            serde_json::from_slice(&req.body).expect("mistral req body must be JSON");
        let turn = {
            let mut guard = self.captured.lock().expect("lock");
            guard.push(body);
            guard.len()
        };

        match turn {
            // Turn 1 — emit a tool call. `finish_reason: "tool_calls"` drives
            // the spine to execute `read_file` and loop with the result.
            1 => ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-mistral-1",
                "object": "chat.completion",
                "created": 1_797_000_000_u64,
                "model": MISTRAL_MODEL,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": serde_json::Value::Null,
                        "tool_calls": [{
                            "id": "call_read_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": json!({"path": NOTE_NAME}).to_string()
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 12, "completion_tokens": 8, "total_tokens": 20}
            })),
            // Turn 2 — the model has the file content; it stops with a
            // one-line synthesis.
            _ => ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-mistral-2",
                "object": "chat.completion",
                "created": 1_797_000_001_u64,
                "model": MISTRAL_MODEL,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "mistral-large-synthesis"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 30, "completion_tokens": 5, "total_tokens": 35}
            })),
        }
    }
}

#[tokio::test]
async fn mistral_large_round_trips_a_multi_step_agentic_request() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ScriptedMistral {
            captured: captured.clone(),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let work_dir: PathBuf = dir.path().to_owned();
    // Plant the file the tool call will read so the tool succeeds and the
    // loop reaches its second turn.
    std::fs::write(work_dir.join(NOTE_NAME), NOTE_BODY).expect("plant note");

    // THE SEAM under test: the EXISTING OpenAI provider, repointed at Mistral
    // via `with_base_url` — exactly what `cs tackle --adapter openai` does
    // when `[adapters.openai].base_url = https://api.mistral.ai` is declared.
    // No Mistral-specific provider code exists or is needed.
    let provider = OpenAIProvider::with_base_url("mistral-key", MISTRAL_MODEL, server.uri());

    let synthesis = openai_run(
        &provider,
        "read note.txt and report what it says",
        &work_dir,
        None,
    )
    .await
    .expect("multi-step agentic loop must complete against the Mistral endpoint");

    assert_eq!(synthesis, "mistral-large-synthesis");

    let captured = captured.lock().expect("lock");
    // Two POSTs = the tool round actually happened. This is the
    // MultiStepAgentic shape, not a degenerate single turn.
    assert_eq!(
        captured.len(),
        2,
        "expected a two-turn exchange (tool call + stop) through the Mistral path"
    );
    // The model identifier must survive verbatim on EVERY turn — a silent
    // rewrite to gpt-4o-mini would route the sovereign hedge back to a US
    // provider, defeating the entire point of the warm standby.
    for (i, req) in captured.iter().enumerate() {
        assert_eq!(
            req.get("model").and_then(|m| m.as_str()),
            Some(MISTRAL_MODEL),
            "turn {}: cosmon must not rewrite the model identifier on the Mistral path",
            i + 1
        );
    }
}
