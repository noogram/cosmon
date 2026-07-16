// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration test for the Anthropic Direct-API
//! adapter's quota-vs-rate-limit classifier.
//!
//! Mirrors `openai_quota_mock.rs`. Anthropic surfaces billing failures
//! as HTTP 400 with `invalid_request_error` and a message about the
//! credit balance, so the classifier must read the message text — the
//! type field alone is not enough.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};

use cosmon_provider::anthropic::{run_agent_loop, AnthropicError, AnthropicProvider};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct CountingQuotaResponder {
    calls: Arc<Mutex<u32>>,
}

impl Respond for CountingQuotaResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        *self.calls.lock().expect("lock") += 1;
        ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": "Your credit balance is too low to access the Claude API. Please go to Plans & Billing to upgrade or purchase credits."
            }
        }))
    }
}

#[tokio::test]
async fn anthropic_400_credit_balance_propagates_without_retry() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    let responder = CountingQuotaResponder {
        calls: calls.clone(),
    };

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = AnthropicProvider::with_base_url("test-key", "claude-opus-4-7", server.uri());

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("400 must surface an error");

    match err {
        AnthropicError::QuotaExceeded { message } => {
            assert!(
                message.to_lowercase().contains("credit balance"),
                "vendor message must round-trip into the typed variant; got: {message}"
            );
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }

    let n = *calls.lock().expect("lock");
    assert_eq!(
        n, 1,
        "QuotaExceeded must propagate on the first response — no retry"
    );
}

#[tokio::test]
async fn anthropic_429_rate_limit_is_transient_not_quota() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "20")
                .set_body_json(json!({
                    "type": "error",
                    "error": {
                        "type": "rate_limit_error",
                        "message": "Number of requests has exceeded your rate limit."
                    }
                })),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = AnthropicProvider::with_base_url("test-key", "claude-opus-4-7", server.uri());

    let err = run_agent_loop(&provider, "Briefing.", dir.path(), None)
        .await
        .expect_err("429 must surface an error");

    match err {
        AnthropicError::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(20)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}
