// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration test for the Anthropic Direct-API adapter.
//!
//! This test exercises the **full harness spine** against a fake
//! Anthropic `/v1/messages` endpoint that emits a `tool_use` block on
//! turn 1 and a final `text` block on turn 2. Asserts:
//!
//! - The spine invokes the registered `edit_file` tool with the
//!   model's input. (`write_file` was retired in favour of `edit_file`
//!   with empty `search` as the v0 way to create a new file.)
//! - The artifact lands inside `work_dir`.
//! - The synthesis matches the model's final text.
//! - The request body to `/v1/messages` has the Anthropic-shape
//!   `tool_result` user-message on turn 2 (NOT OpenAI's `role: "tool"`).
//!
//! That last assertion is the load-bearing one for ADR-102 §C5: the
//! [`cosmon_agent_harness::MessageLog`] trait must absorb the
//! schema divergence between OpenAI and Anthropic, and the only way
//! to know it did is to inspect the second turn's request body.
//!
//! No `ANTHROPIC_API_KEY` required — the test runs on every
//! `cargo test --workspace`.

#![cfg(feature = "http")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cosmon_provider::anthropic::{run_agent_loop, AnthropicProvider};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Custom responder that scripts the two-turn dance and captures
/// every inbound request body so the test can assert on the wire
/// shape of turn 2 (the one carrying tool_result blocks).
struct ScriptedAnthropic {
    captured: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Respond for ScriptedAnthropic {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).expect("request body must be valid JSON");
        let mut guard = self.captured.lock().expect("lock");
        guard.push(body.clone());
        let turn = guard.len();
        drop(guard);

        // Turn 1 → emit a tool_use block that asks the runtime to
        // create `haiku.md` via `edit_file` (empty search = create
        // file, the v0 idiom; `write_file` was retired per
        // `delib-20260518-5178` C2). Turn 2 → emit a final text
        // block; no further tool calls means the spine returns the
        // text as the synthesis.
        if turn == 1 {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg-1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_abc123",
                        "name": "edit_file",
                        "input": {
                            "edits": [
                                {
                                    "path": "haiku.md",
                                    "search": "",
                                    "replace": "old pond\na frog jumps\nsplash"
                                }
                            ]
                        }
                    }
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "output_tokens": 20}
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg-2",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-7",
                "content": [
                    {"type": "text", "text": "haiku written"}
                ],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 30, "output_tokens": 10}
            }))
        }
    }
}

#[tokio::test]
async fn anthropic_spine_executes_tool_call_and_returns_synthesis() {
    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let responder = ScriptedAnthropic {
        captured: captured.clone(),
    };

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let work_dir: PathBuf = dir.path().to_owned();

    let provider = AnthropicProvider::with_base_url("test-key", "claude-opus-4-7", server.uri());

    let synthesis = run_agent_loop(
        &provider,
        "Please write a haiku and save it to haiku.md.",
        &work_dir,
        None,
    )
    .await
    .expect("agent loop must complete");

    // Assertion 1: synthesis is the model's final text.
    assert_eq!(synthesis, "haiku written");

    // Assertion 2: the edit_file tool actually ran against work_dir.
    let written = std::fs::read_to_string(work_dir.join("haiku.md")).expect("haiku.md must exist");
    assert!(written.contains("frog jumps"));

    // Assertion 3: two POSTs, and the turn-2 request body carries the
    // Anthropic-shape tool_result (a user-role message whose content is
    // a block array with one tool_result block, NOT OpenAI's
    // role: "tool" envelope).
    let captured = captured.lock().expect("lock");
    assert_eq!(
        captured.len(),
        2,
        "spine must POST twice (initial + after tool result)"
    );

    let turn2 = &captured[1];
    let messages = turn2
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("messages array on turn 2");
    // Last message must be the user-role tool_result envelope.
    let last = messages.last().expect("non-empty");
    assert_eq!(
        last.get("role").and_then(|r| r.as_str()),
        Some("user"),
        "tool result lives in a user message (Anthropic envelope), NOT a tool role (OpenAI)"
    );
    let content = last
        .get("content")
        .and_then(|c| c.as_array())
        .expect("content block array");
    assert_eq!(content.len(), 1, "exactly one tool_result block expected");
    assert_eq!(
        content[0].get("type").and_then(|t| t.as_str()),
        Some("tool_result")
    );
    assert_eq!(
        content[0].get("tool_use_id").and_then(|t| t.as_str()),
        Some("toolu_abc123")
    );

    // Assertion 4: system prompt sits at top-level, not inside messages.
    assert!(
        turn2.get("system").and_then(|s| s.as_str()).is_some(),
        "system prompt must be at top-level (Anthropic envelope)"
    );
    let any_system_role = messages
        .iter()
        .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"));
    assert!(
        !any_system_role,
        "no system-role message in `messages` (that is OpenAI)"
    );

    // Assertion 5: the tool advertised on the wire uses input_schema,
    // not OpenAI's nested `function.parameters`.
    let tools = turn2
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    assert!(!tools.is_empty(), "tool catalogue must be sent");
    assert!(
        tools[0].get("input_schema").is_some(),
        "Anthropic tool envelope uses input_schema (not OpenAI parameters)"
    );
    assert!(
        tools[0].get("function").is_none(),
        "no nested `function` field (that is OpenAI)"
    );
}
