// SPDX-License-Identifier: AGPL-3.0-only

//! The presence hook's typed output (issue #165): a `session_presence` event in
//! the galaxy ledger on state change only, a typed `state` on the presence
//! record, and the galaxy name taken from the session's galaxy.
//!
//! Every test reads the exact galaxy ledger,
//! `<galaxy>/.cosmon/state/events.jsonl`, and nothing else.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cosmon_state::event_log::resolve_events_log_path;

/// A galaxy named `orchard` with its state directory created.
fn galaxy(tmp: &Path) -> PathBuf {
    let state = tmp.join("orchard").join(".cosmon").join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    state
}

/// Fire `cs sessions hook run --event <event>` as a provider would.
fn fire(state: &Path, session: &str, event: &str, worker_molecule: Option<&str>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.arg("--config")
        .arg(state)
        .args(["sessions", "hook", "run", "--event", event])
        .env("COSMON_SESSION_ID", session)
        .env("COSMON_SESSIONS_CLAUDE_ROOT", state.join("no-claude"))
        .env("COSMON_SESSIONS_CODEX_ROOT", state.join("no-codex"))
        .env_remove("COSMON_NO_OPERATOR_EVENTS")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_WORKER_ID")
        .env_remove("COSMON_STATE_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(mol) = worker_molecule {
        cmd.env("COSMON_PARENT_MOL_ID", mol);
    }
    let mut child = cmd.spawn().expect("spawn cs");
    {
        use std::io::Write as _;
        let mut pipe = child.stdin.take().expect("stdin");
        let _ = pipe.write_all(br#"{"session_id":"native-1"}"#);
    }
    let out = child.wait_with_output().expect("cs exits");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The `session_presence` rows of the galaxy ledger, in order.
fn presence_rows(state: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(resolve_events_log_path(state)).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["type"] == "session_presence")
        .collect()
}

fn states(rows: &[serde_json::Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["state"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn a_state_change_lands_in_the_galaxy_ledger_with_its_contract_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    fire(&state, "pilot-a", "session-start", None);

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row["state"], "session_start");
    assert_eq!(row["role"], "pilot");
    assert_eq!(row["session_id"], "pilot-a");
    assert_eq!(row["provider"], "claude");
    assert_eq!(row["schema_version"], 1);
    assert!(row["timestamp"].is_string(), "{row}");
    assert!(row.get("molecule_id").is_none(), "{row}");
}

#[test]
fn only_a_state_change_is_emitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    for event in [
        "session-start",
        "turn-start",
        "turn-start",
        "turn-start",
        "turn-end",
        "turn-end",
        "waiting",
        "waiting",
        "turn-start",
    ] {
        fire(&state, "pilot-a", event, None);
    }

    assert_eq!(
        states(&presence_rows(&state)),
        [
            "session_start",
            "working",
            "idle",
            "waiting_permission",
            "working"
        ]
    );
}

#[test]
fn a_worker_event_names_its_molecule_and_the_asking_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    fire(&state, "worker-a", "asking", Some("task-20261003-0d5c"));

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["role"], "worker");
    assert_eq!(rows[0]["state"], "asking");
    assert_eq!(rows[0]["molecule_id"], "task-20261003-0d5c");
}

#[test]
fn the_presence_record_carries_a_typed_state_and_the_sessions_galaxy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    fire(&state, "pilot-a", "waiting", None);

    let file = state.join("presence").join("pilot-a.json");
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("presence file"))
            .expect("json");
    assert_eq!(record["state"], "waiting_permission");
    assert_eq!(record["galaxy"], "orchard");
}
