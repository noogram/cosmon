// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic reproduction of the two local-adapter output defects an
//! external tester (a non-expert first-contact user) filed as
//! **noogram/cosmon #24** and **#25**.
//!
//! # The observed run
//!
//! `cs init` → `git init` → `cs demo` on the built-in `local` floor. The
//! worker produced a `main.rs`, and then:
//!
//! - **#24 (honesty).** `synthesis.md` claimed *"Code written to
//!   `…/.cosmon/state/fleets/default/molecules/task-<id>/main.rs`"* — a path
//!   that does not exist. The real file was in
//!   `.worktrees/task-<id>/main.rs`. The report was false.
//! - **#25 (happy path).** `cs done` refused: *"worktree has uncommitted
//!   changes (1 file(s)) — use --force to override: ?? main.rs"*, exit 1. The
//!   documented demo dead-ends unless the user discovers `--force`, and the
//!   output is left in a `.worktrees/` directory a newcomer never looks in.
//!
//! # Why there is no Ollama here
//!
//! Both defects are **plumbing**, not model behaviour, so this file drives the
//! real `cs` binary against a dependency-free mock of Ollama's OpenAI-compat
//! endpoint (a `TcpListener` on loopback, ~100 lines below). Two scripted
//! turns: turn 1 calls `write_file("main.rs", …)`, turn 2 stops with a prose
//! line reporting where the file went.
//!
//! The mock does **not** hard-code the path it claims. It reports the first
//! absolute directory the *briefing* names as the place to write output —
//! which is exactly how a small model behaves, and exactly how the tester's
//! run produced a false claim. So the assertion "the claimed path exists on
//! disk" is a true root-cause gate: it goes green only when the briefing stops
//! naming a directory the sandboxed worker cannot write to.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The file the mocked worker creates in its sandbox.
const DELIVERABLE: &str = "main.rs";

/// Body of the deliverable — a compilable hello-world, like the tester's run.
const DELIVERABLE_BODY: &str = "fn main() {\n    println!(\"hello from cosmon\");\n}\n";

// ---------------------------------------------------------------------------
// Mock Ollama (OpenAI-compat) — std only, no wiremock, no network
// ---------------------------------------------------------------------------

/// A loopback HTTP server speaking just enough of Ollama's OpenAI-compat
/// surface for one local-worker run: `GET /v1/models` (the dispatch preflight)
/// and two `POST /v1/chat/completions` turns.
struct MockOllama {
    base_url: String,
    /// Every `POST /v1/chat/completions` body the mock received, in order.
    ///
    /// This is the *provider request* — the only place that proves a selected
    /// model actually reached the wire rather than merely parsing (Defect 4 of
    /// the double-model review of #23).
    chat_requests: Arc<Mutex<Vec<String>>>,
    _handle: std::thread::JoinHandle<()>,
}

/// What the mocked model does with its turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    /// Turn 1 writes `main.rs`, turn 2 reports where it went (the #24/#25 run).
    WriteDeliverable,
    /// The model produces prose and nothing else — the no-op turn.
    Chatter,
}

impl MockOllama {
    /// Bind an ephemeral loopback port and serve until the test process exits.
    fn start(model: &str) -> Self {
        Self::start_with(model, Script::WriteDeliverable)
    }

    /// As [`MockOllama::start`], with an explicit behaviour script.
    fn start_with(model: &str, script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock ollama");
        let addr = listener.local_addr().expect("mock addr");
        let model = model.to_owned();
        let turns = Arc::new(AtomicUsize::new(0));
        let chat_requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&chat_requests);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let model = model.clone();
                let turns = Arc::clone(&turns);
                let recorded = Arc::clone(&recorded);
                // One thread per connection: reqwest may keep several alive.
                std::thread::spawn(move || {
                    let _ = serve_one(stream, &model, &turns, script, &recorded);
                });
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            chat_requests,
            _handle: handle,
        }
    }

    /// Snapshot of the chat-completions bodies received so far.
    fn chat_requests(&self) -> Vec<String> {
        self.chat_requests.lock().expect("mock lock").clone()
    }
}

/// Handle a single HTTP request on `stream`.
fn serve_one(
    mut stream: TcpStream,
    model: &str,
    turns: &AtomicUsize,
    script: Script,
    recorded: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
            .and_then(|v| v.parse::<usize>().ok())
        {
            content_length = value;
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();

    let payload = if request_line.contains("/v1/models") {
        format!(r#"{{"object":"list","data":[{{"id":"{model}","object":"model"}}]}}"#)
    } else {
        recorded.lock().expect("mock lock").push(body.clone());
        let turn = turns.fetch_add(1, Ordering::SeqCst);
        match script {
            Script::WriteDeliverable if turn == 0 => tool_call_turn(model),
            Script::WriteDeliverable => stop_turn(model, &body),
            Script::Chatter => chatter_turn(model),
        }
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Turn 1 — the model creates the deliverable through the sandboxed
/// `write_file` tool, with a **relative** path (the only kind the local
/// sandbox accepts).
fn tool_call_turn(model: &str) -> String {
    let arguments = serde_json::to_string(&serde_json::json!({
        "path": DELIVERABLE,
        "content": DELIVERABLE_BODY,
    }))
    .expect("serialize write_file arguments");
    serde_json::json!({
        "id": "mock-turn-1",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_write_file_1",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": arguments }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
    .to_string()
}

/// Turn 2 — the model stops and reports where it wrote the file.
///
/// It reports `<first absolute directory the briefing names>/main.rs`. That is
/// not a caricature: the briefing's "write durable output HERE" section is the
/// only place the worker learns an absolute path, and the tester's model
/// echoed exactly that directory. The claim is therefore true if and only if
/// the briefing names the directory the sandbox actually writes into.
fn stop_turn(model: &str, request_body: &str) -> String {
    let claimed = first_absolute_backticked_path(request_body)
        .unwrap_or_else(|| "/unknown".to_owned())
        .trim_end_matches('/')
        .to_owned();
    let content = format!("Code written to `{claimed}/{DELIVERABLE}`\n");
    serde_json::json!({
        "id": "mock-turn-2",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    })
    .to_string()
}

/// A no-op turn: the model answers in prose and touches no file. This is the
/// Jesse #4 "hello." shape, and the shape a re-tackle takes when the model has
/// nothing left to do.
fn chatter_turn(model: &str) -> String {
    serde_json::json!({
        "id": "mock-chatter",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Everything already looks done.\n" },
            "finish_reason": "stop"
        }]
    })
    .to_string()
}

/// First `` `…` ``-quoted absolute path in a chat-completions request body.
///
/// The briefing arrives JSON-escaped inside `messages`; backticks and `/` both
/// survive escaping untouched, so a byte scan is enough.
fn first_absolute_backticked_path(body: &str) -> Option<String> {
    let mut rest = body;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let close = after.find('`')?;
        let candidate = &after[..close];
        if candidate.starts_with('/') && !candidate.contains(' ') && !candidate.contains("\\n") {
            return Some(candidate.to_owned());
        }
        rest = &after[close + 1..];
    }
    None
}

// ---------------------------------------------------------------------------
// Project scaffolding
// ---------------------------------------------------------------------------

/// `cs` with a hermetic environment: no operator config, no parent molecule,
/// and the mock endpoint pinned as the local floor's backend.
fn cs(project: &Path, mock: &MockOllama) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(project)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_DEFAULT_ADAPTER")
        // Ambient session model pins outrank the per-project config row, so a
        // worker environment that exports one would decide which model these
        // tests dispatch. Strip them: the model must come from the test.
        .env_remove("COSMON_DEFAULT_MODEL")
        .env_remove("ANTHROPIC_MODEL")
        .env_remove("COSMON_ARTIFACT_DIR")
        .env_remove("OLLAMA_HOST")
        .env_remove("OPENAI_BASE_URL")
        .env(
            "COSMON_CONFIG_HOME",
            std::env::temp_dir().join("cosmon-test-xdg-isolated-local-output-honesty"),
        )
        .env("COSMON_LOCAL_BASE_URL", &mock.base_url)
        .env("COSMON_LOCAL_MODEL", MOCK_MODEL)
        .env("COSMON_LOCAL_TIMEOUT", "60")
        .env("GIT_AUTHOR_NAME", "cosmon-test")
        .env("GIT_AUTHOR_EMAIL", "test@cosmon.test")
        .env("GIT_COMMITTER_NAME", "cosmon-test")
        .env("GIT_COMMITTER_EMAIL", "test@cosmon.test");
    cmd
}

/// Model id the mock claims to serve (and the floor is pinned to).
const MOCK_MODEL: &str = "cosmon-mock-local";

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

/// A fresh project exactly as a stranger gets it: `git init`, one commit on
/// `main`, and a `.cosmon/` carrying the real `task-work` formula.
fn setup_project(dir: &Path) {
    let cosmon_dir = dir.join(".cosmon");
    fs::create_dir_all(cosmon_dir.join("state")).unwrap();
    fs::create_dir_all(cosmon_dir.join("formulas")).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"local-output-honesty-2cdb\"\n",
    )
    .unwrap();

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let formula_src = manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(".cosmon/formulas/task-work.formula.toml"))
        .expect("walk-up to workspace root must succeed");
    let formula_body = fs::read_to_string(&formula_src)
        .unwrap_or_else(|e| panic!("read task-work formula at {}: {e}", formula_src.display()));
    fs::write(
        cosmon_dir.join("formulas").join("task-work.formula.toml"),
        formula_body,
    )
    .unwrap();

    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join(".gitignore"), ".cosmon/state/\n").unwrap();
    fs::write(dir.join("README.md"), "# local-output-honesty\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn nucleate(project: &Path, mock: &MockOllama) -> String {
    let out = cs(project, mock)
        .args([
            "nucleate",
            "task-work",
            "--json",
            "--no-parent",
            "--var",
            "topic=Write a hello-world main.rs",
        ])
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(id) = v.get("id").and_then(serde_json::Value::as_str) {
                return id.to_owned();
            }
        }
    }
    panic!("could not parse molecule id from:\n{stdout}");
}

/// The molecule's canonical state directory.
fn molecule_dir(state_dir: &Path, mol_id: &str) -> PathBuf {
    state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(mol_id)
}

/// Run the mocked local worker to completion and return its molecule dir.
fn tackle_and_wait(project: &Path, mock: &MockOllama, mol_id: &str) -> PathBuf {
    // `--model` is pinned explicitly: the chain resolver otherwise inherits
    // the ambient session model, which the mock does not serve.
    tackle_extra_and_wait(project, mock, mol_id, &["--model", MOCK_MODEL], &[])
}

/// Block until the detached local worker has finished its whole post-loop
/// sequence, and return its molecule dir.
///
/// The barrier is the worker's own lifecycle record, not a file the loop
/// happens to write early: `run_local_worker` marks the worker `stopped` as its
/// *last* act on both the success and the guard-refusal path, strictly after
/// publishing. Waiting for `synthesis.md` plus a fixed sleep — the earlier
/// shape — raced the publisher under load and made this file intermittently
/// red. Bounded: a hang is a finding, never an infinite wait.
fn wait_for_worker_to_stop(mol_dir: &Path) {
    let state = mol_dir.join("state.json");
    for _ in 0..600 {
        let stopped = fs::read_to_string(&state)
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .and_then(|v| v.get("process")?.get("status")?.as_str().map(str::to_owned))
            .is_some_and(|status| status == "stopped");
        if stopped {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let log = fs::read_to_string(mol_dir.join("local-worker.log")).unwrap_or_default();
    panic!(
        "detached local worker never reached `stopped` under {} within 60s.\n\
         local-worker.log:\n{log}",
        mol_dir.display()
    );
}

/// Run `cs tackle` with extra arguments, then wait for the detached worker.
fn tackle_extra_and_wait(
    project: &Path,
    mock: &MockOllama,
    mol_id: &str,
    extra: &[&str],
    envs: &[(&str, &str)],
) -> PathBuf {
    let mut cmd = cs(project, mock);
    for (key, value) in envs {
        // An empty value means "unset it" — `cs` reads several of these as
        // present-but-blank otherwise.
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }
    let mut args = vec!["tackle", mol_id, "--adapter", "local"];
    args.extend_from_slice(extra);
    let out = cmd.args(&args).output().expect("spawn cs tackle");
    assert!(
        out.status.success(),
        "cs tackle {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let mol_dir = molecule_dir(&project.join(".cosmon").join("state"), mol_id);
    wait_for_worker_to_stop(&mol_dir);
    mol_dir
}

/// `git status --porcelain` of the project, as raw lines.
fn porcelain(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .expect("spawn git status");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The path a synthesis line of the form ``Code written to `…` `` claims.
fn claimed_path(synthesis: &str) -> String {
    let (_, after) = synthesis
        .split_once("Code written to `")
        .unwrap_or_else(|| panic!("synthesis.md carries no `Code written to` claim:\n{synthesis}"));
    let (path, _) = after
        .split_once('`')
        .unwrap_or_else(|| panic!("unterminated path claim in synthesis.md:\n{synthesis}"));
    path.to_owned()
}

// ---------------------------------------------------------------------------
// #24 — the reported output path must be the real one
// ---------------------------------------------------------------------------

/// **noogram/cosmon #24.** The path `synthesis.md` reports for the worker's
/// output must be the path the file is actually at.
///
/// Red before the fix: the briefing hands the sandboxed local worker the
/// canonical molecule directory as the place to "write durable output HERE",
/// but the local sandbox refuses absolute paths and every write lands in the
/// worktree. The worker's report names a file that does not exist.
#[test]
fn local_synthesis_reports_the_path_the_file_is_actually_at() {
    let mock = MockOllama::start(MOCK_MODEL);
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();
    setup_project(project);
    let mol_id = nucleate(project, &mock);
    let mol_dir = tackle_and_wait(project, &mock, &mol_id);

    let synthesis = fs::read_to_string(mol_dir.join("synthesis.md")).expect("read synthesis.md");
    let claimed = claimed_path(&synthesis);
    assert!(
        Path::new(&claimed).is_file(),
        "synthesis.md reports the worker's output at `{claimed}`, but no file is there. \
         A synthesis that names a path the artifact is not at is a false report \
         (noogram/cosmon #24).\n\nsynthesis.md:\n{synthesis}",
    );
    assert_eq!(
        fs::read_to_string(&claimed).expect("read claimed artifact"),
        DELIVERABLE_BODY,
        "the file at the reported path must be the worker's deliverable",
    );
    // Refutation of a trivial green: the claim must land in the worker's
    // sandbox, not merely on *some* file that happens to exist.
    assert!(
        claimed.contains(&format!(".worktrees/{mol_id}/")),
        "the reported path must be inside the worker's sandbox, got `{claimed}`",
    );
    // And cosmon's own ground-truth section must name the same file.
    assert!(
        synthesis.contains("## Files this worker produced (verified on disk)"),
        "synthesis.md must carry cosmon's verified artifact listing:\n{synthesis}",
    );
    assert!(
        synthesis.contains(&claimed),
        "the verified listing must name the deliverable:\n{synthesis}",
    );
}

// ---------------------------------------------------------------------------
// #25 — the documented demo must complete with no --force
// ---------------------------------------------------------------------------

/// **noogram/cosmon #25.** `cs init` → `git init` → local worker → `cs done`
/// must complete with **no `--force`**, and the output must be findable in the
/// project the user started in — not only inside a `.worktrees/` directory
/// that teardown then removes.
#[test]
fn local_worker_output_survives_cs_done_without_force() {
    let mock = MockOllama::start(MOCK_MODEL);
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();
    setup_project(project);
    let mol_id = nucleate(project, &mock);
    let mol_dir = tackle_and_wait(project, &mock, &mol_id);

    // Discoverability half: before teardown the synthesis already tells the
    // operator where the file is and where it will be afterwards.
    let synthesis = fs::read_to_string(mol_dir.join("synthesis.md")).expect("read synthesis.md");
    assert!(
        synthesis.contains("after teardown: `") && synthesis.contains(DELIVERABLE),
        "synthesis.md must say where the deliverable lands after teardown:\n{synthesis}",
    );

    let out = cs(project, &mock)
        .args(["done", &mol_id])
        .output()
        .expect("spawn cs done");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "`cs done` must complete the documented demo with NO --force. \
         A first-contact user has no reason to know that flag \
         (noogram/cosmon #25).\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    let landed = project.join(DELIVERABLE);
    assert!(
        landed.is_file(),
        "after teardown the deliverable must be findable at {} — the worktree it \
         was produced in is destroyed by `cs done`, so an output left only there \
         is lost (noogram/cosmon #25).\nstdout:\n{stdout}\nstderr:\n{stderr}",
        landed.display(),
    );
    assert_eq!(
        fs::read_to_string(&landed).expect("read merged deliverable"),
        DELIVERABLE_BODY,
    );
}

// ---------------------------------------------------------------------------
// REGRESSION — the publisher must be turn-scoped, not branch-wide
// ---------------------------------------------------------------------------

/// **Defect 1 of the double-model review of #24/#25 (data safety).**
///
/// `--no-worktree` parks the worker on the operator's own checkout. The
/// publisher introduced for #25 discovered deliverables *branch-wide* — every
/// tracked difference from `merge-base(HEAD, main)` plus every untracked
/// non-ignored file — and committed that whole set. On this path there is no
/// molecule branch, so the commit landed on whatever the operator had checked
/// out. A local run could therefore sweep unrelated user work into a worker
/// commit, commonly straight onto `main`.
///
/// The contract: a worker publishes what *it* changed during *its* turn, and
/// nothing else. Pre-existing untracked files stay untracked; a pre-existing
/// branch commit is not re-published.
#[test]
fn no_worktree_run_commits_only_this_turns_output() {
    let mock = MockOllama::start(MOCK_MODEL);
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();
    setup_project(project);

    // The operator's own state, none of it this worker's doing:
    //  - a branch that already differs from `main` (a committed file), and
    //  - an untracked file sitting in the working tree.
    git(project, &["checkout", "-q", "-b", "operator-branch"]);
    fs::write(
        project.join("operator-committed.txt"),
        "operator's commit\n",
    )
    .unwrap();
    git(project, &["add", "operator-committed.txt"]);
    git(project, &["commit", "-q", "-m", "operator work"]);
    fs::write(project.join("operator-notes.txt"), "operator's scratch\n").unwrap();

    let head_before = git_stdout(project, &["rev-parse", "HEAD"]);
    let mol_id = nucleate(project, &mock);
    tackle_extra_and_wait(
        project,
        &mock,
        &mol_id,
        &["--model", MOCK_MODEL, "--no-worktree"],
        &[("COSMON_ALLOW_NO_WORKTREE", "1")],
    );

    // The worker's own file was published — the fix must not disable the
    // publisher, only scope it.
    let head_after = git_stdout(project, &["rev-parse", "HEAD"]);
    assert_ne!(
        head_before, head_after,
        "the worker's deliverable must still be committed"
    );
    let committed_files = git_stdout(
        project,
        &["show", "--pretty=format:", "--name-only", "HEAD"],
    );
    let committed: Vec<&str> = committed_files.split_whitespace().collect();
    assert_eq!(
        committed,
        vec![DELIVERABLE],
        "the worker's commit must contain ONLY what the worker wrote this turn, \
         got {committed:?}"
    );

    // The operator's untracked scratch file is untouched — still untracked,
    // still its original content.
    let status = porcelain(project);
    assert!(
        status.contains("?? operator-notes.txt"),
        "a pre-existing untracked file must not be swept into a worker commit: \
         {status:?}"
    );
    assert_eq!(
        fs::read_to_string(project.join("operator-notes.txt")).unwrap(),
        "operator's scratch\n"
    );
}

/// **Defect 2 of the same review.** A no-op turn on a branch that already
/// differs from `main` must not be rescued by the *previous* turn's output:
/// the real-work guard is turn-scoped, so it refuses, nothing is committed, and
/// no synthesis section tells the operator their already-committed files are
/// "NOT committed" and need `--force`.
#[test]
fn no_op_retackle_on_a_diverged_branch_commits_nothing_and_recommends_no_force() {
    let mock = MockOllama::start_with(MOCK_MODEL, Script::Chatter);
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();
    setup_project(project);

    git(project, &["checkout", "-q", "-b", "operator-branch"]);
    fs::write(
        project.join("earlier-deliverable.txt"),
        "from a prior turn\n",
    )
    .unwrap();
    git(project, &["add", "earlier-deliverable.txt"]);
    git(project, &["commit", "-q", "-m", "prior turn"]);
    fs::write(project.join("operator-notes.txt"), "operator's scratch\n").unwrap();

    let head_before = git_stdout(project, &["rev-parse", "HEAD"]);
    let mol_id = nucleate(project, &mock);
    let mol_dir = tackle_extra_and_wait(
        project,
        &mock,
        &mol_id,
        &["--model", MOCK_MODEL, "--no-worktree"],
        &[("COSMON_ALLOW_NO_WORKTREE", "1")],
    );

    assert_eq!(
        head_before,
        git_stdout(project, &["rev-parse", "HEAD"]),
        "a turn that produced nothing must create no commit"
    );
    let status = porcelain(project);
    assert!(
        status.contains("?? operator-notes.txt"),
        "the operator's untracked file must survive a no-op turn: {status:?}"
    );

    let synthesis = fs::read_to_string(mol_dir.join("synthesis.md")).unwrap_or_default();
    assert!(
        !synthesis.contains("--force"),
        "a no-op turn must not recommend --force over files it never produced:\n{synthesis}"
    );
    assert!(
        !synthesis.contains("## Files this worker produced (verified on disk)"),
        "a no-op turn produced no files, so it must claim none:\n{synthesis}"
    );
}

// ---------------------------------------------------------------------------
// #23 — the selected model must reach the provider request, not just parse
// ---------------------------------------------------------------------------

/// **Defect 4 of the same review.** Every #23 model-selection test runs
/// `--dry-run`, which proves only that a configured value *parses*. It never
/// looks at the request the local adapter builds, so a resolver that dropped
/// the selection on the way to the provider would still pass.
///
/// This closes that gap end-to-end: `[adapters.local].default_model` is the
/// only place the model is named — no `--model` flag, no `COSMON_LOCAL_MODEL` —
/// and the assertion is on the `model` field of the actual
/// `POST /v1/chat/completions` body the adapter sent.
#[test]
fn config_selected_model_reaches_the_provider_request() {
    let mock = MockOllama::start(MOCK_MODEL);
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path();
    setup_project(project);

    // The durable, per-project mechanism — and the ONLY statement of the model.
    fs::write(
        project.join(".cosmon").join("config.toml"),
        format!(
            "[project]\nproject_id = \"local-output-honesty-2cdb\"\n\n\
             [adapters.local]\ndefault_model = \"{MOCK_MODEL}\"\n"
        ),
    )
    .unwrap();

    let mol_id = nucleate(project, &mock);
    tackle_extra_and_wait(project, &mock, &mol_id, &[], &[("COSMON_LOCAL_MODEL", "")]);

    let requests = mock.chat_requests();
    assert!(
        !requests.is_empty(),
        "the local adapter must have sent at least one chat-completions request"
    );
    for body in &requests {
        let value: serde_json::Value =
            serde_json::from_str(body).expect("chat request body must be JSON");
        assert_eq!(
            value.get("model").and_then(serde_json::Value::as_str),
            Some(MOCK_MODEL),
            "the configured local model must be the model the provider is asked \
             for — a selection that only parses is not a selection (COSMON #23).\n\
             request body:\n{body}"
        );
    }
}
