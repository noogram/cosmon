// SPDX-License-Identifier: AGPL-3.0-only

//! The direct wire arms report the token counters each answered request
//! carried, typed, at the response boundary.
//!
//! Before this unit both arms decoded the response and dropped `usage`, so a
//! worker's tokens existed only in the provider's own logs. These tests pin
//! what reaches a usage sink: known categories as measured values, absent ones
//! as not reported (never zero), unrepresentable ones as malformed, and
//! requested and served model as two separate facts.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmon_agent_harness::spine::{ReportedCount, UsageSample, UsageSink};
use cosmon_provider::anthropic::{
    run_agent_loop_counted as run_messages_loop_counted, AnthropicProvider,
};
use cosmon_provider::openai::{run_agent_loop_counted, OpenAIProvider, RetryPolicy};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Default)]
struct Collect(Mutex<Vec<UsageSample>>);

impl Collect {
    fn samples(&self) -> Vec<UsageSample> {
        self.0.lock().expect("sample lock").clone()
    }
}

impl UsageSink for Collect {
    fn record(&self, sample: UsageSample) {
        self.0.lock().expect("sample lock").push(sample);
    }
}

fn sse(frames: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for frame in frames {
        body.push_str(&format!("data: {frame}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn stream_template(frames: &[serde_json::Value]) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(frames))
}

/// A failing first POST, then a streamed answer carrying a usage frame.
struct FailOnceThenAnswer {
    calls: Arc<Mutex<u32>>,
    answer: ResponseTemplate,
}

impl Respond for FailOnceThenAnswer {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut calls = self.calls.lock().expect("call counter");
        *calls += 1;
        if *calls == 1 {
            ResponseTemplate::new(500).set_body_string("transient failure")
        } else {
            self.answer.clone()
        }
    }
}

fn one_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 2,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
    }
}

#[tokio::test]
async fn chat_stream_reports_known_categories_and_ignores_the_failed_attempt() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(FailOnceThenAnswer {
            calls: Arc::clone(&calls),
            answer: stream_template(&[
                json!({"id":"chatcmpl-1","model":"served-model-x","choices":[{"delta":{"content":"done"}}]}),
                json!({"id":"chatcmpl-1","model":"served-model-x","choices":[{"delta":{},"finish_reason":"stop"}]}),
                json!({"id":"chatcmpl-1","model":"served-model-x","choices":[],"usage":{
                    "prompt_tokens":120,"completion_tokens":40,
                    "prompt_tokens_details":{"cached_tokens":100},
                    "completion_tokens_details":{"reasoning_tokens":25}}}),
            ]),
        })
        .mount(&server)
        .await;

    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "requested-model", server.uri())
        .with_retry_policy(one_retry())
        .with_usage_sink(Some(sink.clone()));
    run_agent_loop_counted(&provider, "Do the task.", dir.path(), None)
        .await
        .expect("the retried request is answered");

    assert_eq!(
        *calls.lock().expect("call counter"),
        2,
        "one retry happened"
    );
    let samples = sink.samples();
    assert_eq!(
        samples.len(),
        1,
        "the 500 never produced a body, so it produced no sample"
    );
    let sample = &samples[0];
    assert_eq!(sample.response_id.as_deref(), Some("chatcmpl-1"));
    assert_eq!(sample.provider, "openai");
    assert_eq!(sample.usage.input_tokens, ReportedCount::Measured(120));
    assert_eq!(
        sample.usage.cached_input_tokens,
        ReportedCount::Measured(100)
    );
    assert_eq!(sample.usage.output_tokens, ReportedCount::Measured(40));
    assert_eq!(
        sample.usage.reasoning_output_tokens,
        ReportedCount::Measured(25)
    );
    assert_eq!(
        sample.usage.cache_write_tokens,
        ReportedCount::NotReported,
        "the chat wire has no cache-write counter"
    );
}

#[tokio::test]
async fn requested_and_served_model_stay_separate() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(stream_template(&[
            json!({"id":"r1","model":"served-model-x","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
            json!({"id":"r1","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1}}),
        ]))
        .mount(&server)
        .await;
    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "requested-model", server.uri())
        .with_usage_sink(Some(sink.clone()));
    run_agent_loop_counted(&provider, "task", dir.path(), None)
        .await
        .expect("answered");
    let sample = &sink.samples()[0];
    assert_eq!(sample.requested_model, "requested-model");
    assert_eq!(sample.served_model.as_deref(), Some("served-model-x"));
}

#[tokio::test]
async fn a_response_without_usage_is_not_reported_never_zero() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(stream_template(&[
            json!({"id":"r2","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ]))
        .mount(&server)
        .await;
    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "m", server.uri())
        .with_usage_sink(Some(sink.clone()));
    run_agent_loop_counted(&provider, "task", dir.path(), None)
        .await
        .expect("answered");
    let sample = &sink.samples()[0];
    assert_eq!(sample.usage.input_tokens, ReportedCount::NotReported);
    assert_eq!(sample.usage.output_tokens, ReportedCount::NotReported);
    assert_eq!(sample.served_model, None);
}

#[tokio::test]
async fn unrepresentable_values_are_malformed_and_do_not_fail_the_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"r3","model":"m",
            "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":-3,"completion_tokens":1.5,
                     "prompt_tokens_details":{"cached_tokens":"many"}}
        })))
        .mount(&server)
        .await;
    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "m", server.uri())
        .with_usage_sink(Some(sink.clone()));
    let outcome = run_agent_loop_counted(&provider, "task", dir.path(), None)
        .await
        .expect("a malformed usage block does not abort the turn");
    assert_eq!(outcome.synthesis, "ok");
    let usage = sink.samples()[0].usage;
    assert_eq!(usage.input_tokens, ReportedCount::Malformed);
    assert_eq!(usage.output_tokens, ReportedCount::Malformed);
    assert_eq!(usage.cached_input_tokens, ReportedCount::Malformed);
}

#[tokio::test]
async fn an_output_limited_answer_keeps_the_usage_it_consumed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(stream_template(&[
            json!({"id":"r4","choices":[{"delta":{"content":"cut off"},"finish_reason":"length"}]}),
            json!({"id":"r4","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":512}}),
        ]))
        .mount(&server)
        .await;
    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "m", server.uri())
        .with_usage_sink(Some(sink.clone()));
    run_agent_loop_counted(&provider, "task", dir.path(), None)
        .await
        .expect("a limited answer is a typed outcome");
    let usage = sink.samples()[0].usage;
    assert_eq!(usage.output_tokens, ReportedCount::Measured(512));
}

#[tokio::test]
async fn usage_is_requested_only_when_a_sink_is_attached() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(stream_template(&[
            json!({"id":"r5","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ]))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().expect("tempdir");

    let plain = OpenAIProvider::with_base_url("test-key", "m", server.uri());
    run_agent_loop_counted(&plain, "task", dir.path(), None)
        .await
        .expect("answered");
    let with_sink = OpenAIProvider::with_base_url("test-key", "m", server.uri())
        .with_usage_sink(Some(Arc::new(Collect::default())));
    run_agent_loop_counted(&with_sink, "task", dir.path(), None)
        .await
        .expect("answered");

    let bodies: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .expect("recorded requests")
        .iter()
        .map(|r| serde_json::from_slice(&r.body).expect("json body"))
        .collect();
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies[0].get("stream_options").is_none(),
        "the default wire body is unchanged"
    );
    assert_eq!(bodies[1]["stream_options"]["include_usage"], json!(true));
}

#[tokio::test]
async fn messages_input_total_includes_cache_reads_and_writes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_1","model":"served-claude","type":"message","role":"assistant",
            "content":[{"type":"text","text":"done"}],
            "stop_reason":"end_turn",
            "usage":{"input_tokens":10,"cache_read_input_tokens":5,
                     "cache_creation_input_tokens":3,"output_tokens":7}
        })))
        .mount(&server)
        .await;
    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = AnthropicProvider::with_base_url("test-key", "requested-claude", server.uri())
        .with_usage_sink(Some(sink.clone()));
    run_messages_loop_counted(&provider, "task", dir.path(), None)
        .await
        .expect("answered");
    let sample = &sink.samples()[0];
    assert_eq!(sample.provider, "anthropic");
    assert_eq!(sample.response_id.as_deref(), Some("msg_1"));
    assert_eq!(sample.requested_model, "requested-claude");
    assert_eq!(sample.served_model.as_deref(), Some("served-claude"));
    assert_eq!(
        sample.usage.input_tokens,
        ReportedCount::Measured(18),
        "the wire excludes cache traffic; the canonical total includes it"
    );
    assert_eq!(sample.usage.cached_input_tokens, ReportedCount::Measured(5));
    assert_eq!(sample.usage.cache_write_tokens, ReportedCount::Measured(3));
    assert_eq!(sample.usage.output_tokens, ReportedCount::Measured(7));
}

#[tokio::test]
async fn messages_limited_answer_without_usage_reports_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content":[{"type":"text","text":"partial"}],
            "stop_reason":"max_tokens"
        })))
        .mount(&server)
        .await;
    let sink = Arc::new(Collect::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = AnthropicProvider::with_base_url("test-key", "m", server.uri())
        .with_usage_sink(Some(sink.clone()));
    run_messages_loop_counted(&provider, "task", dir.path(), None)
        .await
        .expect("a limited answer is a typed outcome");
    let samples = sink.samples();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].usage.input_tokens, ReportedCount::NotReported);
    assert_eq!(samples[0].response_id, None);
}
