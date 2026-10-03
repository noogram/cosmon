// SPDX-License-Identifier: AGPL-3.0-only

//! Turn and effect evidence is durable: what a killed worker had requested,
//! received and run can be rebuilt from disk alone, and a record that cannot be
//! written stops the loop before the effect it announces.
//!
//! Two kinds of test live here.
//!
//! - **Crash seams.** The test binary re-executes itself as a child that runs
//!   the real loop over the real native message log with a scripted model, and
//!   exits abruptly at one deterministic seam. The parent then reconstructs
//!   the attempt from the ledger and the blob directory and asserts what
//!   survived. The three seams are: right after a valid assistant envelope is
//!   durable, after a tool has run but before its receipt, and after a complete
//!   tool-result boundary.
//! - **The dispatch path.** `cs tackle` runs a real worker against a loopback
//!   responder, and the evidence is read back from the galaxy ledger. This is
//!   what proves `cs tackle` writes it, not only the library.
//!
//! Every network address is loopback and every credential is a fabricated
//! literal.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cosmon_agent_harness::spine::{
    run_loop_counted_with_progress_budgeted,
    run_loop_counted_with_turn_input_and_progress_budgeted, LoopProgress, ScriptedProviderFn,
    TerminalDisposition, TerminalResponse, Turn, TurnInput, TurnInputOutcome, TurnInputSource,
};
use cosmon_agent_harness::tool::ToolCall;
use cosmon_agent_harness::{HarnessError, LoopBudget, TurnJournal};
use cosmon_core::event_v2::EventV2;
use cosmon_core::harness_turn::{
    BlobDigest, BlobKind, BlobRef, EvidenceError, TerminalKind, TurnEvidenceStore, TurnRecord,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_provider::openai::{ChatMessage, OpenAILog};
use cosmon_state::harness_checkpoint::{load_attempt, read_blob, FileTurnEvidenceStore};

const HISTORY: &str = "harness/mol/worker-1/attempt-1";
const MARKER: &str = "marker.txt";
const PEER_KEY: &str = "peer-1";
const PEER_TEXT: &str = "peer evidence block";

fn mol_id() -> MoleculeId {
    MoleculeId::new("task-20261003-aaaa").expect("molecule id")
}

fn file_store(state: &Path, mol_dir: &Path) -> FileTurnEvidenceStore {
    FileTurnEvidenceStore::new(
        state,
        mol_dir,
        mol_id(),
        WorkerId::new("worker-1").expect("worker id"),
        HISTORY,
    )
}

// ---------------------------------------------------------------------------
// The crash child
// ---------------------------------------------------------------------------

/// Store that exits the process abruptly at a chosen seam, to model a kill.
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
        // The result blob is written after the tool body has run and before its
        // receipt, so a crash here is "effect done, receipt missing".
        if self.seam == "tool_effect_without_receipt" && kind == BlobKind::ToolResult {
            std::process::exit(137);
        }
        self.inner.put_blob(kind, bytes)
    }

    fn append(&self, record: TurnRecord) -> Result<(), EvidenceError> {
        let kind = kind_of(&record);
        self.inner.append(record)?;
        let crash_after = match self.seam.as_str() {
            "after_assistant" => "assistant_received",
            "after_checkpoint" => "checkpoint",
            _ => "",
        };
        if kind == crash_after {
            std::process::exit(137);
        }
        Ok(())
    }
}

struct PeerInput {
    taken: Mutex<bool>,
}

impl TurnInputSource for PeerInput {
    fn take(&self) -> Result<Vec<TurnInput>, String> {
        let mut taken = self.taken.lock().expect("peer lock");
        if *taken {
            return Ok(Vec::new());
        }
        *taken = true;
        Ok(vec![TurnInput {
            key: PEER_KEY.to_owned(),
            content: PEER_TEXT.to_owned(),
        }])
    }

    fn record(&self, _inputs: &[TurnInput], _outcome: TurnInputOutcome) -> Result<(), String> {
        Ok(())
    }
}

fn write_file_turn() -> Turn<OpenAILog> {
    let arguments = serde_json::json!({ "path": MARKER, "content": "effect\n" }).to_string();
    let assistant: ChatMessage = serde_json::from_value(serde_json::json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "call-1",
            "type": "function",
            "function": { "name": "write_file", "arguments": arguments }
        }]
    }))
    .expect("native assistant envelope");
    Turn::ToolCalls {
        assistant,
        calls: vec![ToolCall::new("call-1", "write_file", arguments)],
    }
}

/// Body of the crash child. It does nothing unless the parent set the seam, so
/// a plain `cargo test` run passes through it without effect.
#[test]
fn crash_child_body() {
    let Ok(seam) = std::env::var("COSMON_W8_CRASH_SEAM") else {
        return;
    };
    let root = PathBuf::from(std::env::var("COSMON_W8_ROOT").expect("root"));
    let store = CrashingStore {
        inner: file_store(&root.join("state"), &root.join("mol")),
        seam,
    };
    let mut progress =
        LoopProgress::default().with_journal(Arc::new(TurnJournal::new(Arc::new(store))));
    let turns = AtomicUsize::new(0);
    let provider = ScriptedProviderFn::<OpenAILog, std::io::Error>::new(move |_log| {
        if turns.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(write_file_turn())
        } else {
            Ok(Turn::Stop("finished".to_owned()))
        }
    });
    let source = PeerInput {
        taken: Mutex::new(false),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime.block_on(run_loop_counted_with_turn_input_and_progress_budgeted(
        &provider,
        "brief",
        &root.join("work"),
        None,
        &source,
        &mut progress,
        LoopBudget::DEFAULT,
    ));
    // Reaching this line means the seam never fired.
    std::process::exit(if result.is_ok() { 3 } else { 4 });
}

struct Crashed {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Crashed {
    fn state(&self) -> PathBuf {
        self.root.join("state")
    }
    fn mol(&self) -> PathBuf {
        self.root.join("mol")
    }
    fn marker(&self) -> PathBuf {
        self.root.join("work").join(MARKER)
    }
}

/// Run the child to the seam and require that it was killed there.
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
        .env("COSMON_W8_CRASH_SEAM", seam)
        .env("COSMON_W8_ROOT", &root)
        .status()
        .expect("spawn crash child");
    assert_eq!(
        status.code(),
        Some(137),
        "the child must have been killed at the {seam} seam, not finished or failed"
    );
    Crashed { _tmp: tmp, root }
}

fn read_text(mol_dir: &Path, reference: &BlobRef) -> String {
    String::from_utf8(read_blob(mol_dir, reference).expect("blob validates")).expect("utf8 blob")
}

#[test]
fn a_kill_after_the_assistant_envelope_keeps_the_call_identity_inputs_and_budgets() {
    let c = crash_at("after_assistant");
    let loaded = load_attempt(&c.state(), &c.mol(), &mol_id(), HISTORY).expect("load from disk");
    assert!(
        loaded.is_intact(),
        "every blob validates: {:?}",
        loaded.blob_problems
    );
    let r = &loaded.reconstruction;
    assert_eq!(r.requests_sent, 1);
    assert_eq!(r.tools_spent, 0);
    assert!(
        r.unresolved_requests.is_empty(),
        "the response resolved the request"
    );
    assert!(r.unresolved_calls.is_empty(), "no tool had started");
    assert!(!c.marker().exists(), "no effect had happened");
    assert_eq!(
        r.limits.map(|l| l.max_tool_calls),
        Some(LoopBudget::DEFAULT.tools.max_tool_calls)
    );
    assert!(r.pins.contains_key("registry") && r.pins.contains_key("briefing"));

    // Native call identity: the stored envelope is the provider's own message.
    let assistant = &r.assistants[0];
    assert_eq!(assistant.calls[0].call_id, "call-1");
    let envelope = assistant.envelope.as_ref().expect("envelope stored");
    let native: ChatMessage =
        serde_json::from_slice(&read_blob(&c.mol(), envelope).expect("blob")).expect("native");
    let native = serde_json::to_value(native).expect("json");
    assert_eq!(native["tool_calls"][0]["id"], "call-1");
    assert_eq!(native["tool_calls"][0]["function"]["name"], "write_file");

    // Input keys: the peer block's key and digest, not its text.
    assert_eq!(r.inputs.len(), 1);
    assert_eq!(r.inputs[0].key, PEER_KEY);
    assert_eq!(r.inputs[0].digest, BlobDigest::of(PEER_TEXT.as_bytes()));
}

#[test]
fn a_kill_after_a_tool_effect_but_before_its_receipt_leaves_an_unresolved_effect() {
    let c = crash_at("tool_effect_without_receipt");
    assert!(c.marker().exists(), "the tool body had run before the kill");
    let loaded = load_attempt(&c.state(), &c.mol(), &mol_id(), HISTORY).expect("load from disk");
    assert!(loaded.is_intact());
    let r = &loaded.reconstruction;
    assert_eq!(r.tools_spent, 1);
    assert!(r.completed_calls.is_empty(), "no receipt was written");
    assert_eq!(r.unresolved_calls.len(), 1);
    assert_eq!(r.unresolved_calls[0].call_id, "call-1");
    assert_eq!(r.unresolved_calls[0].tool, "write_file");
    assert!(r.last_checkpoint.is_none(), "the turn never completed");
}

#[test]
fn a_kill_after_a_complete_tool_result_boundary_keeps_the_receipt_and_the_native_log() {
    let c = crash_at("after_checkpoint");
    assert!(c.marker().exists());
    let loaded = load_attempt(&c.state(), &c.mol(), &mol_id(), HISTORY).expect("load from disk");
    assert!(loaded.is_intact());
    let r = &loaded.reconstruction;
    assert_eq!(r.tools_spent, 1);
    assert!(r.unresolved_calls.is_empty());
    assert_eq!(r.completed_calls.len(), 1);
    let call = &r.completed_calls[0];
    assert_eq!(call.call_id, "call-1");
    let result = read_text(&c.mol(), call.result.as_ref().expect("result stored"));
    assert!(
        result.contains(MARKER),
        "the stored result is the tool's output: {result}"
    );

    let checkpoint = r.last_checkpoint.as_ref().expect("checkpoint");
    assert_eq!(checkpoint.tools_spent, 1);
    let log: serde_json::Value = serde_json::from_slice(
        &read_blob(&c.mol(), checkpoint.log.as_ref().expect("log stored")).expect("blob"),
    )
    .expect("native log json");
    let messages = log.as_array().expect("native messages");
    assert!(messages
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call-1"));
    assert!(messages
        .iter()
        .any(|m| m["role"] == "assistant" && m["tool_calls"][0]["id"] == "call-1"));
}

// ---------------------------------------------------------------------------
// Persistence failure and terminal evidence, in process
// ---------------------------------------------------------------------------

/// Store that refuses one record kind.
struct FailingStore {
    inner: FileTurnEvidenceStore,
    refuse: &'static str,
}

impl TurnEvidenceStore for FailingStore {
    fn put_blob(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobRef, EvidenceError> {
        self.inner.put_blob(kind, bytes)
    }

    fn append(&self, record: TurnRecord) -> Result<(), EvidenceError> {
        if kind_of(&record) == self.refuse {
            return Err(EvidenceError::Io(format!(
                "injected failure on {}",
                self.refuse
            )));
        }
        self.inner.append(record)
    }
}

fn run_scripted(root: &Path, store: Arc<dyn TurnEvidenceStore>) -> Result<(), String> {
    let mut progress = LoopProgress::default().with_journal(Arc::new(TurnJournal::new(store)));
    let turns = AtomicUsize::new(0);
    let provider = ScriptedProviderFn::<OpenAILog, std::io::Error>::new(move |_log| {
        if turns.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(write_file_turn())
        } else {
            Ok(Turn::Stop("finished".to_owned()))
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime
        .block_on(run_loop_counted_with_progress_budgeted(
            &provider,
            "brief",
            &root.join("work"),
            None,
            &mut progress,
            LoopBudget::DEFAULT,
        ))
        .map(|_| ())
        .map_err(|e| match e {
            HarnessError::Evidence(inner) => format!("evidence: {inner}"),
            other => other.to_string(),
        })
}

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    for dir in ["state", "mol", "work"] {
        fs::create_dir_all(root.join(dir)).expect("dir");
    }
    (tmp, root)
}

#[test]
fn a_tool_intent_that_cannot_be_written_stops_the_loop_before_the_effect() {
    let (_tmp, root) = scratch();
    let store = FailingStore {
        inner: file_store(&root.join("state"), &root.join("mol")),
        refuse: "tool_intent",
    };
    let err = run_scripted(&root, Arc::new(store)).expect_err("the loop must stop");
    assert!(err.starts_with("evidence:"), "{err}");
    assert!(
        !root.join("work").join(MARKER).exists(),
        "the effect must not happen"
    );
    let loaded = load_attempt(&root.join("state"), &root.join("mol"), &mol_id(), HISTORY)
        .expect("what was written is still readable");
    assert_eq!(loaded.reconstruction.assistants.len(), 1);
    assert!(loaded.reconstruction.unresolved_calls.is_empty());
}

#[test]
fn an_output_limited_response_keeps_its_partial_text_and_reason_on_disk() {
    let (_tmp, root) = scratch();
    let store = Arc::new(file_store(&root.join("state"), &root.join("mol")));
    let mut progress = LoopProgress::default().with_journal(Arc::new(TurnJournal::new(store)));
    let provider = ScriptedProviderFn::<OpenAILog, std::io::Error>::new(|_log| {
        Ok(Turn::Terminal(TerminalResponse::new(
            "the answer was cut o",
            TerminalDisposition::OutputLimit,
        )))
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime
        .block_on(run_loop_counted_with_progress_budgeted(
            &provider,
            "brief",
            &root.join("work"),
            None,
            &mut progress,
            LoopBudget::DEFAULT,
        ))
        .expect("a limited response is a terminal, not an error");
    let loaded = load_attempt(&root.join("state"), &root.join("mol"), &mol_id(), HISTORY)
        .expect("load from disk");
    let terminal = loaded.reconstruction.terminal.expect("terminal recorded");
    assert_eq!(terminal.disposition, TerminalKind::OutputLimit);
    assert_eq!(
        read_text(
            &root.join("mol"),
            terminal.partial_text.as_ref().expect("text stored")
        ),
        "the answer was cut o"
    );
}

#[test]
fn a_corrupted_blob_is_reported_and_the_rest_of_the_attempt_is_retained() {
    let (_tmp, root) = scratch();
    let store = Arc::new(file_store(&root.join("state"), &root.join("mol")));
    run_scripted(&root, store).expect("loop completes");
    let loaded =
        load_attempt(&root.join("state"), &root.join("mol"), &mol_id(), HISTORY).expect("load");
    assert!(loaded.is_intact());
    let result = loaded.reconstruction.completed_calls[0]
        .result
        .clone()
        .expect("stored");
    let path =
        cosmon_state::harness_checkpoint::blob_dir(&root.join("mol")).join(result.digest.hex());
    fs::write(path, b"tampered").expect("tamper");
    let after = load_attempt(&root.join("state"), &root.join("mol"), &mol_id(), HISTORY)
        .expect("still loads");
    assert!(!after.is_intact(), "the tampered blob must be reported");
    assert!(after.blob_problems.iter().all(|(r, _)| *r == result));
    assert_eq!(
        after.reconstruction.completed_calls.len(),
        1,
        "the records survive"
    );
}

// ---------------------------------------------------------------------------
// The dispatch path: `cs tackle` writes the evidence
// ---------------------------------------------------------------------------

const GATEWAY_MODEL: &str = "publisher-a/model-pro-preview";
const KEY_ENV: &str = "GATEWAY_API_KEY";
const FAKE_KEY: &str = "gw-test-key-not-a-secret";
const DELIVERABLE: &str = "checkpoint-proof.txt";
const DELIVERABLE_TEXT: &str = "persisted by the dispatch path\n";
const FINAL_TEXT: &str = "created checkpoint-proof.txt";

struct Gateway {
    origin: String,
    _handle: std::thread::JoinHandle<()>,
}

impl Gateway {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback gateway");
        let addr = listener.local_addr().expect("gateway addr");
        let turns = Arc::new(AtomicUsize::new(0));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let turns = Arc::clone(&turns);
                std::thread::spawn(move || {
                    let _ = serve_one(stream, &turns);
                });
            }
        });
        Self {
            origin: format!("http://{addr}"),
            _handle: handle,
        }
    }
}

fn serve_one(mut stream: TcpStream, turns: &AtomicUsize) -> std::io::Result<()> {
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
    let payload = if turns.fetch_add(1, Ordering::SeqCst) == 0 {
        let arguments =
            serde_json::json!({ "path": DELIVERABLE, "content": DELIVERABLE_TEXT }).to_string();
        serde_json::json!({
            "id": "cp-1", "object": "chat.completion", "model": GATEWAY_MODEL,
            "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{ "id": "call_1", "type": "function",
                    "function": { "name": "write_file", "arguments": arguments } }]
            }}],
            "usage": { "prompt_tokens": 90, "completion_tokens": 5, "total_tokens": 95 }
        })
    } else {
        serde_json::json!({
            "id": "cp-2", "object": "chat.completion", "model": GATEWAY_MODEL,
            "choices": [{ "index": 0, "finish_reason": "stop",
                "message": { "role": "assistant", "content": FINAL_TEXT } }],
            "usage": { "prompt_tokens": 120, "completion_tokens": 7, "total_tokens": 127 }
        })
    }
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
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
            "[project]\nproject_id = \"harness-checkpoint-4c1e\"\n\n\
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
    fs::write(dir.join("README.md"), "# harness-checkpoint\n").unwrap();
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
            std::env::temp_dir().join("cosmon-test-xdg-isolated-harness-checkpoint"),
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
            "topic=Create checkpoint-proof.txt",
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

#[test]
fn cs_tackle_persists_turn_and_effect_evidence_to_the_ledger_and_the_molecule_directory() {
    let gateway = Gateway::start();
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    setup_project(project, &gateway);
    let mol_id = nucleate(project);

    let out = cs(project)
        .env(KEY_ENV, FAKE_KEY)
        .args(["tackle", &mol_id, "--adapter", "openai"])
        .output()
        .expect("spawn cs tackle");
    assert!(
        out.status.success(),
        "cs tackle failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(project.join(".worktrees").join(&mol_id).join(DELIVERABLE))
            .ok()
            .as_deref(),
        Some(DELIVERABLE_TEXT),
        "the tool call was executed"
    );

    // The galaxy ledger holds the turn records, in order, for one attempt.
    let state = project.join(".cosmon").join("state");
    let mol = MoleculeId::new(&mol_id).expect("molecule id");
    let mol_dir = state
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(&mol_id);
    let envelopes =
        cosmon_state::event_log::read_all(cosmon_state::event_log::resolve_events_log_path(&state))
            .expect("ledger readable");
    let mut histories = BTreeSet::new();
    let mut kinds = Vec::new();
    for envelope in &envelopes {
        if let EventV2::HarnessTurnRecorded {
            mol_id: m,
            evidence,
        } = &envelope.event
        {
            assert_eq!(*m, mol);
            histories.insert(evidence.history_id.clone());
            kinds.push(kind_of(&evidence.record));
        }
    }
    assert_eq!(
        histories.len(),
        1,
        "one worker attempt, one history: {histories:?}"
    );
    assert_eq!(
        kinds,
        [
            "attempt_started",
            "request_intent",
            "assistant_received",
            "tool_intent",
            "tool_receipt",
            "checkpoint",
            "request_intent",
            "terminal",
        ],
        "the dispatch path wrote the intent/receipt chain"
    );
    let history = histories.into_iter().next().expect("one history");

    // Reconstruct from disk alone and validate every blob.
    let loaded = load_attempt(&state, &mol_dir, &mol, &history).expect("load from disk");
    assert!(loaded.is_intact(), "{:?}", loaded.blob_problems);
    let r = &loaded.reconstruction;
    assert_eq!(r.requests_sent, 2);
    assert_eq!(r.tools_spent, 1);
    assert!(r.unresolved_calls.is_empty() && r.unresolved_requests.is_empty());
    assert_eq!(
        r.pins.get("requested_model").map(String::as_str),
        Some(GATEWAY_MODEL)
    );
    assert_eq!(r.pins.get("adapter").map(String::as_str), Some("openai"));
    assert_eq!(r.completed_calls[0].call_id, "call_1");
    let terminal = r.terminal.as_ref().expect("terminal");
    assert_eq!(terminal.disposition, TerminalKind::Normal);
    assert_eq!(
        read_text(
            &mol_dir,
            terminal.partial_text.as_ref().expect("final text stored")
        ),
        FINAL_TEXT
    );
    let native: ChatMessage = serde_json::from_slice(
        &read_blob(
            &mol_dir,
            r.assistants[0].envelope.as_ref().expect("envelope"),
        )
        .expect("blob"),
    )
    .expect("native envelope");
    assert_eq!(
        serde_json::to_value(native).unwrap()["tool_calls"][0]["id"],
        "call_1"
    );

    // Usage and turn evidence name the same attempt, in the same ledger.
    assert!(
        !loaded.usage_observation_ids.is_empty()
            && loaded
                .usage_observation_ids
                .iter()
                .all(|id| id.starts_with(&history)),
        "usage observations share the attempt's history id: {:?}",
        loaded.usage_observation_ids
    );

    // The molecule journal projects the rows without raw private context.
    let journal = cosmon_state::journal::MoleculeJournal::project_from_state_dir(&state, &mol)
        .expect("journal projects");
    let rendered = journal.render_jsonl();
    assert_eq!(
        journal
            .entries
            .iter()
            .filter(|e| e.event_type == "harness_turn_recorded")
            .count(),
        kinds.len()
    );
    for private in [
        DELIVERABLE_TEXT.trim_end(),
        FINAL_TEXT,
        "Create checkpoint-proof.txt",
    ] {
        assert!(
            !rendered.contains(private),
            "the ledger projection must not carry raw content: {private}"
        );
    }
}
