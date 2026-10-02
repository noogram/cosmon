// SPDX-License-Identifier: AGPL-3.0-only

//! Provider termination is authoritative over partially assembled tool calls.

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};

use cosmon_agent_harness::TerminalDisposition;
use cosmon_provider::anthropic::{
    run_agent_loop_counted as run_messages_loop_counted, AnthropicProvider,
};
use cosmon_provider::openai::{run_agent_loop_counted, OpenAIProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct LimitedThenStop {
    requests: Arc<Mutex<u32>>,
}

impl Respond for LimitedThenStop {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut requests = self.requests.lock().expect("request counter lock");
        *requests += 1;
        if *requests == 1 {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\",\"tool_calls\":[{\"index\":0,\"id\":\"call-limited\",\"type\":\"function\",\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"must-not-exist.txt\\\",\\\"content\\\":\\\"unsafe\\\"}\"}}]}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
                    "data: [DONE]\n\n",
                ))
        } else {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"wrong second turn\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n",
                ))
        }
    }
}

#[tokio::test]
async fn output_limit_dominates_a_complete_tool_envelope() {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(LimitedThenStop {
            requests: Arc::clone(&requests),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "test-model", server.uri());
    let outcome = run_agent_loop_counted(&provider, "Do the task.", dir.path(), None)
        .await
        .expect("a limited response is a typed terminal outcome");

    assert_eq!(outcome.synthesis, "partial answer");
    assert_eq!(
        outcome.terminal_disposition,
        TerminalDisposition::OutputLimit
    );
    assert_eq!(outcome.tools_dispatched, 0);
    assert!(!dir.path().join("must-not-exist.txt").exists());
    assert_eq!(*requests.lock().expect("request counter lock"), 1);
}

#[tokio::test]
async fn malformed_only_stream_is_incomplete_not_normal() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {not-json}\n\ndata: [DONE]\n\n"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "test-model", server.uri());
    let outcome = run_agent_loop_counted(&provider, "Do the task.", dir.path(), None)
        .await
        .expect("wire incompleteness is retained as a typed terminal outcome");

    assert!(outcome.synthesis.is_empty());
    assert_eq!(
        outcome.terminal_disposition,
        TerminalDisposition::Incomplete
    );
    assert_eq!(outcome.tools_dispatched, 0);
}

#[tokio::test]
async fn messages_output_limit_dominates_a_tool_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [
                {"type": "text", "text": "partial messages answer"},
                {
                    "type": "tool_use",
                    "id": "call-limited",
                    "name": "write_file",
                    "input": {"path": "must-not-exist.txt", "content": "unsafe"}
                }
            ],
            "stop_reason": "max_tokens"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = AnthropicProvider::with_base_url("test-key", "test-model", server.uri());
    let outcome = run_messages_loop_counted(&provider, "Do the task.", dir.path(), None)
        .await
        .expect("a limited response is a typed terminal outcome");

    assert_eq!(outcome.synthesis, "partial messages answer");
    assert_eq!(
        outcome.terminal_disposition,
        TerminalDisposition::OutputLimit
    );
    assert_eq!(outcome.tools_dispatched, 0);
    assert!(!dir.path().join("must-not-exist.txt").exists());
}
