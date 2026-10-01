// SPDX-License-Identifier: AGPL-3.0-only

//! The gateway recipe of `docs/guides/gateway-worker.md`, executed end to end
//! against a loopback responder (noogram/cosmon #145).
//!
//! The recipe reaches a hosted model through the **existing** `openai`
//! adapter: `[adapters.openai]` carries the gateway `base_url`, the name of
//! the environment variable holding the key, and the namespaced model id. No
//! network is involved: the "gateway" is a `TcpListener` on `127.0.0.1`, so
//! the test asserts what the configured worker actually put on the wire.
//!
//! Asserted properties, each one a sentence of the guide:
//!
//! 1. the request lands on `<base_url>/v1/chat/completions` with the gateway's
//!    path prefix preserved, once per turn;
//! 2. it carries `Authorization: Bearer <value of the configured variable>`;
//! 3. the namespaced model id reaches the wire verbatim, and a per-molecule
//!    `--model` pin overrides the configured `default_model`;
//! 4. a declared `api_key_env` that is unset refuses the dispatch even when a
//!    sibling vendor key is present, and sends nothing;
//! 5. the model the gateway reports back is recorded as a `ModelObserved`
//!    event, while the tool call the model emitted is executed in the worktree.
//!
//! The key used here is a fabricated literal; no real credential is read.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Namespaced model id in the form the gateway documents (`publisher/model`).
const GATEWAY_MODEL: &str = "publisher-a/model-pro-preview";

/// A second id, used to prove a per-molecule pin outranks `default_model`.
const PINNED_MODEL: &str = "publisher-a/model-flash-preview";

/// Environment variable the recipe names in `api_key_env`.
const KEY_ENV: &str = "GATEWAY_API_KEY";

/// Fabricated credential; never a real one.
const FAKE_KEY: &str = "gw-test-key-not-a-secret";

/// File the scripted model creates through its `write_file` tool call.
const DELIVERABLE: &str = "gateway-proof.txt";

/// One request as the loopback gateway saw it.
#[derive(Clone, Debug)]
struct Seen {
    request_line: String,
    authorization: Option<String>,
    body: String,
}

/// Loopback stand-in for an OpenAI-compatible gateway.
struct Gateway {
    /// `http://127.0.0.1:<port>`; the test appends the gateway path prefix.
    origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    _handle: std::thread::JoinHandle<()>,
}

impl Gateway {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback gateway");
        let addr = listener.local_addr().expect("gateway addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let turns = Arc::new(AtomicUsize::new(0));
        let (seen_t, turns_t) = (Arc::clone(&seen), Arc::clone(&turns));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (seen, turns) = (Arc::clone(&seen_t), Arc::clone(&turns_t));
                std::thread::spawn(move || {
                    let _ = serve_one(stream, &seen, &turns);
                });
            }
        });
        Self {
            origin: format!("http://{addr}"),
            seen,
            _handle: handle,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("gateway lock").clone()
    }
}

fn serve_one(
    mut stream: TcpStream,
    seen: &Mutex<Vec<Seen>>,
    turns: &AtomicUsize,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    let mut authorization = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            authorization = header.split_once(':').map(|(_, v)| v.trim().to_owned());
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();
    let turn = turns.fetch_add(1, Ordering::SeqCst);
    seen.lock().expect("gateway lock").push(Seen {
        request_line: request_line.trim_end().to_owned(),
        authorization,
        body,
    });
    let payload = if turn == 0 {
        tool_call_turn()
    } else {
        stop_turn()
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Turn 1: the model asks for a file to be created. The gateway answers with
/// the model id it served, which is what the realized-model capture reads.
fn tool_call_turn() -> String {
    let arguments = serde_json::json!({ "path": DELIVERABLE, "content": "via gateway\n" });
    serde_json::json!({
        "id": "gw-1",
        "object": "chat.completion",
        "model": GATEWAY_MODEL,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": arguments.to_string() }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
    .to_string()
}

fn stop_turn() -> String {
    serde_json::json!({
        "id": "gw-2",
        "object": "chat.completion",
        "model": GATEWAY_MODEL,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "created gateway-proof.txt" },
            "finish_reason": "stop"
        }]
    })
    .to_string()
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
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A project whose config is exactly the recipe, pointed at the loopback
/// gateway. `base_url` is `<origin>/api`, the prefix a real gateway uses.
fn setup_project(dir: &Path, gateway: &Gateway) {
    let cosmon_dir = dir.join(".cosmon");
    fs::create_dir_all(cosmon_dir.join("state")).unwrap();
    fs::create_dir_all(cosmon_dir.join("formulas")).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        format!(
            "[project]\nproject_id = \"gateway-worker-recipe-5e1a\"\n\n\
             [adapters.openai]\nbase_url = \"{}/api\"\napi_key_env = \"{KEY_ENV}\"\n\
             default_model = \"{GATEWAY_MODEL}\"\n",
            gateway.origin
        ),
    )
    .unwrap();

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let formula_src = manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(".cosmon/formulas/task-work.formula.toml"))
        .expect("walk-up to workspace root must succeed");
    fs::write(
        cosmon_dir.join("formulas").join("task-work.formula.toml"),
        fs::read_to_string(&formula_src).expect("read task-work formula"),
    )
    .unwrap();

    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join(".gitignore"), ".cosmon/state/\n").unwrap();
    fs::write(dir.join("README.md"), "# gateway-worker-recipe\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

/// `cs` with a hermetic environment: no ambient model pin, no ambient vendor
/// keys, no operator config.
fn cs(project: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(project)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env_remove("COSMON_DEFAULT_MODEL")
        .env_remove("ANTHROPIC_MODEL")
        .env_remove("COSMON_ARTIFACT_DIR")
        .env_remove("COSMON_EGRESS_POLICY")
        .env_remove("CB_DEPTH")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_MODEL")
        .env_remove("XAI_API_KEY")
        .env_remove("MOONSHOT_API_KEY")
        .env_remove(KEY_ENV)
        .env(
            "COSMON_CONFIG_HOME",
            std::env::temp_dir().join("cosmon-test-xdg-isolated-gateway-recipe"),
        )
        .env("GIT_AUTHOR_NAME", "cosmon-test")
        .env("GIT_AUTHOR_EMAIL", "test@cosmon.test")
        .env("GIT_COMMITTER_NAME", "cosmon-test")
        .env("GIT_COMMITTER_EMAIL", "test@cosmon.test");
    cmd
}

fn nucleate(project: &Path) -> String {
    let out = cs(project)
        .args([
            "nucleate",
            "task-work",
            "--json",
            "--no-parent",
            "--var",
            "topic=Create gateway-proof.txt",
        ])
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_owned))
        .expect("molecule id in nucleate output")
}

fn state_dir(project: &Path) -> PathBuf {
    project.join(".cosmon").join("state")
}

/// Run `cs tackle --adapter openai` with the given extra args and key setting.
fn tackle(
    project: &Path,
    mol_id: &str,
    extra: &[&str],
    envs: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = cs(project);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.args(["tackle", mol_id, "--adapter", "openai"])
        .args(extra)
        .output()
        .expect("spawn cs tackle")
}

fn events_text(project: &Path) -> String {
    let mut all = String::new();
    let mut stack = vec![state_dir(project)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "events.jsonl") {
                all.push_str(&fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    all
}

/// The configured recipe reaches the configured gateway with the configured
/// key and model, preserves the `/api` prefix, executes the returned tool call
/// and records the model the gateway reports.
#[test]
fn recipe_reaches_the_configured_gateway_with_the_configured_model() {
    let gateway = Gateway::start();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    setup_project(project, &gateway);
    let mol_id = nucleate(project);

    let out = tackle(project, &mol_id, &[], &[(KEY_ENV, FAKE_KEY)]);
    assert!(
        out.status.success(),
        "cs tackle --adapter openai failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let seen = gateway.seen();
    assert_eq!(seen.len(), 2, "one tool turn and one stop turn: {seen:#?}");
    for (i, req) in seen.iter().enumerate() {
        assert!(
            req.request_line
                .starts_with("POST /api/v1/chat/completions "),
            "turn {}: gateway prefix must be kept and /v1 appended once, got {:?}",
            i + 1,
            req.request_line
        );
        assert_eq!(
            req.authorization.as_deref(),
            Some(format!("Bearer {FAKE_KEY}").as_str()),
            "turn {}: the key comes from the variable named by api_key_env",
            i + 1
        );
        let body: serde_json::Value = serde_json::from_str(&req.body).expect("request is JSON");
        assert_eq!(
            body["model"],
            GATEWAY_MODEL,
            "turn {}: the namespaced model id must reach the wire verbatim",
            i + 1
        );
    }

    let worktree = project.join(".worktrees").join(&mol_id);
    assert_eq!(
        fs::read_to_string(worktree.join(DELIVERABLE))
            .ok()
            .as_deref(),
        Some("via gateway\n"),
        "the tool call returned by the gateway must be executed in the worktree"
    );

    let events = events_text(project);
    assert!(
        events.contains("model_observed") && events.contains(GATEWAY_MODEL),
        "the model the gateway served must be recorded as a ModelObserved event:\n{events}"
    );
    let host = gateway.origin.trim_start_matches("http://");
    let host = host.split(':').next().unwrap_or(host);
    assert!(
        events.contains("remote_egress_opt_in") && events.contains(host),
        "the egress audit must name the configured gateway host, not the adapter name:\n{events}"
    );
}

/// `--model` is the top of the chain: it outranks `default_model` and is still
/// sent verbatim.
#[test]
fn per_molecule_model_pin_outranks_the_configured_default() {
    let gateway = Gateway::start();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    setup_project(project, &gateway);
    let mol_id = nucleate(project);

    let out = tackle(
        project,
        &mol_id,
        &["--model", PINNED_MODEL],
        &[(KEY_ENV, FAKE_KEY)],
    );
    assert!(
        out.status.success(),
        "cs tackle failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = gateway.seen();
    assert!(!seen.is_empty(), "the gateway must have been reached");
    for req in &seen {
        let body: serde_json::Value = serde_json::from_str(&req.body).expect("JSON");
        assert_eq!(body["model"], PINNED_MODEL);
    }
}

/// A declared `api_key_env` is the only credential source: when it is unset,
/// the dispatch is refused and nothing is sent, even with a sibling vendor key
/// present in the environment.
#[test]
fn unset_declared_key_refuses_and_sends_nothing() {
    let gateway = Gateway::start();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    setup_project(project, &gateway);
    let mol_id = nucleate(project);

    let out = tackle(
        project,
        &mol_id,
        &[],
        &[("OPENAI_API_KEY", "sibling-decoy-key")],
    );
    assert!(
        !out.status.success(),
        "dispatch must be refused when the declared key variable is unset"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(KEY_ENV),
        "the refusal must name {KEY_ENV}: {stderr}"
    );
    assert!(
        gateway.seen().is_empty(),
        "no request may reach the gateway, and the sibling key must not be used"
    );
}
