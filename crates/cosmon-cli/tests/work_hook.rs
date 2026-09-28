// SPDX-License-Identifier: AGPL-3.0-only

//! Binary-level examples of work delivery at a provider tool boundary.

use std::{fs, path::Path, process::Command};

const OWNER: &str = "task-20260928-a001";
const A: &str = "task-20260928-a002";
const B: &str = "task-20260928-a003";

fn mol(state: &Path, id: &str) -> std::path::PathBuf {
    state.join("fleets/default/molecules").join(id)
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp galaxy");
    let state = dir.path().join(".cosmon/state");
    for id in [OWNER, A, B] {
        fs::create_dir_all(mol(&state, id)).expect("molecule dir");
        fs::write(mol(&state, id).join("state.json"), b"state\n").expect("state");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(dir.path())
        .args([
            "--config",
            state.to_str().expect("state"),
            "work",
            "declare",
            OWNER,
            "--seat",
            &format!("a={A}"),
            "--seat",
            &format!("b={B}"),
        ])
        .output()
        .expect("declare");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (dir, state)
}

fn cs(dir: &Path, state: &Path, member: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(dir)
        .env("COSMON_MOL_DIR", mol(state, member))
        .args(["--config", state.to_str().expect("state")])
        .args(args)
        .output()
        .expect("cs")
}

fn hook(dir: &Path, state: &Path, member: &str, adapter: &str) -> std::process::Output {
    use std::io::Write;
    let payload: &[u8] = if adapter == "codex" {
        br#"{"hook_event_name":"PostToolUse","tool_name":"functions.exec","tool_input":{},"tool_response":{}}"#
    } else {
        br#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{},"tool_response":{}}"#
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(dir)
        .env("COSMON_MOL_DIR", mol(state, member))
        .args(["work-hook", adapter])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload)
        .expect("payload");
    child.wait_with_output().expect("output")
}

#[test]
fn hook_delivers_once_and_records_unknown_context() {
    let (dir, state) = setup();
    let sent = cs(
        dir.path(),
        &state,
        A,
        &[
            "work",
            "send",
            "--to",
            "b",
            "--text",
            "finding",
            "--key",
            "finding-1",
        ],
    );
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let first = hook(dir.path(), &state, B, "claude");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&first.stdout).expect("one JSON document");
    let context = doc["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    assert!(
        context.contains("finding-1") && context.contains("digest"),
        "{context}"
    );
    let second = hook(dir.path(), &state, B, "claude");
    assert!(second.status.success());
    assert!(second.stdout.is_empty());
    let receipts = fs::read_to_string(mol(&state, OWNER).join("work/receipts/finding-1.jsonl"))
        .expect("receipts");
    let stages: Vec<serde_json::Value> = receipts
        .lines()
        .map(|line| serde_json::from_str(line).expect("receipt JSON"))
        .collect();
    assert!(stages
        .iter()
        .any(|r| r["stage"]["stage"] == "delivery_attempted"
            && r["stage"]["outcome"]["result"] == "submitted"));
    assert!(stages
        .iter()
        .any(|r| r["stage"]["stage"] == "context_delivered"
            && r["stage"]["observation"]["status"] == "unknown"));
    assert!(!receipts.contains("observed"));
    let capabilities = fs::read_to_string(mol(&state, OWNER).join("work/capabilities/b.jsonl"))
        .expect("capability");
    assert_eq!(capabilities.lines().count(), 1);
}

#[test]
fn empty_and_unreadable_store_do_not_inject_context() {
    let (dir, state) = setup();
    assert!(hook(dir.path(), &state, B, "codex").stdout.is_empty());
    fs::write(mol(&state, OWNER).join("work/scope.json"), b"invalid").expect("corrupt scope");
    let output = hook(dir.path(), &state, B, "codex");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn stale_work_reference_does_not_qualify_for_hook_wiring() {
    let (_dir, state) = setup();
    let member = mol(&state, B);
    assert!(cosmon_cli::work_hook::is_current_member(&member));
    let scope_path = mol(&state, OWNER).join("work/scope.json");
    let mut scope: serde_json::Value =
        serde_json::from_slice(&fs::read(&scope_path).expect("scope")).expect("scope JSON");
    scope["seats"].as_object_mut().expect("seats").remove("b");
    fs::write(scope_path, serde_json::to_vec(&scope).expect("scope JSON")).expect("revise roster");
    assert!(!cosmon_cli::work_hook::is_current_member(&member));
}

#[test]
fn declared_seats_exchange_and_ack_across_hook_adapters() {
    for (sender, recipient) in [
        ("claude", "codex"),
        ("claude", "claude"),
        ("codex", "codex"),
    ] {
        let (dir, state) = setup();
        let sent = cs(
            dir.path(),
            &state,
            A,
            &[
                "work",
                "send",
                "--to",
                "b",
                "--text",
                "review finding",
                "--key",
                "first",
            ],
        );
        assert!(
            sent.status.success(),
            "{}",
            String::from_utf8_lossy(&sent.stderr)
        );
        let received = hook(dir.path(), &state, B, recipient);
        assert!(received.status.success());
        let doc: serde_json::Value =
            serde_json::from_slice(&received.stdout).expect("recipient context");
        assert!(doc["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("context")
            .contains("first"));
        let reply = cs(
            dir.path(),
            &state,
            B,
            &[
                "work",
                "send",
                "--to",
                "a",
                "--text",
                "reply finding",
                "--key",
                "reply",
                "--reply-to",
                "first",
            ],
        );
        assert!(
            reply.status.success(),
            "{}",
            String::from_utf8_lossy(&reply.stderr)
        );
        let ack = cs(
            dir.path(),
            &state,
            B,
            &["work", "ack", "first", "--considered", "--reply", "reply"],
        );
        assert!(
            ack.status.success(),
            "{}",
            String::from_utf8_lossy(&ack.stderr)
        );
        let back = hook(dir.path(), &state, A, sender);
        assert!(back.status.success());
        let doc: serde_json::Value = serde_json::from_slice(&back.stdout).expect("reply context");
        assert!(doc["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("context")
            .contains("reply"));
        let listed = Command::new(env!("CARGO_BIN_EXE_cs"))
            .current_dir(dir.path())
            .args([
                "--config",
                state.to_str().expect("state"),
                "--json",
                "work",
                "list",
                OWNER,
            ])
            .output()
            .expect("list");
        assert!(listed.status.success());
        let doc: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("list JSON");
        for key in ["first", "reply"] {
            let view = &doc["envelopes"][key];
            assert!(view["admitted"].is_string(), "{view}");
            assert_eq!(view["delivery_attempts"].as_array().map(Vec::len), Some(1));
            assert_eq!(
                view["context_delivered"]["observation"]["status"],
                "unknown"
            );
        }
        assert_eq!(
            doc["envelopes"]["first"]["consumed"]["disposition"],
            "considered"
        );
    }
}
