// SPDX-License-Identifier: AGPL-3.0-only

//! Mock-server integration tests for the OpenAI adapter's **own-side
//! streaming tool-call extraction** — the mode-C structural fix
//! (delib-20260707-df9b M2).
//!
//! Motivation: mode C runs [`OpenAIProvider`] in-process against ollama's
//! `/v1/chat/completions`. With `stream:false` ollama parses the model's
//! emitted tool call **server-side** and answers HTTP 500 `error parsing tool
//! call` when a long / malformed argument trips its parser — the spine
//! treated that as fatal and the worker died (task-20260707-c253). The D-A A/B
//! measurement showed `stream:true` returns HTTP 200 and streams the raw
//! `arguments` text without a server-side parse. This adapter now requests
//! `stream:true` and accumulates the fragments **itself**, so:
//!
//! 1. **A streamed tool call is assembled own-side and dispatched** — the
//!    fragments arrive across several SSE frames and the harness executes the
//!    reconstructed call against `work_dir`.
//! 2. **A malformed streamed argument becomes a recoverable `tool_result`**,
//!    not a dead worker: the raw bytes reach `dispatch_tool_calls`, the
//!    registry rejects them, and the failure is fed back to the model, which
//!    self-corrects on the next streamed turn.

#![cfg(feature = "http")]

use cosmon_provider::openai::{run_agent_loop, OpenAIProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Build one OpenAI-style SSE body from a list of `data:` JSON payloads,
/// terminated by `data: [DONE]`. Each frame is separated by a blank line, the
/// standard event-stream framing.
fn sse_body(frames: &[&str]) -> String {
    let mut s = String::new();
    for f in frames {
        s.push_str("data: ");
        s.push_str(f);
        s.push_str("\n\n");
    }
    s.push_str("data: [DONE]\n\n");
    s
}

fn sse_response(frames: &[&str]) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse_body(frames))
}

/// Turn 1 streams a `read_file` tool call whose `arguments` are split across
/// three frames; turn 2 streams a final text answer. The win: the adapter
/// assembles the fragmented call own-side, the harness runs it, and the file
/// contents round-trip back to the model.
struct StreamingTwoTurn {
    calls: std::sync::Arc<std::sync::Mutex<u32>>,
}

impl Respond for StreamingTwoTurn {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut guard = self.calls.lock().expect("lock");
        *guard += 1;
        if *guard == 1 {
            // A tool call streamed as fragments: id+name first, then the
            // arguments JSON in two slices, then a finish frame.
            sse_response(&[
                r#"{"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"no"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"te.txt\"}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            ])
        } else {
            sse_response(&[
                r#"{"choices":[{"delta":{"role":"assistant","content":"the note says hi"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ])
        }
    }
}

#[tokio::test]
async fn streamed_tool_call_is_assembled_and_dispatched() {
    let server = MockServer::start().await;
    let calls = std::sync::Arc::new(std::sync::Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StreamingTwoTurn {
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("note.txt"), "hi from the note").expect("write note");

    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri());
    let synthesis = run_agent_loop(&provider, "Read note.txt.", dir.path(), None)
        .await
        .expect("a streamed tool call must be assembled and dispatched, not error");

    assert_eq!(synthesis, "the note says hi");
    assert_eq!(*calls.lock().expect("lock"), 2, "one tool turn + one stop");
}

/// A **malformed** streamed argument (a truncated JSON) must NOT kill the
/// worker: the raw bytes reach the harness dispatch, the registry rejects
/// them, and the model — seeing the error in the next turn — corrects and
/// completes. This is the mode-C survival property, own-side.
struct StreamingMalformedThenRecover {
    calls: std::sync::Arc<std::sync::Mutex<u32>>,
}

impl Respond for StreamingMalformedThenRecover {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut guard = self.calls.lock().expect("lock");
        *guard += 1;
        if *guard == 1 {
            // Truncated arguments — valid JSON never closes. With the old
            // server-side parse this shape produced the HTTP 500; streamed, it
            // arrives as raw bytes we hand to dispatch.
            sse_response(&[
                r#"{"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call-bad","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"no"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            ])
        } else {
            // The model saw the tool error and gives up gracefully with text.
            sse_response(&[
                r#"{"choices":[{"delta":{"content":"could not read; done"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ])
        }
    }
}

#[tokio::test]
async fn malformed_streamed_arg_recovers_via_tool_result_not_death() {
    let server = MockServer::start().await;
    let calls = std::sync::Arc::new(std::sync::Mutex::new(0_u32));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(StreamingMalformedThenRecover {
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = OpenAIProvider::with_base_url("test-key", "gpt-oss:120b", server.uri());

    // The loop must SURVIVE the malformed call and reach the model's final
    // text — the mode-C death this whole change exists to prevent.
    let synthesis = run_agent_loop(&provider, "Read note.txt.", dir.path(), None)
        .await
        .expect("a malformed streamed arg must recover, not kill the worker");
    assert_eq!(synthesis, "could not read; done");
    assert_eq!(
        *calls.lock().expect("lock"),
        2,
        "turn 1 = malformed tool call fed back as an error; turn 2 = recovery"
    );
}
