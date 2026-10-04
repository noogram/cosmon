// SPDX-License-Identifier: AGPL-3.0-only

//! Installed-harness witnesses for issue #168.
//!
//! These tests are ignored by the portable workspace gate because they require
//! independently installed CLI binaries. Run them explicitly on a harness host:
//!
//! ```text
//! cargo test -p cosmon-cli --test external_cli_gateway_loopback -- --ignored
//! ```
//!
//! Every endpoint is a loopback stub and every credential is fabricated.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MODEL: &str = "publisher/coder-model";
const KEY_ENV: &str = "COSMON_PERSON_GATEWAY_KEY";
const FAKE_KEY: &str = "fixture-key-168-not-secret";

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: Option<String>,
    body: String,
}

#[derive(Clone, Copy)]
enum Protocol {
    Responses,
    ChatCompletions,
}

struct Gateway {
    base_url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    _handle: std::thread::JoinHandle<()>,
}

impl Gateway {
    fn start(protocol: Protocol) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback gateway");
        let address = listener.local_addr().expect("gateway address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = Arc::clone(&seen);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = serve(stream, protocol, &seen_thread);
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            seen,
            _handle: handle,
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen lock").clone()
    }
}

fn serve(
    mut stream: TcpStream,
    protocol: Protocol,
    seen: &Mutex<Vec<Seen>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut content_length = 0usize;
    let mut authorization = None;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or_default();
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    seen.lock().expect("seen lock").push(Seen {
        path: request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned(),
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let payload = match protocol {
        Protocol::Responses => responses_stream(),
        Protocol::ChatCompletions => chat_stream(),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn responses_stream() -> String {
    let response = serde_json::json!({
        "id": "resp_loopback",
        "object": "response",
        "created_at": 1,
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "instructions": null,
        "max_output_tokens": null,
        "model": MODEL,
        "output": [{
            "id": "msg_loopback",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "loopback complete", "annotations": []}]
        }],
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": {"effort": null, "summary": null},
        "store": false,
        "temperature": null,
        "text": {"format": {"type": "text"}},
        "tool_choice": "auto",
        "tools": [],
        "top_p": null,
        "truncation": "disabled",
        "usage": {
            "input_tokens": 1,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 1,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 2
        },
        "metadata": {}
    });
    format!(
        "event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"type": "response.created", "response": response}),
        serde_json::json!({"type": "response.completed", "response": response})
    )
}

fn chat_stream() -> String {
    let first = serde_json::json!({
        "id": "chat_loopback",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": MODEL,
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "loopback complete"}, "finish_reason": null}]
    });
    let last = serde_json::json!({
        "id": "chat_loopback",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": MODEL,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    });
    format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n")
}

fn run_with_deadline(mut command: Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn installed harness");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().expect("poll harness").is_some() {
            return child.wait_with_output().expect("collect harness output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect timed-out harness");
            panic!(
                "installed harness timed out\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_request(gateway: &Gateway, path: &str) {
    let requests = gateway.requests();
    assert!(!requests.is_empty(), "the gateway must receive a request");
    for request in requests {
        assert_eq!(request.path, path);
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer fixture-key-168-not-secret")
        );
        let body: serde_json::Value = serde_json::from_str(&request.body).expect("request JSON");
        assert_eq!(body["model"], MODEL);
    }
}

fn assert_tree_excludes(root: &Path, needle: &str) {
    let mut stack = vec![root.to_owned()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                assert!(
                    !bytes
                        .windows(needle.len())
                        .any(|window| window == needle.as_bytes()),
                    "credential persisted in {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
#[ignore = "requires an installed Codex CLI; loopback only"]
fn codex_reaches_the_gateway_without_persisting_the_key() {
    let gateway = Gateway::start(Protocol::Responses);
    let root = tempfile::tempdir().expect("isolated root");
    let home = root.path().join("home");
    let codex_home = root.path().join("codex-home");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&codex_home).unwrap();

    let mut command = Command::new("codex");
    command
        .current_dir(root.path())
        .env("HOME", &home)
        .env("CODEX_HOME", &codex_home)
        .env(KEY_ENV, FAKE_KEY)
        .args([
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--skip-git-repo-check",
            "-c",
            "model_provider=cosmon_gateway",
            "-c",
            "model_providers.cosmon_gateway.name=cosmon-gateway",
            "-c",
            &format!(
                "model_providers.cosmon_gateway.base_url={}",
                gateway.base_url
            ),
            "-c",
            &format!("model_providers.cosmon_gateway.env_key={KEY_ENV}"),
            "-c",
            "model_providers.cosmon_gateway.wire_api=responses",
            "-c",
            "model_providers.cosmon_gateway.requires_openai_auth=false",
            "-c",
            "model_providers.cosmon_gateway.supports_websockets=false",
            "--model",
            MODEL,
            "Reply once without calling tools.",
        ]);
    let output = run_with_deadline(command);
    assert!(
        output.status.success(),
        "codex failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_request(&gateway, "/v1/responses");
    assert!(!output
        .stdout
        .windows(FAKE_KEY.len())
        .any(|w| w == FAKE_KEY.as_bytes()));
    assert!(!output
        .stderr
        .windows(FAKE_KEY.len())
        .any(|w| w == FAKE_KEY.as_bytes()));
    assert_tree_excludes(root.path(), FAKE_KEY);
}

#[test]
#[ignore = "requires an installed OpenCode CLI; loopback only"]
fn opencode_reaches_the_gateway_without_persisting_the_key() {
    let gateway = Gateway::start(Protocol::ChatCompletions);
    let root = tempfile::tempdir().expect("isolated root");
    let home = root.path().join("home");
    let config = root.path().join("config");
    let data = root.path().join("data");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();
    let models =
        std::collections::BTreeMap::from([(MODEL.to_owned(), serde_json::json!({"name": MODEL}))]);
    let inline = serde_json::json!({
        "provider": {
            "cosmon-gateway": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "cosmon gateway",
                "options": {"baseURL": gateway.base_url.clone(), "apiKey": format!("{{env:{KEY_ENV}}}")},
                "models": models
            }
        }
    })
    .to_string();

    let mut command = Command::new("opencode");
    command
        .current_dir(root.path())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("OPENCODE_CONFIG_CONTENT", inline)
        .env(KEY_ENV, FAKE_KEY)
        .args([
            "run",
            "--pure",
            "--format",
            "json",
            "--model",
            &format!("cosmon-gateway/{MODEL}"),
            "Reply once without calling tools.",
        ]);
    let output = run_with_deadline(command);
    assert!(
        output.status.success(),
        "opencode failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_request(&gateway, "/v1/chat/completions");
    assert!(!output
        .stdout
        .windows(FAKE_KEY.len())
        .any(|w| w == FAKE_KEY.as_bytes()));
    assert!(!output
        .stderr
        .windows(FAKE_KEY.len())
        .any(|w| w == FAKE_KEY.as_bytes()));
    assert_tree_excludes(root.path(), FAKE_KEY);
}
