// SPDX-License-Identifier: AGPL-3.0-only

//! A synchronous agent-loop failure after a tool call must preserve its work.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

fn cs(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args(args)
        .output()
        .expect("cs")
}

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git")
}

fn respond(mut stream: std::net::TcpStream, status: &str, content_type: &str, body: &str) {
    let mut request = Vec::new();
    let mut buf = [0; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let count = stream.read(&mut buf).expect("request header");
        assert!(count > 0, "request closed before headers");
        request.extend_from_slice(&buf[..count]);
    }
    let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while request.len() - header_end < content_length {
        let count = stream.read(&mut buf).expect("request body");
        assert!(count > 0, "request closed before body");
        request.extend_from_slice(&buf[..count]);
    }
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("response");
}

fn run_case(adapter: &'static str, tool_first: bool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let init = cs(root, &["init", "--yes"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(git(root, &["init", "-q"]).status.success());
    assert!(git(root, &["config", "user.email", "test@cosmon.test"])
        .status
        .success());
    assert!(git(root, &["config", "user.name", "cosmon-test"])
        .status
        .success());

    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let address = listener.local_addr().expect("address");
    let config = root.join(".cosmon/config.toml");
    let mut body = fs::read_to_string(&config).expect("config");
    body.push_str(&format!(
        "\n[adapters.{adapter}]\nbase_url = \"http://{address}\"\ndefault_model = \"probe-model\"\n"
    ));
    fs::write(&config, body).expect("configured adapter");
    assert!(git(root, &["add", "-A"]).status.success());
    assert!(git(root, &["commit", "-q", "-m", "init"]).status.success());

    let nucleate = cs(
        root,
        &["--json", "nucleate", "task-work", "--var", "topic=probe"],
    );
    assert!(
        nucleate.status.success(),
        "{}",
        String::from_utf8_lossy(&nucleate.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&nucleate.stdout).expect("nucleate json");
    let id = parsed["id"].as_str().expect("molecule id");
    let worktree = root.join(".worktrees").join(id);
    let branch = format!("feat/{id}");

    let server = std::thread::spawn(move || {
        if tool_first {
            let (stream, _) = listener.accept().expect("first request");
            let args =
                serde_json::json!({"path":"probe-work.txt","content":"work the loop produced\n"});
            if adapter == "openai" {
                let chunk = serde_json::json!({"id":"c1","model":"probe-model","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"write_file","arguments":args.to_string()}}]},"finish_reason":null}]});
                let sse = format!("data: {chunk}\n\ndata: {{\"id\":\"c1\",\"model\":\"probe-model\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n");
                respond(stream, "200 OK", "text/event-stream", &sse);
            } else {
                let reply = serde_json::json!({"id":"c1","model":"probe-model","type":"message","role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"write_file","input":args}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}});
                respond(stream, "200 OK", "application/json", &reply.to_string());
            }
        }
        let (stream, _) = listener.accept().expect("failing request");
        respond(
            stream,
            "400 Bad Request",
            "application/json",
            "{\"error\":{\"message\":\"probe failure\"}}",
        );
    });

    let tackle = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("OPENAI_API_KEY", "probe-key")
        .env("ANTHROPIC_API_KEY", "probe-key")
        .args(["tackle", id, "--adapter", adapter])
        .output()
        .expect("tackle");
    server.join().expect("provider server");
    assert!(!tackle.status.success(), "provider error must fail tackle");
    let state_path = root
        .join(".cosmon/state/fleets/default/molecules")
        .join(id)
        .join("state.json");
    let molecule_dir = state_path.parent().expect("molecule directory");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).expect("state")).expect("state json");
    let branches = git(root, &["branch", "--list", &branch]);
    assert!(branches.status.success());
    let branch_exists = !branches.stdout.is_empty();
    if tool_first {
        assert_eq!(
            fs::read_to_string(worktree.join("probe-work.txt")).expect("written file"),
            "work the loop produced\n"
        );
        assert!(branch_exists, "branch with partial work must survive");
        assert_eq!(state["status"], "collapsed");
        assert_eq!(state["collapse_reason_kind"], "agent_loop_failed");
        assert!(state["collapse_reason"]
            .as_str()
            .unwrap_or("")
            .contains("agent loop"));
        let synthesis =
            fs::read_to_string(molecule_dir.join("synthesis.md")).expect("partial synthesis");
        assert!(synthesis.contains("\"bytes_written\":23"), "{synthesis}");
        let events = fs::read_to_string(root.join(".cosmon/state/events.jsonl")).expect("events");
        assert!(events.contains("molecule_collapsed"), "{events}");
        assert!(events.contains("agent_loop_failed"), "{events}");
        assert!(!events.contains("worker_spawn_rolled_back"), "{events}");
    } else {
        assert!(
            !worktree.exists(),
            "pre-work failure must clean up worktree"
        );
        assert!(!branch_exists, "pre-work failure must clean up branch");
        assert_eq!(state["status"], "pending");
    }
}

#[test]
fn provider_error_after_write_preserves_work_and_collapses() {
    run_case("openai", true);
    run_case("anthropic", true);
}

#[test]
fn provider_error_before_work_rolls_back() {
    run_case("openai", false);
    run_case("anthropic", false);
}
