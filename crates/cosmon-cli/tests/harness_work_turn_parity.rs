// SPDX-License-Identifier: AGPL-3.0-only

//! The messages direct arm delivers work-turn input and molecule context like
//! the chat-completions arm: the pending evidence is in its next native
//! request, the receipt follows the answered request, and the shell tool
//! reaches the worker's own molecule directory.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EVIDENCE: &str = "parity-evidence-7f3a: line 4 of the report is off by one";

fn cs(root: &Path, caller: Option<&Path>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cs"));
    command
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .args(args);
    match caller {
        Some(dir) => command.env("COSMON_MOL_DIR", dir),
        None => command.env_remove("COSMON_MOL_DIR"),
    };
    command.output().expect("cs")
}

fn cs_ok(root: &Path, caller: Option<&Path>, args: &[&str]) -> String {
    let out = cs(root, caller, args);
    assert!(
        out.status.success(),
        "cs {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}");
}

fn nucleate(root: &Path) -> (String, PathBuf) {
    let out = cs_ok(
        root,
        None,
        &["--json", "nucleate", "task-work", "--var", "topic=probe"],
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("nucleate json");
    let id = parsed["id"].as_str().expect("id").to_owned();
    let dir = root
        .join(".cosmon/state/fleets/default/molecules")
        .join(&id);
    (id, dir)
}

/// Read one HTTP request, returning its body.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut buf = [0; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let count = stream.read(&mut buf).expect("request header");
        assert!(count > 0, "request closed before headers");
        request.extend_from_slice(&buf[..count]);
    }
    let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length: ")?
                .trim()
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0);
    while request.len() - header_end < length {
        let count = stream.read(&mut buf).expect("request body");
        assert!(count > 0, "request closed before body");
        request.extend_from_slice(&buf[..count]);
    }
    String::from_utf8_lossy(&request[header_end..]).into_owned()
}

fn reply(mut stream: std::net::TcpStream, body: &serde_json::Value) {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("response");
}

#[test]
fn messages_arm_delivers_work_input_and_molecule_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    cs_ok(root, None, &["init", "--yes"]);
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@cosmon.test"]);
    git(root, &["config", "user.name", "cosmon-test"]);

    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let address = listener.local_addr().expect("address");
    let config = root.join(".cosmon/config.toml");
    let mut body = fs::read_to_string(&config).expect("config");
    body.push_str(&format!(
        "\n[adapters.anthropic]\nbase_url = \"http://{address}\"\ndefault_model = \"probe-model\"\n"
    ));
    fs::write(&config, body).expect("configured adapter");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);

    let (member, member_dir) = nucleate(root);
    let (owner, owner_dir) = nucleate(root);
    cs_ok(
        root,
        None,
        &[
            "work",
            "declare",
            &owner,
            "--seat",
            &format!("m={member}"),
            "--seat",
            &format!("o={owner}"),
        ],
    );
    cs_ok(
        root,
        Some(&owner_dir),
        &[
            "work", "send", "--to", "m", "--text", EVIDENCE, "--key", "ev-1",
        ],
    );

    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        let (mut first, _) = listener.accept().expect("first request");
        bodies.push(read_request(&mut first));
        reply(
            first,
            &serde_json::json!({
                "id":"m1","model":"probe-model","type":"message","role":"assistant",
                "content":[{"type":"tool_use","id":"call_1","name":"exec_command",
                    "input":{"command":"printf '%s' \"$COSMON_MOL_DIR\" > mol-dir-probe.txt"}}],
                "stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}
            }),
        );
        let (mut second, _) = listener.accept().expect("second request");
        bodies.push(read_request(&mut second));
        reply(
            second,
            &serde_json::json!({
                "id":"m2","model":"probe-model","type":"message","role":"assistant",
                "content":[{"type":"text","text":"done"}],
                "stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}
            }),
        );
        bodies
    });

    let tackle = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("ANTHROPIC_API_KEY", "probe-key")
        .args(["tackle", &member, "--adapter", "anthropic"])
        .output()
        .expect("tackle");
    let bodies = server.join().expect("provider server");
    let stderr = String::from_utf8_lossy(&tackle.stderr);

    assert!(
        bodies[0].contains(EVIDENCE),
        "first native request must carry the work envelope; stderr: {stderr}\n{}",
        bodies[0]
    );
    assert!(
        !bodies[1].contains(EVIDENCE),
        "peer evidence is request-only, not repeated on later turns"
    );

    let probe = root
        .join(".worktrees")
        .join(&member)
        .join("mol-dir-probe.txt");
    let seen = fs::read_to_string(&probe).unwrap_or_else(|e| panic!("probe {probe:?}: {e}"));
    assert_eq!(
        fs::canonicalize(&seen).expect("probe path exists"),
        fs::canonicalize(&member_dir).expect("member dir"),
        "the shell tool must see the worker's own molecule directory"
    );

    let listed = cs_ok(root, None, &["--json", "work", "list", &owner]);
    let listed: serde_json::Value = serde_json::from_str(&listed).expect("list json");
    let view = &listed["envelopes"]["ev-1"];
    assert_eq!(
        view["delivery_attempts"].as_array().map(Vec::len),
        Some(1),
        "one submitted delivery receipt after the answered request: {view}"
    );
}
