// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration tests for the OpenAI adapter's client-side
//! transient-429 back-off.
//!
//! Motivation: the `mistral-large-latest` key was measured on a
//! **4-requests-per-minute** tier. The
//! model is Claude-class on quality; the only wall is that billing ceiling,
//! which — before this change — surfaced a 429 the spine treated as fatal and
//! aborted a fast multi-turn agentic loop.
//!
//! These tests pin the two halves of the fix:
//!
//! 1. **Pacing, not fatal** — a transient 429 followed by a 200 must
//!    *succeed* after a bounded back-off, not abort.
//! 2. **Bounded** — a server that 429s forever must still surface
//!    [`OpenAiError::RateLimited`] after exactly `max_retries + 1` POSTs, so
//!    `one_turn` stays finite and the spine's termination proof holds.
//!
//! `Retry-After: 0` keeps the tests instant; the back-off *schedule* itself
//! is unit-tested in `src/openai/mod.rs::tests::backoff_delay_*`.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmon_provider::openai::{run_agent_loop, OpenAIProvider, OpenAiError, RetryPolicy};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Responds 429 for the first `fail_first` calls, then 200 with a
/// `finish_reason:"stop"` body. Counts every inbound POST so the test can
/// assert exactly how many round-trips the back-off loop performed.
struct PacingResponder {
    calls: Arc<Mutex<u32>>,
    fail_first: u32,
}

impl Respond for PacingResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut guard = self.calls.lock().expect("lock");
        *guard += 1;
        let nth = *guard;
        if nth <= self.fail_first {
            // `retry-after: 0` ⇒ the back-off helper sleeps for zero
            // duration, keeping the test instant while still exercising the
            // server-hint path.
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(json!({
                    "error": { "type": "rate_limit_exceeded", "message": "slow down" }
                }))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "paced and done" },
                    "finish_reason": "stop"
                }]
            }))
        }
    }
}

/// The win: two transient 429s then a 200 must yield a clean completion —
/// the loop is *paced*, not aborted. Exactly three POSTs: 429, 429, 200.
#[tokio::test]
async fn transient_429_then_200_succeeds_after_backoff() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(PacingResponder {
            calls: calls.clone(),
            fail_first: 2,
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    // Default policy already retries; pin a fast one so the test is robust
    // even if DEFAULT's backoff grows. `initial_backoff` is irrelevant here
    // (the server supplies `retry-after: 0`).
    let provider = OpenAIProvider::with_base_url("test-key", "mistral-large-latest", server.uri())
        .with_retry_policy(RetryPolicy {
            max_retries: 4,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        });

    let synthesis = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect("a paced 429 must NOT abort the loop");
    assert_eq!(synthesis, "paced and done");

    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 3,
        "expected 429, 429, then 200 — the loop must retry twice and then succeed"
    );
}

/// The bound: a server that 429s forever must surface `RateLimited` after
/// exactly `max_retries + 1` POSTs — never an unbounded hammer. This is the
/// finiteness witness the spine's `O(K)` termination proof relies on.
#[tokio::test]
async fn sustained_429_surfaces_rate_limited_after_bounded_retries() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(PacingResponder {
            calls: calls.clone(),
            fail_first: u32::MAX, // never recovers
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "mistral-large-latest", server.uri())
        .with_retry_policy(RetryPolicy {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        });

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("a sustained 429 must eventually surface as a typed error");
    assert!(
        matches!(err, OpenAiError::RateLimited { .. }),
        "exhausted retries must surface RateLimited, got: {err:?}"
    );

    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 4,
        "first attempt + 3 retries = 4 POSTs before surfacing the failure"
    );
}

/// Regression guard: `RetryPolicy::DISABLED` restores the legacy
/// one-POST-one-error behaviour, so a caller that delegates pacing to an
/// external scheduler is never silently retried.
#[tokio::test]
async fn disabled_policy_does_not_retry() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(PacingResponder {
            calls: calls.clone(),
            fail_first: u32::MAX,
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-4o-mini", server.uri())
        .with_retry_policy(RetryPolicy::DISABLED);

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("429 must surface immediately under DISABLED");
    assert!(matches!(err, OpenAiError::RateLimited { .. }));

    let n = *calls.lock().expect("lock");
    assert_eq!(n, 1, "DISABLED policy must perform exactly one POST");
}
