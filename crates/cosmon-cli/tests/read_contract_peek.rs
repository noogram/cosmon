// SPDX-License-Identifier: AGPL-3.0-only

//! Read contracts for what `cs peek` shows per molecule (issue #167).
//!
//! `docs/book/src/reference/read-contracts.md` lists the fields an external
//! reader may rely on to rebuild a `cs peek` row without calling `cs peek`.
//! This test drives the real `cs nucleate` and `cs tackle` in a scratch galaxy
//! against a loopback stand-in for the model endpoint (no model call) and
//! asserts that each field the page names is present in the raw JSON a reader
//! sees: `usage_observed` events, the liveness timestamps, the worktree path,
//! and the typed links that carry polymer membership and order.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

const MODEL: &str = "stub-model-1";
const KEY_ENV: &str = "STUB_API_KEY";

/// Two-turn responder: the model first asks for a file to be written (the
/// step needs a worktree change to be accepted), then stops and reports its
/// usage.
fn serve(mut stream: TcpStream, turn: usize) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim_end().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let write_file = serde_json::json!({ "path": "proof.txt", "content": "proof\n" });
    let first_turn = serde_json::json!({
        "id": "stub-0",
        "object": "chat.completion",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": write_file.to_string() }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 120, "completion_tokens": 7, "total_tokens": 127 }
    })
    .to_string();
    let stop_turn = serde_json::json!({
        "id": "stub-1",
        "object": "chat.completion",
        "model": MODEL,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "done" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 120, "completion_tokens": 7, "total_tokens": 127 }
    })
    .to_string();
    let payload = if turn == 0 { first_turn } else { stop_turn };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )?;
    stream.flush()
}

fn start_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback endpoint");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let turns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for stream in listener.incoming().flatten() {
            let turn = turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || {
                let _ = serve(stream, turn);
            });
        }
    });
    origin
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "cosmon-test")
        .env("GIT_AUTHOR_EMAIL", "test@cosmon.test")
        .env("GIT_COMMITTER_NAME", "cosmon-test")
        .env("GIT_COMMITTER_EMAIL", "test@cosmon.test")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn setup_project(dir: &Path, origin: &str) {
    let cosmon = dir.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        format!(
            "[project]\nproject_id = \"read-contract-peek-3c9d\"\n\n\
             [adapters.openai]\nbase_url = \"{origin}/api\"\napi_key_env = \"{KEY_ENV}\"\n\
             default_model = \"{MODEL}\"\n"
        ),
    )
    .unwrap();
    let formula =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::write(
        cosmon.join("formulas/task-work.formula.toml"),
        fs::read_to_string(formula).unwrap(),
    )
    .unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join(".gitignore"), ".cosmon/state/\n").unwrap();
    fs::write(dir.join("README.md"), "# scratch\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn cs(project: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(project);
    for var in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_STATE_DIR",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_DEFAULT_MODEL",
        "ANTHROPIC_MODEL",
        "COSMON_ARTIFACT_DIR",
        "COSMON_EGRESS_POLICY",
        "CB_DEPTH",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_MODEL",
    ] {
        cmd.env_remove(var);
    }
    cmd.env("COSMON_CONFIG_HOME", project.join("isolated-config-home"))
        .env(KEY_ENV, "stub-key-not-a-secret")
        .env("GIT_AUTHOR_NAME", "cosmon-test")
        .env("GIT_AUTHOR_EMAIL", "test@cosmon.test")
        .env("GIT_COMMITTER_NAME", "cosmon-test")
        .env("GIT_COMMITTER_EMAIL", "test@cosmon.test");
    cmd
}

fn nucleate(project: &Path, extra: &[&str]) -> String {
    let out = cs(project)
        .args(["nucleate", "task-work", "--json", "--no-parent"])
        .args(["--var", "topic=read contract"])
        .args(extra)
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "nucleate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v["id"].as_str().map(str::to_owned))
        .expect("molecule id")
}

fn state(project: &Path, id: &str) -> serde_json::Value {
    let path: PathBuf = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(id)
        .join("state.json");
    serde_json::from_str(&fs::read_to_string(path).expect("state.json")).expect("state JSON")
}

fn events(project: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(project.join(".cosmon/state/events.jsonl"))
        .expect("events.jsonl")
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn the_peek_fields_named_by_the_read_contract_are_present_after_a_real_run() {
    let origin = start_endpoint();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    setup_project(project, &origin);

    // Polymer of two: `first` blocks `second`.
    let first = nucleate(project, &[]);
    let second = nucleate(project, &["--blocked-by", &first]);

    let out = cs(project)
        .args(["tackle", &first, "--adapter", "openai"])
        .output()
        .expect("spawn cs tackle");
    assert!(
        out.status.success(),
        "tackle: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let first_state = state(project, &first);
    let second_state = state(project, &second);
    let log = events(project);

    // 1. Usage per model segment. Each answered request appends one
    // `usage_observed` event whose counters are cumulative over the history.
    let usage: Vec<&serde_json::Value> = log
        .iter()
        .filter(|e| e["type"] == "usage_observed")
        .map(|e| &e["usage"])
        .collect();
    assert_eq!(usage.len(), 2, "one observation per request: {usage:#?}");
    for u in &usage {
        assert_eq!(u["schema_version"], 1, "per-observation version: {u}");
        assert_eq!(u["subject"]["history"]["kind"], "known");
        assert_eq!(u["tokens"]["model_segments"]["status"], "available");
    }
    assert_eq!(
        usage[0]["subject"]["history"],
        usage[1]["subject"]["history"]
    );
    let segment = |u: &serde_json::Value, field: &str| -> u64 {
        let seg = &u["tokens"]["model_segments"]["value"][0];
        assert_eq!(seg["model"], MODEL);
        assert_eq!(seg[field]["status"], "measured", "{seg}");
        seg[field]["tokens"].as_u64().expect("measured tokens")
    };
    assert_eq!(segment(usage[0], "input_tokens"), 120);
    assert_eq!(
        segment(usage[1], "input_tokens"),
        240,
        "cumulative, not a delta"
    );
    assert_eq!(segment(usage[1], "output_tokens"), 14);
    // A category the endpoint never reported is unavailable, never zero.
    let cached = &usage[1]["tokens"]["model_segments"]["value"][0]["cached_input_tokens"];
    assert_eq!(cached["status"], "unavailable", "{cached}");
    assert_eq!(
        usage[1]["subject"]["worker_id"],
        first_state["process"]["worker_id"]
    );
    // `worker_spawned` ties that worker to the molecule once the process
    // record is gone.
    assert!(
        log.iter().any(|e| e["type"] == "worker_spawned"
            && e["worker_id"] == usage[1]["subject"]["worker_id"]
            && e["molecule_id"] == first.as_str()),
        "no worker_spawned event links the usage worker to the molecule"
    );

    // 2. Liveness timestamps, both written by the step that just completed.
    for key in ["last_progress_at", "last_output_at"] {
        let stamp = first_state[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key}: {first_state}"));
        assert!(
            chrono::DateTime::parse_from_rfc3339(stamp).is_ok(),
            "{key} is not RFC 3339: {stamp}"
        );
    }

    // 3. Worktree path: the directory the worker ran in, as an explicit field.
    let worktree = first_state["process"]["worktree_path"]
        .as_str()
        .expect("process.worktree_path");
    assert!(
        Path::new(worktree).join("proof.txt").exists(),
        "process.worktree_path {worktree} is not the directory the worker wrote in"
    );
    assert!(
        worktree.ends_with(&format!(".worktrees/{first}")),
        "{worktree}"
    );

    // 4. Polymer membership and order: the edge is on both ends.
    let links = |s: &serde_json::Value, rel: &str, key: &str| -> Vec<String> {
        s["typed_links"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["rel"] == rel)
            .filter_map(|l| l[key].as_str().map(str::to_owned))
            .collect()
    };
    assert_eq!(links(&first_state, "blocks", "target"), [second.clone()]);
    assert_eq!(
        links(&second_state, "blocked_by", "source"),
        [first.clone()]
    );
}
