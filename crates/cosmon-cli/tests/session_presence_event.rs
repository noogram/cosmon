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
    fire_with(
        state,
        session,
        event,
        worker_molecule,
        r#"{"session_id":"native-1"}"#,
    );
}

/// Like [`fire`], with the provider's JSON payload given explicitly.
fn fire_with(
    state: &Path,
    session: &str,
    event: &str,
    worker_molecule: Option<&str>,
    payload: &str,
) {
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
        let _ = pipe.write_all(payload.as_bytes());
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

/// The latest presence snapshot for `session`.
fn presence_record(state: &Path, session: &str) -> serde_json::Value {
    let file = state
        .join("presence")
        .join(format!("session-{session}.json"));
    serde_json::from_str(&std::fs::read_to_string(file).expect("presence file"))
        .expect("presence json")
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

    let file = state.join("presence").join("session-pilot-a.json");
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("presence file"))
            .expect("json");
    assert_eq!(record["state"], "waiting_permission");
    assert_eq!(record["galaxy"], "orchard");
}

/// Fire a `waiting` hook whose payload is Claude's `Notification` of the given
/// `notification_type` (none when `None`), and return the recorded state.
fn state_after_notification(notification_type: Option<&str>) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    let payload = match notification_type {
        Some(t) => {
            format!(r#"{{"session_id":"native-1","notification_type":"{t}","message":"not read"}}"#)
        }
        None => r#"{"session_id":"native-1"}"#.to_owned(),
    };
    fire_with(&state, "worker-a", "waiting", None, &payload);
    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows[0]["state"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_permission_prompt_notification_is_waiting_permission() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    fire_with(
        &state,
        "worker-a",
        "waiting",
        None,
        r#"{"session_id":"native-1","notification_type":"permission_prompt","message":"Cosmon  needs\tyour permission\nprivate second line"}"#,
    );

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["state"], "waiting_permission");
    assert_eq!(rows[0]["detail"], "Cosmon needs your permission");
    assert_eq!(
        presence_record(&state, "worker-a")["detail"],
        "Cosmon needs your permission"
    );
}

#[test]
fn notification_detail_redacts_a_fake_token_and_absolute_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    let fake_token = "sk-exampletoken123456789";
    let private_path = "/srv/cosmon/private.txt";
    let payload = format!(
        r#"{{"session_id":"native-1","notification_type":"permission_prompt","message":"Use {fake_token} at {private_path}"}}"#
    );

    fire_with(&state, "worker-a", "waiting", None, &payload);

    for record in [
        presence_rows(&state).remove(0),
        presence_record(&state, "worker-a"),
    ] {
        let detail = record["detail"].as_str().expect("detail");
        assert!(detail.contains("[redacted]"), "{detail}");
        assert!(detail.contains("[path]"), "{detail}");
        assert!(!detail.contains(fake_token), "{detail}");
        assert!(!detail.contains(private_path), "{detail}");
    }
}

#[test]
fn notification_detail_is_hard_capped_at_160_characters() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    let message = "waiting for a reviewed operator decision ".repeat(10);
    assert!(message.chars().count() >= 400);
    let payload = serde_json::json!({
        "session_id": "native-1",
        "notification_type": "permission_prompt",
        "message": message,
    })
    .to_string();

    fire_with(&state, "worker-a", "waiting", None, &payload);

    for record in [
        presence_rows(&state).remove(0),
        presence_record(&state, "worker-a"),
    ] {
        let detail = record["detail"].as_str().expect("detail");
        assert!(detail.chars().count() <= 160, "{detail}");
        assert!(detail.ends_with('…'), "{detail}");
    }
}

#[test]
fn turn_end_has_no_detail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    fire_with(
        &state,
        "worker-a",
        "turn-end",
        None,
        r#"{"session_id":"native-1","message":"must not cross"}"#,
    );

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["state"], "idle");
    assert!(rows[0].get("detail").is_none(), "{}", rows[0]);
    let record = presence_record(&state, "worker-a");
    assert!(record.get("detail").is_none(), "{record}");
}

#[test]
fn an_asking_tool_call_never_carries_a_detail() {
    // `asking` is also reached from the tool-call moment, whose payload is
    // conversation content. Only a Notification moment may fill `detail`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    fire_with(
        &state,
        "worker-a",
        "asking",
        None,
        r#"{"session_id":"native-1","message":"must not cross"}"#,
    );

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["state"], "asking");
    assert!(rows[0].get("detail").is_none(), "{}", rows[0]);
    let record = presence_record(&state, "worker-a");
    assert!(record.get("detail").is_none(), "{record}");
}

#[test]
fn detail_changes_update_presence_without_emitting_an_unchanged_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    for message in ["First permission", "Latest permission"] {
        let payload = serde_json::json!({
            "session_id": "native-1",
            "notification_type": "permission_prompt",
            "message": message,
        })
        .to_string();
        fire_with(&state, "worker-a", "waiting", None, &payload);
    }

    let rows = presence_rows(&state);
    assert_eq!(
        rows.len(),
        1,
        "a detail-only change must not append: {rows:?}"
    );
    assert_eq!(rows[0]["detail"], "First permission");
    assert_eq!(
        presence_record(&state, "worker-a")["detail"],
        "Latest permission"
    );
}

#[test]
fn an_idle_prompt_notification_is_idle_input_not_a_blocked_worker() {
    assert_eq!(state_after_notification(Some("idle_prompt")), "idle_input");
}

#[test]
fn an_elicitation_dialog_notification_is_asking() {
    assert_eq!(
        state_after_notification(Some("elicitation_dialog")),
        "asking"
    );
}

#[test]
fn an_unknown_or_absent_notification_type_stays_waiting_permission() {
    assert_eq!(
        state_after_notification(Some("auth_success")),
        "waiting_permission"
    );
    assert_eq!(state_after_notification(None), "waiting_permission");
}
