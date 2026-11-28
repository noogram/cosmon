// SPDX-License-Identifier: AGPL-3.0-only

//! An agy worker through the built `cs` binary with a fake `agy` (#240, #245).
//!
//! The fake speaks agy's multi-turn print mode as measured on agy 1.5.19
//! (`--input-format stream-json`): one `init`, then one turn per NDJSON line on
//! stdin, each ending with a `result` whose usage is cumulative; it exits when
//! stdin closes. It records its argv and every stdin line, so a test can see
//! exactly what reached the process.
//!
//! The first group drives `cs agy-pane` directly with a pipe for a terminal.
//! The last test runs the whole path in an isolated tmux server: `cs tackle
//! --adapter agy --harness effort=high`, the readable pane, `cs whisper`, and
//! the usage `cs peek` reads afterwards.

#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use cosmon_core::event_v2::{AdapterSelectionSource, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
use serde_json::{json, Value};

const QUOTA: &str = include_str!("fixtures/agy/quota.jsonl");

/// Write a fake `agy` into `bin`. It records under `record`; with `quota` it
/// answers the first message with that receipt and exits 3 at once, as agy
/// does on a quota refusal.
fn fake_agy(bin: &Path, record: &Path, exit_code: i32, quota: Option<&Path>) -> PathBuf {
    fs::create_dir_all(bin).expect("bin dir");
    fs::create_dir_all(record).expect("record dir");
    let quota_branch = quota.map_or_else(String::new, |q| {
        format!("  cat '{}'; exit 3\n", q.display())
    });
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = --version ]; then printf '1.5.19-fake\n'; exit 0; fi
printf '%s\0' "$@" > '{rec}/argv'
printf '%s\n' '{{"event":"init","conversation_id":"fake-1","init":{{"model":"fake-model","tools":["read_file"]}}}}'
n=0
while IFS= read -r line; do
  n=$((n+1))
  printf '%s\n' "$line" >> '{rec}/stdin'
{quota_branch}  printf '{{"event":"step_update","step_update":{{"state":"ACTIVE","step_type":"tool","tool_name":"read_file","tool_info":{{"name":"read_file","parameters":{{"Path":"/w/f%d.rs"}}}}}}}}\n' "$n"
  printf '{{"event":"step_update","step_update":{{"state":"ACTIVE","step_type":"agent_response","text_delta":"reply %d"}}}}\n' "$n"
  printf '{{"event":"step_update","step_update":{{"state":"DONE","step_type":"agent_response","text_delta":""}}}}\n'
  printf '{{"event":"result","result":{{"conversation_id":"fake-1","status":"SUCCESS","num_turns":%d,"usage":{{"input_tokens":%d,"output_tokens":%d,"thinking_tokens":0,"cache_read_tokens":%d}}}}}}\n' "$n" $((n*100)) $((n*10)) $((n*1000))
done
echo "fake agy: input closed" >&2
exit {exit_code}
"#,
        rec = record.display(),
    );
    let path = bin.join("agy");
    fs::write(&path, script).expect("fake agy");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("executable");
    path
}

fn stdin_messages(record: &Path) -> Vec<Value> {
    fs::read_to_string(record.join("stdin"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("each stdin line is one JSON message"))
        .collect()
}

fn argv(record: &Path) -> Vec<String> {
    fs::read(record.join("argv"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|w| !w.is_empty())
        .map(|w| String::from_utf8_lossy(w).into_owned())
        .collect()
}

fn wait_for(limit: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    predicate()
}

/// A molecule `cs peek` lists as an agy worker, so the receipt the driver
/// writes into its directory is read the way #241 reads it.
fn seed_molecule(root: &Path) -> PathBuf {
    let state = root.join(".cosmon/state");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        root.join(".cosmon/config.toml"),
        "[project]\nproject_id = \"agy-pane-test\"\n",
    )
    .unwrap();
    let store = FileStore::new(&state);
    let id = MoleculeId::new("task-20261128-a240").unwrap();
    let worker_id = WorkerId::new("worker-agy-pane").unwrap();
    let now = Utc::now();
    let worker: WorkerData = serde_json::from_value(json!({
        "id": worker_id, "agent_id": "agent-test", "role": "implementation",
        "clearance": "execute", "status": "stopped", "current_molecule": id, "updated_at": now
    }))
    .unwrap();
    let molecule: MoleculeData = serde_json::from_value(json!({
        "id": id, "formula_id": "task-work", "status": "running", "variables": {},
        "assigned_worker": worker_id, "created_at": now, "updated_at": now,
        "total_steps": 1, "current_step": 0, "completed_steps": [], "links": [],
        "project_id": "agy-pane-test"
    }))
    .unwrap();
    let mut fleet = Fleet::new();
    fleet.workers.insert(worker_id, worker);
    store.save_fleet(&fleet).unwrap();
    store.save_molecule(&id, &molecule).unwrap();
    cosmon_state::event_log::emit_one(
        &cosmon_state::event_log::resolve_events_log_path(&state),
        EventV2::AdapterSelected {
            mol_id: id.clone(),
            adapter_name: "agy".to_owned(),
            selected_at: now,
            selection_source: AdapterSelectionSource::Cli {
                flag: "agy".to_owned(),
            },
            role_hint: None,
            loop_ownership: Default::default(),
        },
        None,
    )
    .unwrap();
    store.molecule_dir(&id)
}

fn peek_json(root: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env("NO_COLOR", "1")
        .args(["--json", "peek"])
        .output()
        .expect("cs peek");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("peek JSON")
}

fn agy_pane(mol_dir: &Path, prompt: &str, agy: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.args(["agy-pane", "--molecule-dir"])
        .arg(mol_dir)
        .args(["--prompt", prompt, "--"])
        .arg(agy)
        .args(["--input-format", "stream-json", "-p", ""]);
    cmd
}

/// The session stays open after the brief: a later terminal line is the next
/// turn, blank lines are not sent, the receipt is agy's raw output, the pane
/// output is readable, and the exit status is agy's, in the pane and on disk.
#[test]
fn a_driven_session_takes_further_turns_and_keeps_the_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let mol_dir = seed_molecule(tmp.path());
    let record = tmp.path().join("record");
    let agy = fake_agy(&tmp.path().join("bin"), &record, 5, None);
    let brief = "Work on it's \"brief\"\nsecond line";

    let mut child = agy_pane(&mol_dir, brief, &agy)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut terminal = child.stdin.take().unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || stdin_messages(&record).len()
            == 1),
        "the brief reaches agy as the first message"
    );
    terminal.write_all(b"\n   \ncontinue please\n").unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || stdin_messages(&record).len()
            == 2),
        "a terminal line after the first turn reaches the open session"
    );
    drop(terminal);
    let out: Output = child.wait_with_output().unwrap();

    assert_eq!(out.status.code(), Some(5), "the exit status is agy's");
    assert_eq!(
        fs::read_to_string(mol_dir.join("agy-exit.code")).unwrap(),
        "5\n"
    );
    let messages = stdin_messages(&record);
    assert_eq!(messages.len(), 2, "blank lines are not sent: {messages:?}");
    assert_eq!(
        messages[0],
        json!({"event":"user","message":{"content": brief}})
    );
    assert_eq!(
        messages[1],
        json!({"event":"user","message":{"content":"continue please"}})
    );

    let receipt = fs::read_to_string(mol_dir.join("agy-stream.jsonl")).unwrap();
    assert!(receipt.starts_with(r#"{"event":"init","conversation_id":"fake-1""#));
    assert_eq!(
        receipt.matches(r#"{"event":"result""#).count(),
        2,
        "{receipt}"
    );
    assert!(
        receipt.contains("fake agy: input closed\n"),
        "stderr is kept: {receipt}"
    );

    let shown = String::from_utf8(out.stdout).unwrap();
    assert!(
        !shown.contains("{\"event\""),
        "the pane shows no raw frame: {shown}"
    );
    for expected in [
        "agy session fake-1 · model fake-model\n",
        "→ read_file /w/f1.rs\n",
        "reply 1\n",
        "── turn 1 SUCCESS · tokens in 100 cache 1000 out 10 thinking 0\n",
        "reply 2\n",
        "── turn 2 SUCCESS · tokens in 200 cache 2000 out 20 thinking 0\n",
        "fake agy: input closed\n",
    ] {
        assert!(shown.contains(expected), "missing {expected:?} in {shown}");
    }

    let doc = peek_json(tmp.path());
    assert_eq!(
        doc["molecules"][0]["agy_usage"]["input_tokens"], 200,
        "usage is the latest cumulative result: {doc}"
    );
}

/// agy ending on its own (a quota refusal) ends the driver at once, though
/// the terminal is still open, and the quota stop is read from the receipt.
#[test]
fn a_quota_stop_ends_the_session_and_is_read_from_the_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let mol_dir = seed_molecule(tmp.path());
    let record = tmp.path().join("record");
    let quota = tmp.path().join("quota.jsonl");
    fs::write(&quota, QUOTA).unwrap();
    let agy = fake_agy(&tmp.path().join("bin"), &record, 0, Some(&quota));

    let mut child = agy_pane(&mol_dir, "brief", &agy)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _terminal_kept_open = child.stdin.take().unwrap();
    let ended = wait_for(Duration::from_secs(10), || {
        child.try_wait().ok().flatten().is_some()
    });
    if !ended {
        let _ = child.kill();
    }
    assert!(ended, "the driver must end when agy ends");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        fs::read_to_string(mol_dir.join("agy-exit.code")).unwrap(),
        "3\n"
    );
    let shown = String::from_utf8(out.stdout).unwrap();
    assert!(
        shown.contains("ERROR: Individual quota reached. Resets in 19m45s."),
        "{shown}"
    );
    let doc = peek_json(tmp.path());
    assert_eq!(doc["molecules"][0]["ghost"], "quota_exhausted", "{doc}");
    assert_eq!(doc["molecules"][0]["quota_reset_delay"], "19m45s");
}

/// An agy that cannot be started is an error naming it, with no exit code
/// recorded as if agy had run.
#[test]
fn a_missing_agy_is_reported_and_records_no_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let out = agy_pane(tmp.path(), "brief", &tmp.path().join("no-such-agy"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot start agy"), "{stderr}");
    assert!(stderr.contains("no-such-agy"), "{stderr}");
    assert!(!tmp.path().join("agy-exit.code").exists());
}

// ---------------------------------------------------------------------------
// Whole path in an isolated tmux server
// ---------------------------------------------------------------------------

struct TmuxRig {
    root: tempfile::TempDir,
    sockets: tempfile::TempDir,
    bins: PathBuf,
}

const SOCKET: &str = "agypane-0000";

impl TmuxRig {
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.root.path())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                    self.bins.display()
                ),
            )
            .env("HOME", self.root.path())
            .env("LANG", "C.UTF-8")
            .env("NO_COLOR", "1")
            .env("COSMON_CONFIG_HOME", self.root.path().join("config-home"))
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("GIT_AUTHOR_NAME", "Noogram")
            .env("GIT_COMMITTER_NAME", "Noogram")
            .env("GIT_AUTHOR_EMAIL", "test@example.org")
            .env("GIT_COMMITTER_EMAIL", "test@example.org");
        command
    }

    fn cs(&self, args: &[&str]) -> Output {
        let out = self
            .command(env!("CARGO_BIN_EXE_cs"))
            .args(args)
            .output()
            .expect("cs runs");
        assert!(
            out.status.success(),
            "cs {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = self
            .command("tmux")
            .args(["-L", SOCKET])
            .args(args)
            .output()
            .expect("tmux runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn new() -> Option<Self> {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("note: tmux not installed; skipping the tmux agy test");
            return None;
        }
        let root = tempfile::tempdir().unwrap();
        let sockets = tempfile::Builder::new()
            .prefix("csa-")
            .tempdir_in("/tmp")
            .unwrap();
        let bins = root.path().join("bin");
        let rig = Self {
            root,
            sockets,
            bins,
        };
        let config = rig.root.path().join(".cosmon");
        fs::create_dir_all(config.join("formulas")).unwrap();
        fs::write(
            config.join("config.toml"),
            format!("[project]\nproject_id='{SOCKET}'\n[harvest]\non_pane_died='mark'\n"),
        )
        .unwrap();
        fs::write(
            config.join("formulas/witness.formula.toml"),
            "formula='witness'\nversion=1\nid_prefix='witness'\ndescription='Fake agy worker'\n\
             [[steps]]\nid='observe'\ntitle='Observe'\ndescription='Synthetic task input only'\n\
             acceptance='Pane driven'\n",
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", ".cosmon"],
            vec!["commit", "-qm", "test: initialize isolated agy rig"],
        ] {
            let out = rig.command("git").args(args).output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Some(rig)
    }
}

impl Drop for TmuxRig {
    fn drop(&mut self) {
        let _ = self
            .command("tmux")
            .args(["-L", SOCKET, "kill-server"])
            .output();
    }
}

/// #240 and #245 end to end: the harness setting reaches agy's argv, the pane
/// is readable, the session stays open after the brief, `cs whisper` delivers
/// a multi-line continuation as one turn, `cs peek` reads the cumulative
/// usage, and ending the input ends agy with its own exit status.
#[test]
fn tackle_whisper_and_peek_an_agy_worker_in_tmux() {
    let Some(rig) = TmuxRig::new() else { return };
    let record = rig.root.path().join("record");
    fake_agy(&rig.bins, &record, 4, None);

    let value: Value =
        serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"]).stdout).unwrap();
    let id = value["id"].as_str().expect("molecule id").to_owned();
    rig.cs(&[
        "tackle",
        &id,
        "--adapter",
        "agy",
        "--model",
        "fake-model",
        "--harness",
        "effort=high",
        "--harness",
        "mode=plan it",
        "--name",
        "agy-worker",
    ]);
    let mol_dir = FileStore::new(rig.root.path().join(".cosmon/state"))
        .molecule_dir(&MoleculeId::new(&id).unwrap());

    assert!(wait_for(Duration::from_secs(10), || stdin_messages(
        &record
    )
    .len()
        == 1));
    let words = argv(&record);
    let tail: Vec<&str> = words.iter().map(String::as_str).collect();
    assert_eq!(
        tail,
        [
            "--print-timeout",
            "0",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--model",
            "fake-model",
            "--effort",
            "high",
            "--mode",
            "plan it",
            "--dangerously-skip-permissions",
            "-p",
        ],
        "harness settings reach agy verbatim, after the model"
    );
    let brief = stdin_messages(&record)[0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        brief.contains(&id),
        "the brief is the first message: {brief}"
    );

    let events = fs::read_to_string(rig.root.path().join(".cosmon/state/events.jsonl")).unwrap();
    let receipts: Vec<Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e.to_string().contains("harness_setting_selected"))
        .collect();
    let effort = receipts
        .iter()
        .find(|e| e.to_string().contains("--effort high"))
        .unwrap_or_else(|| panic!("an ex-ante receipt for effort: {events}"));
    let effort = effort.to_string();
    assert!(effort.contains("agy:flag"), "{effort}");
    assert!(
        effort.contains("1.5.19-fake"),
        "agy's version is probed: {effort}"
    );

    let pane = || rig.tmux(&["capture-pane", "-p", "-J", "-S", "-200", "-t", "agy-worker"]);
    assert!(
        wait_for(Duration::from_secs(10), || pane()
            .contains("── turn 1 SUCCESS")),
        "{}",
        pane()
    );
    let first = pane();
    assert!(
        first.contains("agy session fake-1 · model fake-model"),
        "{first}"
    );
    assert!(first.contains("reply 1"), "{first}");
    assert!(
        !first.contains("{\"event\""),
        "the pane shows no raw frame: {first}"
    );
    assert_eq!(
        rig.tmux(&["display", "-p", "-t", "agy-worker", "#{pane_dead}"])
            .trim(),
        "0",
        "the session stays open after the brief's turn"
    );

    rig.cs(&["whisper", &id, "--message", "step two\nwith a second line"]);
    assert!(
        wait_for(Duration::from_secs(15), || stdin_messages(&record).len()
            >= 2),
        "the whisper reaches the open session: {}",
        pane()
    );
    assert!(wait_for(Duration::from_secs(10), || pane()
        .contains("── turn 2 SUCCESS")));
    let messages = stdin_messages(&record);
    assert_eq!(
        messages.len(),
        2,
        "one whisper is one turn, extra Enters send nothing: {messages:?}"
    );
    assert_eq!(
        messages[1],
        json!({"event":"user","message":{"content":"step two\nwith a second line"}})
    );

    let doc = peek_json(rig.root.path());
    let row = doc["molecules"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == id.as_str()))
        .cloned()
        .unwrap_or_else(|| panic!("molecule in peek: {doc}"));
    assert_eq!(row["agy_usage"]["input_tokens"], 200, "{row}");

    rig.tmux(&["send-keys", "-t", "agy-worker", "C-d"]);
    assert!(
        wait_for(Duration::from_secs(10), || {
            fs::read_to_string(mol_dir.join("agy-exit.code")).is_ok_and(|c| c == "4\n")
        }),
        "ending the input ends agy with its own status"
    );
}

/// #245: a formula step's `[steps.harness]` table reaches agy's argv the same
/// way as `--harness`, with no flag on the command line.
#[test]
fn a_formula_step_harness_pin_reaches_agy() {
    let Some(rig) = TmuxRig::new() else { return };
    let record = rig.root.path().join("record");
    fake_agy(&rig.bins, &record, 0, None);
    fs::write(
        rig.root.path().join(".cosmon/formulas/pinned.formula.toml"),
        "formula='pinned'\nversion=1\nid_prefix='pinned'\ndescription='Step pins an effort'\n\
         [[steps]]\nid='observe'\ntitle='Observe'\ndescription='Synthetic task input only'\n\
         acceptance='Pane driven'\nadapter='agy'\n[steps.harness]\neffort='xhigh'\n",
    )
    .unwrap();
    let value: Value =
        serde_json::from_slice(&rig.cs(&["--json", "nucleate", "pinned"]).stdout).unwrap();
    let id = value["id"].as_str().expect("molecule id").to_owned();
    rig.cs(&["tackle", &id, "--name", "agy-pinned"]);
    assert!(wait_for(Duration::from_secs(10), || !argv(&record).is_empty()));
    let words = argv(&record);
    let at = words
        .iter()
        .position(|w| w == "--effort")
        .unwrap_or_else(|| panic!("the step's effort reaches agy: {words:?}"));
    assert_eq!(words[at + 1], "xhigh");
    assert_eq!(words[at + 2], "--dangerously-skip-permissions");
}
