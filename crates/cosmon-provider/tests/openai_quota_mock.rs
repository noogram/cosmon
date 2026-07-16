// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration test for the OpenAI Direct-API adapter's
//! quota-vs-rate-limit classifier.
//!
//! Scenario: a Moonshot-style endpoint suspends the account and
//! responds to every `chat/completions` POST with HTTP 402 and
//! envelope `{"error":{"type":"exceeded_current_quota_error",
//! "message":"… insufficient balance …"}}`. The adapter must:
//!
//! - propagate the failure as the typed [`OpenAiError::QuotaExceeded`]
//!   variant — never `RateLimited` (transient) nor `Http`
//!   (stringly-typed);
//! - NOT loop / retry on this variant — the spine must surface the
//!   failure on the first response, not after exhausting the turn
//!   budget;
//! - preserve the vendor's `message` text for the operator to read.
//!
//! No `OPENAI_API_KEY` required — the wiremock server is the entire
//! upstream. Runs on every `cargo test --workspace`.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};

use cosmon_provider::openai::{run_agent_loop, OpenAIProvider, OpenAiError};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Counts every inbound POST so the test can assert the spine does
/// NOT retry on QuotaExceeded (would normally hammer until
/// `TurnBudgetExhausted`).
struct CountingQuotaResponder {
    calls: Arc<Mutex<u32>>,
}

impl Respond for CountingQuotaResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        *self.calls.lock().expect("lock") += 1;
        ResponseTemplate::new(402).set_body_json(json!({
            "error": {
                "type": "exceeded_current_quota_error",
                "message": "Your account has been suspended due to insufficient balance, please recharge."
            }
        }))
    }
}

#[tokio::test]
async fn openai_402_quota_propagates_without_retry() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    let responder = CountingQuotaResponder {
        calls: calls.clone(),
    };

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "kimi-k2", server.uri());

    let result = run_agent_loop(
        &provider,
        "Write a short haiku and save it to haiku.md.",
        dir.path(),
        None,
    )
    .await;

    let err = result.expect_err("must surface a typed error, not Ok");
    match err {
        OpenAiError::QuotaExceeded { message } => {
            assert!(
                message.to_lowercase().contains("insufficient balance"),
                "vendor message must round-trip into the typed variant; got: {message}"
            );
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }

    // Load-bearing assertion: the spine must NOT retry on QuotaExceeded.
    // A single round-trip ⇒ a single inbound POST. Anything > 1 would
    // mean the loop or a downstream retry policy interpreted the
    // permanent failure as transient.
    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 1,
        "QuotaExceeded must propagate on the first response — no retry"
    );
}

/// Counterpoint: a true transient 429 with `rate_limit_exceeded` body
/// — the adapter must surface [`OpenAiError::RateLimited`], not
/// QuotaExceeded.
///
/// Retry is **disabled** ([`RetryPolicy::DISABLED`]) so this test pins
/// the *classifier* in isolation: one POST in → one `RateLimited` out,
/// with the `Retry-After` header preserved. The pacing behaviour the
/// default policy adds is covered separately in
/// `tests/openai_rate_limit_retry.rs`.
#[tokio::test]
async fn openai_429_rate_limit_is_transient_not_quota() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "12")
                .set_body_json(json!({
                    "error": {
                        "type": "rate_limit_exceeded",
                        "message": "Please slow down."
                    }
                })),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-4o-mini", server.uri())
        .with_retry_policy(cosmon_provider::openai::RetryPolicy::DISABLED);

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("429 must surface an error");

    match err {
        OpenAiError::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(12)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}
