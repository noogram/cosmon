// SPDX-License-Identifier: AGPL-3.0-only

//! The configured output bound reaches the wire, and the final serialized
//! request is checked against the input ceiling before any network I/O.
//!
//! The guard measures the body that is about to be sent, so a large tool
//! argument appended to the log by an earlier turn counts, which the
//! log-only estimate used by the spine does not cover.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};

use cosmon_provider::anthropic::{self, AnthropicError, AnthropicProvider};
use cosmon_provider::openai::{self, OpenAIProvider, OpenAiError};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Records each request body; replies with `replies[n]` (last one repeats).
struct Recorder {
    bodies: Arc<Mutex<Vec<Value>>>,
    replies: Vec<Value>,
}

impl Respond for Recorder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut bodies = self.bodies.lock().expect("lock");
        bodies.push(serde_json::from_slice(&request.body).expect("json body"));
        let n = (bodies.len() - 1).min(self.replies.len() - 1);
        ResponseTemplate::new(200).set_body_json(self.replies[n].clone())
    }
}

fn openai_stop() -> Value {
    json!({"choices": [{"message": {"role": "assistant", "content": "done"},
                        "finish_reason": "stop"}]})
}

/// A turn whose tool call carries `bytes` bytes of argument text.
fn openai_big_tool_call(bytes: usize) -> Value {
    let args = json!({ "path": "out.txt", "content": "y".repeat(bytes) }).to_string();
    json!({"choices": [{"message": {"role": "assistant", "content": null,
        "tool_calls": [{"id": "call-1", "type": "function",
            "function": {"name": "read_file", "arguments": args}}]},
        "finish_reason": "tool_calls"}]})
}

async fn openai_server(replies: Vec<Value>) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let server = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Recorder {
            bodies: bodies.clone(),
            replies,
        })
        .mount(&server)
        .await;
    (server, bodies)
}

/// The configured output bound is sent as `max_tokens`; unset sends none.
#[tokio::test]
async fn openai_sends_configured_output_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (server, bodies) = openai_server(vec![openai_stop()]).await;
    let bounded =
        OpenAIProvider::with_base_url("k", "m", server.uri()).with_request_budget(None, Some(777));
    openai::run_agent_loop(&bounded, "hi", dir.path(), None)
        .await
        .expect("loop");
    let unbounded = OpenAIProvider::with_base_url("k", "m", server.uri());
    openai::run_agent_loop(&unbounded, "hi", dir.path(), None)
        .await
        .expect("loop");
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["max_tokens"], json!(777));
    assert!(bodies[1].get("max_tokens").is_none());
}

/// A briefing that fits the log estimate but whose full body (tool schemas
/// included) is over the ceiling is refused with zero HTTP requests.
#[tokio::test]
async fn openai_refuses_oversized_first_request_before_network() {
    let dir = tempfile::tempdir().unwrap();
    let (server, bodies) = openai_server(vec![openai_stop()]).await;
    let p =
        OpenAIProvider::with_base_url("k", "m", server.uri()).with_request_budget(Some(300), None);
    let err = openai::run_agent_loop(&p, &"z".repeat(4_000), dir.path(), None)
        .await
        .expect_err("over the ceiling");
    assert!(
        matches!(err, OpenAiError::ContextOverflow { limit: 300, .. }),
        "{err:?}"
    );
    assert_eq!(
        bodies.lock().unwrap().len(),
        0,
        "no HTTP request may be sent"
    );
}

/// A large tool argument appended by turn 1 pushes turn 2 over the ceiling:
/// exactly one request is sent, the second is refused before the network.
#[tokio::test]
async fn openai_refuses_request_grown_by_large_tool_argument() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("out.txt"), "x").unwrap();
    let (server, bodies) = openai_server(vec![openai_big_tool_call(40_000), openai_stop()]).await;
    // Turn 1 (tool schemas + short briefing) fits; turn 2 carries 40 kB of
    // argument text (~10 000 estimated tokens) and must not.
    let p = OpenAIProvider::with_base_url("k", "m", server.uri())
        .with_request_budget(Some(6_000), None);
    let err = openai::run_agent_loop(&p, "hi", dir.path(), None)
        .await
        .expect_err("second request over the ceiling");
    assert!(
        matches!(err, OpenAiError::ContextOverflow { limit: 6_000, .. }),
        "{err:?}"
    );
    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "second request must not be sent"
    );
}

fn anthropic_stop() -> Value {
    json!({"id": "m", "type": "message", "role": "assistant", "model": "m",
           "content": [{"type": "text", "text": "done"}], "stop_reason": "end_turn",
           "usage": {"input_tokens": 1, "output_tokens": 1}})
}

async fn anthropic_server(reply: Value) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let server = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Recorder {
            bodies: bodies.clone(),
            replies: vec![reply],
        })
        .mount(&server)
        .await;
    (server, bodies)
}

/// The configured output bound replaces the default `max_tokens`.
#[tokio::test]
async fn anthropic_sends_configured_output_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (server, bodies) = anthropic_server(anthropic_stop()).await;
    let p = AnthropicProvider::with_base_url("k", "m", server.uri())
        .with_request_budget(None, Some(555));
    anthropic::run_agent_loop(&p, "hi", dir.path(), None)
        .await
        .expect("loop");
    assert_eq!(bodies.lock().unwrap()[0]["max_tokens"], json!(555));
}

/// An oversized request is refused before any HTTP request is sent.
#[tokio::test]
async fn anthropic_refuses_oversized_request_before_network() {
    let dir = tempfile::tempdir().unwrap();
    let (server, bodies) = anthropic_server(anthropic_stop()).await;
    let p = AnthropicProvider::with_base_url("k", "m", server.uri())
        .with_request_budget(Some(300), None);
    let err = anthropic::run_agent_loop(&p, &"z".repeat(4_000), dir.path(), None)
        .await
        .expect_err("over the ceiling");
    assert!(
        matches!(err, AnthropicError::ContextOverflow { limit: 300, .. }),
        "{err:?}"
    );
    assert_eq!(bodies.lock().unwrap().len(), 0);
}
