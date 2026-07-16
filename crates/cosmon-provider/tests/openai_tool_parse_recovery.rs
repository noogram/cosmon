// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration tests for the OpenAI adapter's tool-call
//! **parse-error recovery** — the mode-C parity fix (task-20260707-4991).
//!
//! Motivation: mode C runs [`OpenAIProvider`] in-process against ollama's
//! `/v1/chat/completions`. When a local model emits a tool call whose
//! arguments trip ollama's server-side tool-call parser (an entire script
//! as one JSON string), ollama answers **HTTP 500** with a body carrying
//! `... error parsing tool call ...`. Before this fix the 500 fell through
//! to [`OpenAiError::Http`] and the spine treated it as **fatal**, killing
//! the worker on the first fumble — the model never saw the error and could
//! not self-correct (delib-20260707-50f5: 7/32 calls failed, fleet died at
//! role 1/9 with zero artefacts).
//!
//! The subprocess adapters (Claude Code, modes H/CH) feed such a failure
//! back to the model as a tool result and it self-corrects (writes its
//! script in smaller pieces), surviving 8/8. These tests pin the in-process
//! parity:
//!
//! 1. **Re-inject, not fatal** — a 500 parse-error followed by a 200 must
//!    *succeed*: the adapter splices a corrective `user` turn and re-POSTs.
//! 2. **Bounded** — a server that parse-errors forever must surface the
//!    typed [`OpenAiError::ToolCallParse`] after exactly `max_retries + 1`
//!    POSTs, so `one_turn` stays finite and the spine's termination proof
//!    holds.
//! 3. **The model sees the error** — the corrective turn lands in the
//!    re-POST body so the model can actually self-correct.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_provider::openai::{
    run_agent_loop, telemetry_for, OpenAIProvider, OpenAiError, RetryPolicy,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Responds 500 `error parsing tool call` for the first `fail_first` calls,
/// then 200 with a `finish_reason:"stop"` body. Records every inbound
/// request body so the test can assert the corrective turn was injected.
struct ParseErrorResponder {
    calls: Arc<Mutex<u32>>,
    bodies: Arc<Mutex<Vec<String>>>,
    fail_first: u32,
}

impl Respond for ParseErrorResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.bodies
            .lock()
            .expect("lock")
            .push(String::from_utf8_lossy(&request.body).into_owned());
        let mut guard = self.calls.lock().expect("lock");
        *guard += 1;
        let nth = *guard;
        if nth <= self.fail_first {
            // The ollama /v1 mode-C failure shape: HTTP 500, tool-call
            // parser rejected the emitted call.
            ResponseTemplate::new(500).set_body_json(json!({
                "error": {
                    "message": "error parsing tool call: unexpected end of JSON input"
                }
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "recovered and done" },
                    "finish_reason": "stop"
                }]
            }))
        }
    }
}

/// The win: two 500 parse-errors then a 200 must yield a clean completion —
/// the loop *re-injects and retries*, not aborts. Exactly three POSTs.
#[tokio::test]
async fn tool_parse_error_then_200_recovers() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ParseErrorResponder {
            calls: calls.clone(),
            bodies: bodies.clone(),
            fail_first: 2,
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(RetryPolicy {
            max_retries: 4,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        });

    let synthesis = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect("a tool-call parse error must NOT abort the loop");
    assert_eq!(synthesis, "recovered and done");

    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 3,
        "expected 500, 500, then 200 — the loop must re-inject twice then succeed"
    );

    // The model must actually SEE the parse error: the second and third
    // POST bodies carry the corrective `user` turn absent from the first.
    let bodies = bodies.lock().expect("lock");
    assert!(
        !bodies[0].contains("could not be parsed"),
        "first POST must not yet carry a correction"
    );
    assert!(
        bodies[1].contains("could not be parsed") && bodies[1].contains("smaller"),
        "second POST must re-inject the parse error as a corrective turn; got: {}",
        bodies[1]
    );
}

/// The bound: a server that parse-errors forever must surface the typed
/// [`OpenAiError::ToolCallParse`] after exactly `max_retries + 1` POSTs —
/// never an unbounded hammer, never a stringly-typed `Http`.
#[tokio::test]
async fn sustained_tool_parse_error_surfaces_typed_after_bounded_retries() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ParseErrorResponder {
            calls: calls.clone(),
            bodies: bodies.clone(),
            fail_first: u32::MAX, // never recovers
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(RetryPolicy {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        });

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("a sustained parse error must eventually surface a typed error");
    assert!(
        matches!(err, OpenAiError::ToolCallParse { .. }),
        "exhausted retries must surface the typed ToolCallParse, not Http; got: {err:?}"
    );

    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 4,
        "first attempt + 3 retries = 4 POSTs before surfacing the failure"
    );
}

/// Regression guard: `RetryPolicy::DISABLED` performs exactly one POST and
/// surfaces the typed [`OpenAiError::ToolCallParse`] immediately — a caller
/// delegating pacing to an external scheduler is never silently retried, but
/// still gets the typed class rather than a fatal `Http`.
#[tokio::test]
async fn disabled_policy_surfaces_tool_parse_without_retry() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ParseErrorResponder {
            calls: calls.clone(),
            bodies,
            fail_first: u32::MAX,
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(RetryPolicy::DISABLED);

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("parse error must surface immediately under DISABLED");
    assert!(matches!(err, OpenAiError::ToolCallParse { .. }));

    let n = *calls.lock().expect("lock");
    assert_eq!(n, 1, "DISABLED policy must perform exactly one POST");
}

/// Ride-along (delib-20260707-df9b): each tool-parse re-inject lands a typed
/// `AdapterLivenessProbed { verdict: "retried", reason: "tool_parse_reinject" }`
/// row on `events.jsonl`. This makes the mode-C robustness bench's pass/fail
/// predicate disk-evaluable forever (`grep -c tool_parse_reinject
/// events.jsonl`) instead of scraping a tmux pane.
#[tokio::test]
async fn tool_parse_reinject_emits_retried_event_on_disk() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ParseErrorResponder {
            calls: calls.clone(),
            bodies,
            fail_first: 2,
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mol_id = MoleculeId::new("task-20260707-4991").expect("mol id");
    let worker_id = WorkerId::new("polecat-tpr0").expect("worker id");
    let telemetry = telemetry_for(
        mol_id,
        worker_id,
        dir.path().to_owned(),
        "reinject-trail-uuid",
    );

    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(RetryPolicy {
            max_retries: 4,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        });

    let synthesis = run_agent_loop(&provider, "Briefing.", dir.path(), Some(&telemetry))
        .await
        .expect("recovers after two parse errors");
    assert_eq!(synthesis, "recovered and done");

    let events =
        std::fs::read_to_string(dir.path().join("events.jsonl")).expect("events.jsonl must exist");
    let n_reinject = events
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| {
            r.get("type").and_then(|t| t.as_str()) == Some("adapter_liveness_probed")
                && r.get("probe_result")
                    .and_then(|p| p.get("reason"))
                    .and_then(|s| s.as_str())
                    == Some("tool_parse_reinject")
        })
        .count();
    assert_eq!(
        n_reinject, 2,
        "two tool-parse re-injects must emit two `tool_parse_reinject` events; got: {events}"
    );
}
