// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration tests for the OpenAI adapter's **transient
//! server/transport retry** — the mode-C fleet-survival fix (M1 of
//! delib-20260707-df9b).
//!
//! Motivation: mode C runs [`OpenAIProvider`] in-process against ollama's
//! `/v1/chat/completions`. A transient 5xx (server hiccup, model reload)
//! used to fall through to the overloaded [`OpenAiError::Http`], which the
//! spine treated as **fatal**, killing the worker on the first blip — the
//! same failure class as the tool-call parse rejection, one layer down. M1
//! splits `Http` into the fatal 4xx/protocol variant and a retryable
//! [`OpenAiError::ServerError`] (`status = Some(5xx)` for a server response,
//! `None` for a pre-response transport failure), then routes both it and the
//! rate-limit through one `is_retryable` gate with the *existing*
//! `backoff_delay` / `RetryPolicy` machinery.
//!
//! These tests pin the surviving behaviour:
//!
//! 1. **5xx → retried, not fatal** — a 500 followed by a 200 must *succeed*.
//! 2. **4xx → fatal, exactly one POST** — a 400 is a malformed request; a
//!    retry loop must not pound it.
//! 3. **Bounded** — a server that 5xx-es forever surfaces the typed
//!    [`OpenAiError::ServerError`] after exactly `max_retries + 1` POSTs.
//! 4. **Disk-evaluable retry trail** — the `AdapterLivenessProbed { Retried }`
//!    event lands on `events.jsonl` so a bench greps it instead of scraping a
//!    tmux pane.

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

/// Responds HTTP `status` for the first `fail_first` calls, then 200 with a
/// `finish_reason:"stop"` body. Records the call count so the test can assert
/// the exact number of POSTs.
struct StatusResponder {
    calls: Arc<Mutex<u32>>,
    fail_first: u32,
    status: u16,
    body: serde_json::Value,
}

impl Respond for StatusResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut guard = self.calls.lock().expect("lock");
        *guard += 1;
        let nth = *guard;
        if nth <= self.fail_first {
            ResponseTemplate::new(self.status).set_body_json(self.body.clone())
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

fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 4,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(10),
    }
}

/// The win: two transient 500s then a 200 must yield a clean completion —
/// the loop re-POSTs, not aborts. Exactly three POSTs.
#[tokio::test]
async fn transient_5xx_then_200_recovers() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StatusResponder {
            calls: calls.clone(),
            fail_first: 2,
            status: 500,
            body: json!({ "error": { "type": "server_error", "message": "upstream hiccup" } }),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(fast_retry());

    let synthesis = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect("a transient 5xx must NOT abort the loop");
    assert_eq!(synthesis, "recovered and done");

    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 3,
        "expected 500, 500, then 200 — two retries then success"
    );
}

/// A 503 with a generic body (no quota / content-filter / tool-parse signal)
/// is also transient and recovers.
#[tokio::test]
async fn transient_503_then_200_recovers() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StatusResponder {
            calls: calls.clone(),
            fail_first: 1,
            status: 503,
            body: json!({ "error": { "message": "service unavailable" } }),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(fast_retry());

    let synthesis = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect("a transient 503 must NOT abort the loop");
    assert_eq!(synthesis, "recovered and done");
    assert_eq!(*calls.lock().expect("lock"), 2);
}

/// A 400 is a malformed request — fatal, exactly one POST. A retry loop must
/// not pound a client error (the de-overloading half of M1).
#[tokio::test]
async fn client_4xx_is_fatal_single_post() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StatusResponder {
            calls: calls.clone(),
            fail_first: u32::MAX,
            status: 400,
            body: json!({ "error": { "type": "invalid_request_error", "message": "bad model" } }),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(fast_retry());

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("a 400 must surface as a fatal error");
    assert!(
        matches!(err, OpenAiError::Http(_)),
        "a 4xx must stay on the non-retryable Http variant; got: {err:?}"
    );
    assert_eq!(
        *calls.lock().expect("lock"),
        1,
        "a fatal 4xx must perform exactly one POST — no retry"
    );
}

/// The bound: a server that 5xx-es forever surfaces the typed
/// [`OpenAiError::ServerError`] after exactly `max_retries + 1` POSTs — never
/// an unbounded hammer, never a stringly-typed `Http`.
#[tokio::test]
async fn sustained_5xx_surfaces_typed_server_error_after_bounded_retries() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StatusResponder {
            calls: calls.clone(),
            fail_first: u32::MAX,
            status: 502,
            body: json!({ "error": { "message": "bad gateway" } }),
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
        .expect_err("a sustained 5xx must eventually surface a typed error");
    match err {
        OpenAiError::ServerError { status, .. } => {
            assert_eq!(status, Some(502), "must preserve the 5xx status");
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
    assert_eq!(
        *calls.lock().expect("lock"),
        4,
        "first attempt + 3 retries = 4 POSTs before surfacing the failure"
    );
}

/// A pre-response transport failure (the endpoint is unreachable — nothing is
/// listening) is retryable too: `ServerError { status: None }`. The loop
/// re-POSTs the bounded number of times, then surfaces the typed error.
#[tokio::test]
async fn unreachable_endpoint_is_bounded_retryable_transport_error() {
    // Bind then drop a listener so the port is (almost certainly) closed —
    // a connect attempt fails pre-response.
    let dir = tempfile::tempdir().expect("tempdir");
    // 127.0.0.1:1 is the classic unreachable target (privileged, unbound).
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", "http://127.0.0.1:1")
        .with_timeout(Duration::from_millis(200))
        .with_retry_policy(RetryPolicy {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
        });

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("an unreachable endpoint must surface a transport error");
    assert!(
        matches!(err, OpenAiError::ServerError { status: None, .. }),
        "a pre-response transport failure must be ServerError with status=None; got: {err:?}"
    );
    assert!(
        err.is_retryable(),
        "a transport ServerError must be retryable"
    );
}

/// Ride-along: each transient retry lands a typed
/// `AdapterLivenessProbed { verdict: "retried" }` row on `events.jsonl` with a
/// greppable `reason`, so a mode-C robustness bench evaluates recovery from
/// disk rather than scraping a tmux pane (delib-20260707-df9b).
#[tokio::test]
async fn transient_5xx_emits_retried_event_on_disk() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StatusResponder {
            calls: calls.clone(),
            fail_first: 2,
            status: 500,
            body: json!({ "error": { "message": "hiccup" } }),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mol_id = MoleculeId::new("task-20260707-3fd9").expect("mol id");
    let worker_id = WorkerId::new("polecat-r5xx").expect("worker id");
    let telemetry = telemetry_for(mol_id, worker_id, dir.path().to_owned(), "retry-trail-uuid");

    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri())
        .with_retry_policy(fast_retry());

    let synthesis = run_agent_loop(&provider, "Briefing.", dir.path(), Some(&telemetry))
        .await
        .expect("recovers after two 5xx");
    assert_eq!(synthesis, "recovered and done");

    let events =
        std::fs::read_to_string(dir.path().join("events.jsonl")).expect("events.jsonl must exist");
    let retried: Vec<serde_json::Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| {
            r.get("type").and_then(|t| t.as_str()) == Some("adapter_liveness_probed")
                && r.get("probe_result")
                    .and_then(|p| p.get("verdict"))
                    .and_then(|v| v.as_str())
                    == Some("retried")
        })
        .collect();
    assert_eq!(
        retried.len(),
        2,
        "two 5xx retries must emit two Retried events; got: {events}"
    );
    for row in &retried {
        assert_eq!(
            row.get("probe_result")
                .and_then(|p| p.get("reason"))
                .and_then(|s| s.as_str()),
            Some("server_error_5xx"),
            "the Retried reason must name the transient class: {row}"
        );
    }
}
