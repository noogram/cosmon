// SPDX-License-Identifier: AGPL-3.0-only

//! Resume only from checkpoints the evidence proves safe.
//!
//! The in-process loop writes turn and effect evidence before it acts
//! (ADR-185). These tests check what a restart does with it.
//!
//! - **Crash seams.** The test binary re-executes itself as a child that runs
//!   the real loop over the real native log with a scripted model and exits
//!   abruptly at one deterministic seam. The parent reads the evidence back
//!   from disk and tries to continue.
//! - **The dispatch path.** `cs tackle` runs a real worker against a loopback
//!   responder, the attempt is interrupted, and `cs tackle --force --resume`
//!   continues it. The responder reads the galaxy ledger file at its exact path
//!   to prove the continuation is durable before its first request is sent.
//!
//! Every network address is loopback and every credential is a fabricated
//! literal.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cosmon_agent_harness::spine::{
    run_loop_counted_with_progress_budgeted, LoopProgress, ResumeState, ScriptedProviderFn, Turn,
};
use cosmon_agent_harness::tool::ToolCall;
use cosmon_agent_harness::{
    ContextBudget, HarnessError, LoopBudget, MessageLog, ToolBudget, TurnBudget, TurnJournal,
};
use cosmon_core::harness_turn::{
    BlobKind, BlobRef, EvidenceError, ResumeRefusal, TurnEvidenceStore, TurnRecord,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_provider::openai::{ChatMessage, OpenAILog};
use cosmon_state::harness_checkpoint::{
    acquire_loop_owner, blob_dir, load_resumable, FileTurnEvidenceStore,
};

const FIRST_ATTEMPT: &str = "harness/mol/worker-1/attempt-1";
const SECOND_ATTEMPT: &str = "harness/mol/worker-1/attempt-2";
const SHELL_LOG: &str = "effect.log";

/// Two tool calls, two turns, the default input ceiling. Small on purpose: the
/// continuation must inherit what was spent, so a restart that refreshed the
/// tool counter would get one call more than a single uninterrupted run.
fn budget() -> LoopBudget {
    LoopBudget {
        turns: TurnBudget::DEFAULT,
        tools: ToolBudget { max_tool_calls: 2 },
        context: ContextBudget::DEFAULT,
    }
}

fn mol_id() -> MoleculeId {
    MoleculeId::new("task-20261003-bbbb").expect("molecule id")
}

fn store(root: &Path, history: &str) -> FileTurnEvidenceStore {
    FileTurnEvidenceStore::new(
        &root.join("state"),
        &root.join("mol"),
        mol_id(),
        WorkerId::new("worker-1").expect("worker id"),
        history,
    )
}

fn journal(inner: Arc<dyn TurnEvidenceStore>) -> Arc<TurnJournal> {
    Arc::new(TurnJournal::new(inner).with_pin("requested_model", "model-a"))
}

fn write_turn(id: &str, path: &str) -> Turn<OpenAILog> {
    let arguments = serde_json::json!({ "path": path, "content": "x\n" }).to_string();
    let assistant: ChatMessage = serde_json::from_value(serde_json::json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": { "name": "write_file", "arguments": arguments }
        }]
    }))
    .expect("native assistant envelope");
    Turn::ToolCalls {
        assistant,
        calls: vec![ToolCall::new(id, "write_file", arguments)],
    }
}

/// A shell command that is not idempotent: every run appends a line.
fn shell_turn() -> Turn<OpenAILog> {
    let arguments =
        serde_json::json!({ "command": format!("echo run >> {SHELL_LOG}") }).to_string();
    let assistant: ChatMessage = serde_json::from_value(serde_json::json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "call-shell",
            "type": "function",
            "function": { "name": "exec_command", "arguments": arguments }
        }]
    }))
    .expect("native assistant envelope");
    Turn::ToolCalls {
        assistant,
        calls: vec![ToolCall::new("call-shell", "exec_command", arguments)],
    }
}

/// The scripted model: one turn per index, the same for every run so a
/// continuation can be compared to an uninterrupted one.
fn script(turn: usize) -> Turn<OpenAILog> {
    match turn {
        0 => write_turn("call-0", "a.txt"),
        1 => write_turn("call-1", "b.txt"),
        2 => write_turn("call-2", "c.txt"),
        _ => Turn::Stop("finished".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// The crash child
// ---------------------------------------------------------------------------

struct CrashingStore {
    inner: FileTurnEvidenceStore,
    seam: String,
}

fn kind_of(record: &TurnRecord) -> String {
    serde_json::to_value(record).expect("record json")["record"]
        .as_str()
        .expect("record tag")
        .to_owned()
}

impl TurnEvidenceStore for CrashingStore {
    fn put_blob(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobRef, EvidenceError> {
        // The result blob is written after the tool body ran and before its
        // receipt: "effect done, receipt missing".
        if self.seam == "tool_effect_without_receipt" && kind == BlobKind::ToolResult {
            std::process::exit(137);
        }
        self.inner.put_blob(kind, bytes)
    }

    fn append(&self, record: TurnRecord) -> Result<(), EvidenceError> {
        let kind = kind_of(&record);
        self.inner.append(record)?;
        if self.seam == "after_checkpoint" && kind == "checkpoint" {
            std::process::exit(137);
        }
        Ok(())
    }
}

/// Body of the crash child. It does nothing unless the parent set the seam.
#[test]
fn crash_child_body() {
    let Ok(seam) = std::env::var("COSMON_W9_CRASH_SEAM") else {
        return;
    };
    let root = PathBuf::from(std::env::var("COSMON_W9_ROOT").expect("root"));
    let crashing = CrashingStore {
        inner: store(&root, FIRST_ATTEMPT),
        seam: seam.clone(),
    };
    let mut progress = LoopProgress::default().with_journal(journal(Arc::new(crashing)));
    let provider = ScriptedProviderFn::<OpenAILog, std::io::Error>::new(move |_log| {
        Ok(if seam == "tool_effect_without_receipt" {
            shell_turn()
        } else {
            script(0)
        })
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime.block_on(run_loop_counted_with_progress_budgeted(
        &provider,
        "brief",
        &root.join("work"),
        None,
        &mut progress,
        budget(),
    ));
    // Reaching this line means the seam never fired.
    std::process::exit(if result.is_ok() { 3 } else { 4 });
}

struct Crashed {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

fn crash_at(seam: &str) -> Crashed {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    for dir in ["state", "mol", "work"] {
        fs::create_dir_all(root.join(dir)).expect("dir");
    }
    let status = Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "crash_child_body",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("COSMON_W9_CRASH_SEAM", seam)
        .env("COSMON_W9_ROOT", &root)
        .status()
        .expect("spawn crash child");
    assert_eq!(
        status.code(),
        Some(137),
        "the child must have been killed at the {seam} seam, not finished or failed"
    );
    Crashed { _tmp: tmp, root }
}

/// What an uninterrupted run sends and spends, to compare a continuation to.
struct Observed {
    /// The native log of every request, in order.
    requests: Vec<String>,
    outcome: Result<u32, String>,
}

fn run(
    root: &Path,
    history: &str,
    resume: Option<ResumeState>,
    first_script_turn: usize,
    pins: &[(&str, &str)],
) -> Observed {
    let mut journal = TurnJournal::new(Arc::new(store(root, history)));
    for (name, value) in pins {
        journal = journal.with_pin(*name, *value);
    }
    let mut progress = LoopProgress::default().with_journal(Arc::new(journal));
    if let Some(resume) = resume {
        progress = progress.with_resume(resume);
    }
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&requests);
    let turns = AtomicUsize::new(first_script_turn);
    let provider = ScriptedProviderFn::<OpenAILog, std::io::Error>::new(move |log| {
        sink.lock()
            .expect("request sink")
            .push(String::from_utf8(log.encode_checkpoint().expect("encodable")).expect("utf8"));
        Ok(script(turns.fetch_add(1, Ordering::SeqCst)))
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let outcome = runtime
        .block_on(run_loop_counted_with_progress_budgeted(
            &provider,
            "brief",
            &root.join("work"),
            None,
            &mut progress,
            budget(),
        ))
        .map(|o| o.tools_dispatched)
        .map_err(|e| match e {
            HarnessError::Resume(refusal) => format!("resume refused: {refusal}"),
            other => other.to_string(),
        });
    let requests = requests.lock().expect("request sink").clone();
    Observed { requests, outcome }
}

fn candidate(root: &Path) -> Result<ResumeState, ResumeRefusal> {
    load_resumable(&root.join("state"), &root.join("mol"), &mol_id()).map(|c| ResumeState {
        plan: c.plan,
        log: c.log,
    })
}

// ---------------------------------------------------------------------------
// RED/GREEN: a shell effect with no receipt is not repeated
// ---------------------------------------------------------------------------

#[test]
fn a_shell_effect_without_a_receipt_is_surfaced_and_never_repeated() {
    let c = crash_at("tool_effect_without_receipt");
    let log = c.root.join("work").join(SHELL_LOG);
    assert_eq!(
        fs::read_to_string(&log)
            .expect("the shell ran once")
            .lines()
            .count(),
        1,
        "the effect happened exactly once before the kill"
    );

    let refusal = candidate(&c.root).expect_err("the evidence cannot prove the effect finished");
    match &refusal {
        ResumeRefusal::UnresolvedEffect { calls } => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "exec_command");
            assert_eq!(calls[0].call_id, "call-shell");
        }
        other => panic!("expected an unresolved effect, got {other:?}"),
    }
    assert!(
        refusal.to_string().contains("unknown"),
        "the uncertainty is stated, not hidden: {refusal}"
    );
    assert_eq!(
        fs::read_to_string(&log).expect("log").lines().count(),
        1,
        "asking to resume must not run the command again"
    );
}

// ---------------------------------------------------------------------------
// RED/GREEN: a complete file-tool checkpoint continues exactly
// ---------------------------------------------------------------------------

/// Run the script straight through, with no interruption, in a fresh scratch.
fn uninterrupted() -> Observed {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    for dir in ["state", "mol", "work"] {
        fs::create_dir_all(root.join(dir)).expect("dir");
    }
    run(
        &root,
        FIRST_ATTEMPT,
        None,
        0,
        &[("requested_model", "model-a")],
    )
}

#[test]
fn a_complete_checkpoint_continues_the_uninterrupted_request_sequence_and_keeps_counters() {
    // The reference: with a two-call ceiling the third tool call is refused.
    let reference = uninterrupted();
    assert_eq!(reference.requests.len(), 3, "turns 0, 1 and 2 are sent");
    assert!(
        reference
            .outcome
            .as_ref()
            .is_err_and(|e| e.contains("tool budget exhausted")),
        "{:?}",
        reference.outcome
    );

    let c = crash_at("after_checkpoint");
    assert!(c.root.join("work/a.txt").exists());
    let state = candidate(&c.root).expect("a complete file-tool checkpoint is safe");
    assert_eq!(state.plan.next_turn, 1);
    assert_eq!(state.plan.tools_spent, 1);

    // The continuation sends what the uninterrupted run sent from turn 1 on.
    let resumed = run(
        &c.root,
        SECOND_ATTEMPT,
        Some(state),
        1,
        &[("requested_model", "model-a")],
    );
    assert_eq!(
        resumed.requests,
        reference.requests[1..].to_vec(),
        "the restored log is byte-identical to the one the uninterrupted run held"
    );
    // The counter was not refreshed: the second call after the restart is the
    // third overall and must trip the same ceiling. A fresh counter would have
    // let it through and finished.
    assert!(
        resumed
            .outcome
            .as_ref()
            .is_err_and(|e| e.contains("tool budget exhausted")),
        "{:?}",
        resumed.outcome
    );
    assert!(c.root.join("work/b.txt").exists(), "turn 1 ran once");
    assert!(
        !c.root.join("work/c.txt").exists(),
        "the call over the ceiling did not run"
    );

    // The continuation is on the ledger as one that inherited the checkpoint.
    let loaded = cosmon_state::harness_checkpoint::load_attempt(
        &c.root.join("state"),
        &c.root.join("mol"),
        &mol_id(),
        SECOND_ATTEMPT,
    )
    .expect("the continuation's evidence loads");
    let inherited = loaded.reconstruction.resumed_from.expect("resumed record");
    assert_eq!(inherited.history_id, FIRST_ATTEMPT);
    assert_eq!(inherited.checkpoint_turn, 0);
    assert_eq!(inherited.requests_sent, 1);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn a_changed_model_pin_refuses_the_continuation_before_anything_is_sent() {
    let c = crash_at("after_checkpoint");
    let state = candidate(&c.root).expect("safe");
    let moved = run(
        &c.root,
        SECOND_ATTEMPT,
        Some(state),
        1,
        &[("requested_model", "model-b")],
    );
    assert!(
        moved
            .outcome
            .as_ref()
            .is_err_and(|e| e.starts_with("resume refused") && e.contains("requested_model")),
        "{:?}",
        moved.outcome
    );
    assert!(moved.requests.is_empty(), "no request left the process");
    assert!(
        !c.root.join("work/b.txt").exists(),
        "no tool ran under the new pin"
    );
    // The refusal wrote nothing, so the interrupted attempt is still the latest
    // one and can be continued once the pin is restored.
    assert!(candidate(&c.root).is_ok());
}

#[test]
fn a_changed_tool_ceiling_refuses_the_continuation() {
    let c = crash_at("after_checkpoint");
    let state = candidate(&c.root).expect("safe");
    let mut progress =
        LoopProgress::default().with_journal(journal(Arc::new(store(&c.root, SECOND_ATTEMPT))));
    progress = progress.with_resume(state);
    let provider = ScriptedProviderFn::<OpenAILog, std::io::Error>::new(|_| {
        Ok(Turn::Stop("never sent".to_owned()))
    });
    let raised = LoopBudget {
        tools: ToolBudget { max_tool_calls: 64 },
        ..budget()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let outcome = runtime.block_on(run_loop_counted_with_progress_budgeted(
        &provider,
        "brief",
        &c.root.join("work"),
        None,
        &mut progress,
        raised,
    ));
    assert!(
        matches!(
            outcome,
            Err(HarnessError::Resume(ResumeRefusal::LimitsChanged))
        ),
        "{outcome:?}"
    );
}

#[test]
fn a_damaged_checkpoint_blob_is_refused_not_half_restored() {
    let c = crash_at("after_checkpoint");
    let state = candidate(&c.root).expect("safe");
    fs::write(
        blob_dir(&c.root.join("mol")).join(state.plan.log.digest.hex()),
        b"tampered",
    )
    .expect("tamper");
    assert!(matches!(candidate(&c.root), Err(ResumeRefusal::Corrupt(_))));
}

#[test]
fn a_molecule_without_evidence_has_nothing_to_resume() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("state")).expect("state");
    fs::create_dir_all(tmp.path().join("mol")).expect("mol");
    // No ledger at all, and an empty one, read the same way.
    assert!(matches!(
        candidate(tmp.path()),
        Err(ResumeRefusal::Corrupt(_) | ResumeRefusal::NothingToResume)
    ));
    fs::write(tmp.path().join("state/events.jsonl"), "").expect("empty ledger");
    assert_eq!(
        candidate(tmp.path()).err(),
        Some(ResumeRefusal::NothingToResume)
    );
}

#[test]
fn a_second_owner_of_the_same_molecule_loop_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let first = acquire_loop_owner(tmp.path()).expect("first owner");
    assert_eq!(
        acquire_loop_owner(tmp.path()).err(),
        Some(ResumeRefusal::ActiveOwner),
        "two resumes of one attempt would run its next tools twice"
    );
    drop(first);
    // Sibling tests fork crash children; a child inherits the descriptor until
    // its exec, so the release can lag by a moment. Wait for it, boundedly.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let released = loop {
        if acquire_loop_owner(tmp.path()).is_ok() {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(released, "ownership ends with the owner");
}

// ---------------------------------------------------------------------------
// The dispatch path: `cs tackle --force --resume`
// ---------------------------------------------------------------------------

const GATEWAY_MODEL: &str = "publisher-a/model-pro-preview";
const KEY_ENV: &str = "GATEWAY_API_KEY";
const FAKE_KEY: &str = "gw-test-key-not-a-secret";

/// How the loopback responder answers its Nth request.
#[derive(Clone, Copy)]
enum Reply {
    /// A `write_file` tool call for the named file.
    Write(&'static str),
    /// A normal stop with a final text.
    Stop,
    /// An HTTP 500, which fails the loop after the previous turns' effects.
    ServerError,
}

struct Gateway {
    origin: String,
    /// Request bodies, in arrival order.
    bodies: Arc<Mutex<Vec<String>>>,
    /// What the responder read from the galaxy ledger when each request arrived.
    ledger_rows: Arc<Mutex<Vec<Vec<String>>>>,
    _handle: std::thread::JoinHandle<()>,
}

impl Gateway {
    /// `ledger` is the exact path of the galaxy ledger file; the responder
    /// reads that one file and nothing else under the project.
    fn start(replies: Vec<Reply>, ledger: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback gateway");
        let addr = listener.local_addr().expect("gateway addr");
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let ledger_rows = Arc::new(Mutex::new(Vec::new()));
        let turns = Arc::new(AtomicUsize::new(0));
        let (b, l) = (Arc::clone(&bodies), Arc::clone(&ledger_rows));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let _ = serve_one(stream, &turns, &replies, &ledger, &b, &l);
            }
        });
        Self {
            origin: format!("http://{addr}"),
            bodies,
            ledger_rows,
            _handle: handle,
        }
    }
}

/// The `record` tags of the turn rows currently in the ledger file.
fn turn_record_tags(ledger: &Path) -> Vec<String> {
    let text = fs::read_to_string(ledger).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["type"] == "harness_turn_recorded")
        .filter_map(|v| {
            v["evidence"]["record"]["record"]
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

fn serve_one(
    mut stream: TcpStream,
    turns: &AtomicUsize,
    replies: &[Reply],
    ledger: &Path,
    bodies: &Mutex<Vec<String>>,
    ledger_rows: &Mutex<Vec<Vec<String>>>,
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
        if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    bodies
        .lock()
        .expect("bodies")
        .push(String::from_utf8_lossy(&body).into_owned());
    ledger_rows
        .lock()
        .expect("ledger rows")
        .push(turn_record_tags(ledger));
    let index = turns.fetch_add(1, Ordering::SeqCst);
    let reply = replies.get(index).copied().unwrap_or(Reply::Stop);
    let (status, payload) = match reply {
        Reply::Write(path) => {
            let arguments = serde_json::json!({ "path": path, "content": "x\n" }).to_string();
            (
                "200 OK",
                serde_json::json!({
                    "id": format!("cp-{index}"), "object": "chat.completion", "model": GATEWAY_MODEL,
                    "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
                        "role": "assistant", "content": null,
                        "tool_calls": [{ "id": format!("call_{index}"), "type": "function",
                            "function": { "name": "write_file", "arguments": arguments } }]
                    }}],
                    "usage": { "prompt_tokens": 90, "completion_tokens": 5, "total_tokens": 95 }
                })
                .to_string(),
            )
        }
        Reply::Stop => (
            "200 OK",
            serde_json::json!({
                "id": format!("cp-{index}"), "object": "chat.completion", "model": GATEWAY_MODEL,
                "choices": [{ "index": 0, "finish_reason": "stop",
                    "message": { "role": "assistant", "content": "done" } }],
                "usage": { "prompt_tokens": 120, "completion_tokens": 7, "total_tokens": 127 }
            })
            .to_string(),
        ),
        Reply::ServerError => (
            "500 Internal Server Error",
            serde_json::json!({ "error": { "message": "upstream unavailable" } }).to_string(),
        ),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
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

fn setup_project(dir: &Path, gateway: &Gateway) {
    let cosmon_dir = dir.join(".cosmon");
    fs::create_dir_all(cosmon_dir.join("state")).unwrap();
    fs::create_dir_all(cosmon_dir.join("formulas")).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        format!(
            "[project]\nproject_id = \"harness-resume-7d3a\"\n\n\
             [adapters.openai]\nbase_url = \"{}/api\"\napi_key_env = \"{KEY_ENV}\"\n\
             default_model = \"{GATEWAY_MODEL}\"\n",
            gateway.origin
        ),
    )
    .unwrap();
    let formula_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(".cosmon/formulas/task-work.formula.toml"))
        .expect("workspace root");
    fs::write(
        cosmon_dir.join("formulas").join("task-work.formula.toml"),
        fs::read_to_string(&formula_src).expect("read task-work formula"),
    )
    .unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join(".gitignore"), ".cosmon/state/\n").unwrap();
    fs::write(dir.join("README.md"), "# harness-resume\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

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
        .env(
            "COSMON_CONFIG_HOME",
            std::env::temp_dir().join("cosmon-test-xdg-isolated-harness-resume"),
        )
        .env("GIT_AUTHOR_NAME", "cosmon-test")
        .env("GIT_AUTHOR_EMAIL", "test@cosmon.test")
        .env("GIT_COMMITTER_NAME", "cosmon-test")
        .env("GIT_COMMITTER_EMAIL", "test@cosmon.test");
    cmd
}

fn nucleate(project: &Path, topic: &str) -> String {
    let out = cs(project)
        .args([
            "nucleate",
            "task-work",
            "--json",
            "--no-parent",
            "--var",
            &format!("topic={topic}"),
        ])
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_owned))
        .expect("molecule id in nucleate output")
}

fn tackle(project: &Path, mol: &str, extra: &[&str]) -> std::process::Output {
    cs(project)
        .env(KEY_ENV, FAKE_KEY)
        .args(["tackle", mol, "--adapter", "openai"])
        .args(extra)
        .output()
        .expect("spawn cs tackle")
}

fn molecule_dir(project: &Path, mol: &str) -> PathBuf {
    project
        .join(".cosmon/state/fleets/default/molecules")
        .join(mol)
}

/// A crash leaves the molecule with a dead worker, not collapsed. The loop
/// failure path collapses it, so put it where a crash would have left it.
fn leave_as_a_crash_would(project: &Path, mol: &str) {
    let path = molecule_dir(project, mol).join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("state.json")).expect("json");
    state["status"] = serde_json::json!("frozen");
    fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).expect("write state");
}

fn requests_of(gateway: &Gateway) -> Vec<String> {
    gateway.bodies.lock().expect("bodies").clone()
}

/// The `messages` array of a request body with the molecule's own id masked,
/// so two molecules of one project compare on what the loop sent.
fn messages_of(body: &str, mol: &str) -> serde_json::Value {
    let masked = body.replace(mol, "<mol>");
    let parsed: serde_json::Value = serde_json::from_str(&masked).expect("request json");
    parsed["messages"].clone()
}

#[test]
fn cs_tackle_resume_continues_an_interrupted_attempt_without_repeating_or_refreshing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let ledger = project.join(".cosmon/state/events.jsonl");

    // Reference: one uninterrupted run, two requests.
    let reference_gateway =
        Gateway::start(vec![Reply::Write("proof.txt"), Reply::Stop], ledger.clone());
    setup_project(project, &reference_gateway);
    let reference_mol = nucleate(project, "Create proof.txt");
    let out = tackle(project, &reference_mol, &[]);
    assert!(
        out.status.success(),
        "reference run:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let reference = requests_of(&reference_gateway);
    assert_eq!(reference.len(), 2);

    // The interrupted run: the first turn lands its effect and checkpoint, the
    // second request fails, so the attempt ends with no terminal record.
    // The provider retries a failed request, so the error repeats.
    let mut failing = vec![Reply::Write("proof.txt")];
    failing.extend(std::iter::repeat(Reply::ServerError).take(8));
    let gateway = Gateway::start(failing, ledger.clone());
    fs::write(
        project.join(".cosmon/config.toml"),
        fs::read_to_string(project.join(".cosmon/config.toml"))
            .unwrap()
            .replace(&reference_gateway.origin, &gateway.origin),
    )
    .unwrap();
    let mol = nucleate(project, "Create proof.txt");
    let out = tackle(project, &mol, &[]);
    assert!(!out.status.success(), "the second request fails the loop");
    let worktree = project.join(".worktrees").join(&mol);
    assert_eq!(
        fs::read_to_string(worktree.join("proof.txt"))
            .ok()
            .as_deref(),
        Some("x\n"),
        "the first turn's effect landed"
    );
    leave_as_a_crash_would(project, &mol);

    // Resume against a responder that finishes. It reads the galaxy ledger
    // file at its exact path when the request arrives.
    let resumed_gateway = Gateway::start(vec![Reply::Stop], ledger.clone());
    fs::write(
        project.join(".cosmon/config.toml"),
        fs::read_to_string(project.join(".cosmon/config.toml"))
            .unwrap()
            .replace(&gateway.origin, &resumed_gateway.origin),
    )
    .unwrap();
    let out = tackle(project, &mol, &["--force", "--resume"]);
    assert!(
        out.status.success(),
        "resume:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // One request, and it is the one the uninterrupted run sent as its second.
    let sent = requests_of(&resumed_gateway);
    assert_eq!(sent.len(), 1, "the first turn's request was not paid twice");
    assert_eq!(
        messages_of(&sent[0], &mol),
        messages_of(&reference[1], &reference_mol),
        "the continuation's request equals the uninterrupted run's second request"
    );
    // The effect was not replayed.
    assert_eq!(
        fs::read_to_string(worktree.join("proof.txt"))
            .ok()
            .as_deref(),
        Some("x\n")
    );

    // The continuation was on the ledger before its request left the process.
    let at_request = resumed_gateway.ledger_rows.lock().expect("rows")[0].clone();
    assert!(
        at_request.iter().any(|t| t == "resumed"),
        "the resumed record is durable before the request: {at_request:?}"
    );
    assert_eq!(
        at_request.iter().filter(|t| *t == "resumed").count(),
        1,
        "exactly one continuation"
    );
}

#[test]
fn cs_tackle_resume_refuses_a_killed_request_instead_of_paying_for_it_again() {
    // A kill after a request was sent and before its response was handled leaves
    // a request intent with no outcome. Run to the end, then remove the terminal
    // record, which is exactly the ledger such a kill leaves behind.
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let ledger = project.join(".cosmon/state/events.jsonl");
    let gateway = Gateway::start(vec![Reply::Write("proof.txt"), Reply::Stop], ledger.clone());
    setup_project(project, &gateway);
    let mol = nucleate(project, "Create proof.txt");
    let out = tackle(project, &mol, &[]);
    assert!(out.status.success());

    let text = fs::read_to_string(&ledger).expect("ledger");
    let mut kept = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
        if v["type"] == "harness_turn_recorded" && v["evidence"]["record"]["record"] == "terminal" {
            continue;
        }
        kept.push(line.to_owned());
    }
    fs::write(&ledger, kept.join("\n") + "\n").expect("rewrite ledger");
    leave_as_a_crash_would(project, &mol);

    let before = requests_of(&gateway).len();
    let out = tackle(project, &mol, &["--force", "--resume"]);
    assert!(!out.status.success(), "the unresolved request is refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no recorded outcome") && stderr.contains("billed"),
        "the refusal says why: {stderr}"
    );
    assert_eq!(
        requests_of(&gateway).len(),
        before,
        "nothing was sent: a request that may have been billed is not sent again"
    );
}
