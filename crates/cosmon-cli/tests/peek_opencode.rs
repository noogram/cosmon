// SPDX-License-Identifier: AGPL-3.0-only

//! Recorded OpenCode message data to the built observability commands.

use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::event_v2::{AdapterSelectionSource, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const ASSISTANT: &str = include_str!("fixtures/opencode/assistant.json");
const SESSION: &str = "fixture-session";
const TITLE: &str = "cosmon worker fixture";

fn seed(root: &Path, message: &Value, bind_session: bool) -> anyhow::Result<()> {
    let state = root.join(".cosmon/state");
    fs::create_dir_all(&state)?;
    fs::write(
        root.join(".cosmon/config.toml"),
        "[project]\nproject_id = \"peek-opencode-test\"\n",
    )?;
    let store = FileStore::new(&state);
    let id = MoleculeId::new("task-20261128-a901")?;
    let worker_id = WorkerId::new("worker-opencode-test")?;
    let now = Utc::now();
    let worker: WorkerData = serde_json::from_value(json!({
        "id": worker_id, "agent_id": "agent-test", "role": "implementation",
        "clearance": "execute", "status": "stopped", "current_molecule": id, "updated_at": now
    }))?;
    let molecule: MoleculeData = serde_json::from_value(json!({
        "id": id, "formula_id": "task-work", "status": "running", "variables": {},
        "assigned_worker": worker_id, "created_at": now, "updated_at": now,
        "total_steps": 1, "current_step": 0, "completed_steps": [], "links": [],
        "project_id": "peek-opencode-test"
    }))?;
    let mut fleet = Fleet::new();
    fleet.workers.insert(worker_id.clone(), worker);
    store.save_fleet(&fleet)?;
    store.save_molecule(&id, &molecule)?;
    cosmon_state::event_log::emit_one(
        &cosmon_state::event_log::resolve_events_log_path(&state),
        EventV2::AdapterSelected {
            mol_id: id.clone(),
            adapter_name: "opencode".to_owned(),
            selected_at: now,
            selection_source: AdapterSelectionSource::Cli {
                flag: "opencode".to_owned(),
            },
            role_hint: None,
            loop_ownership: Default::default(),
        },
        None,
    )?;
    if bind_session {
        fs::write(
            store.molecule_dir(&id).join("opencode-session.json"),
            serde_json::to_vec(&json!({"worker_id": worker_id, "title": TITLE}))?,
        )?;
    }
    let db = Connection::open(root.join("opencode.db"))?;
    db.execute_batch(
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, title TEXT, model TEXT);
         CREATE TABLE session_message (session_id TEXT, seq INTEGER, type TEXT, data TEXT);",
    )?;
    db.execute(
        "INSERT INTO session_v2 VALUES (?1, ?2, ?3)",
        params![SESSION, TITLE, message["model"].to_string()],
    )?;
    db.execute(
        "INSERT INTO session_message VALUES (?1, 1, 'assistant', ?2)",
        params![SESSION, message.to_string()],
    )?;
    // A different session, even on the same database, cannot be charged to this worker.
    db.execute(
        "INSERT INTO session_message VALUES ('unrelated', 2, 'assistant', ?1)",
        [message.to_string()],
    )?;
    Ok(())
}

fn cs(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("OPENCODE_DB", root.join("opencode.db"))
        .args(args)
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn assert_record(record: &Value) {
    let tokens = &record["tokens"];
    assert_eq!(tokens["input_tokens"]["tokens"], 17, "{record}");
    assert_eq!(tokens["cached_input_tokens"]["tokens"], 5);
    assert_eq!(tokens["output_tokens"]["tokens"], 11);
    assert_eq!(tokens["reasoning_output_tokens"]["tokens"], 7);
    assert_eq!(tokens["provenance"]["provider"], "normal");
    assert_eq!(record["api_equivalent"]["amount_usd"], 0.0000365);
}

#[test]
fn recorded_usage_reaches_peek_and_pulse_without_a_live_pane_or_observer() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), &serde_json::from_str(ASSISTANT)?, true)?;
    let doc: Value = serde_json::from_str(&cs(tmp.path(), &["peek", "--json"])?)?;
    let row = &doc["molecules"][0];
    assert_record(&row["usage"][0]);
    assert_eq!(row["opencode"]["model"], "normal/fixture", "{doc}");
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("IN/CACHED/OUT/RSN 17/5/11/7"), "{text}");
    assert!(text.contains("normal/fixture"), "{text}");
    let pulse: Value = serde_json::from_str(&cs(tmp.path(), &["pulse", "--json"])?)?;
    let worker = &pulse["worker_usage"][0];
    assert_eq!(worker["worker_id"], "worker-opencode-test", "{pulse}");
    assert_eq!(worker["opencode"]["model"], "normal/fixture");
    assert_record(&worker["usage"]);
    Ok(())
}

#[test]
fn absent_cache_stays_unknown_and_measured_zero_survives() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut message: Value = serde_json::from_str(ASSISTANT)?;
    message["tokens"]["cache"] = json!({"write": 0});
    message["tokens"]["reasoning"] = json!(0);
    seed(tmp.path(), &message, true)?;
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("IN/CACHED/OUT/RSN -/-/4/0"), "{text}");
    let doc: Value = serde_json::from_str(&cs(tmp.path(), &["peek", "--json"])?)?;
    let usage = &doc["molecules"][0]["usage"][0];
    let tokens = &usage["tokens"];
    assert_eq!(tokens["input_tokens"]["status"], "unavailable", "{doc}");
    assert_eq!(tokens["cached_input_tokens"]["status"], "unavailable");
    assert_eq!(tokens["reasoning_output_tokens"]["tokens"], 0);
    Ok(())
}

#[test]
fn an_unbound_worker_does_not_borrow_a_session() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), &serde_json::from_str(ASSISTANT)?, false)?;
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("opencode IN/CACHED/OUT/RSN -/-/-/-"), "{text}");
    let doc: Value = serde_json::from_str(&cs(tmp.path(), &["peek", "--json"])?)?;
    assert_eq!(doc["molecules"][0]["usage"], json!([]), "{doc}");
    Ok(())
}

#[test]
fn a_terminal_rate_refusal_is_visible_without_inventing_usage() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let message: Value = serde_json::from_str(include_str!("fixtures/opencode/rate-limited.json"))?;
    seed(tmp.path(), &message, true)?;
    let db = Connection::open(tmp.path().join("opencode.db"))?;
    db.execute(
        "INSERT INTO session_message VALUES (?1, 2, 'idle', ?2)",
        params![SESSION, json!({"error": message["error"]}).to_string()],
    )?;
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("IN/CACHED/OUT/RSN -/-/-/-"), "{text}");
    assert!(text.contains("rate-limited"), "{text}");
    let doc: Value = serde_json::from_str(&cs(tmp.path(), &["peek", "--json"])?)?;
    let row = &doc["molecules"][0];
    assert_eq!(row["usage"], json!([]), "{doc}");
    assert_eq!(row["opencode"]["refusal"]["kind"], "rate_limited");
    assert_eq!(row["opencode"]["refusal"]["retry_after_ms"], 1000);
    let pulse: Value = serde_json::from_str(&cs(tmp.path(), &["pulse", "--json"])?)?;
    assert_eq!(pulse["worker_usage"][0]["opencode"], row["opencode"]);
    assert!(pulse["worker_usage"][0]["usage"].is_null());
    Ok(())
}
