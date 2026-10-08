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
    fire_provider_with(state, session, event, worker_molecule, None, payload);
}

/// Like [`fire_with`], with the provider named as its installed hook does.
fn fire_provider_with(
    state: &Path,
    session: &str,
    event: &str,
    worker_molecule: Option<&str>,
    provider: Option<&str>,
    payload: &str,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.arg("--config")
        .arg(state)
        .args(["sessions", "hook", "run", "--event", event])
        .env("COSMON_SESSION_ID", session)
        // Claude state-transition fixtures model a marked launch. The Codex
        // regressions below deliberately exercise unmarked native fallback.
        .env(
            "COSMON_SESSION_LAUNCH_ID",
            if provider == Some("codex") {
                ""
            } else {
                session
            },
        )
        .env("COSMON_SESSIONS_CLAUDE_ROOT", state.join("no-claude"))
        .env("COSMON_SESSIONS_CODEX_ROOT", state.join("no-codex"))
        .env_remove("COSMON_NO_OPERATOR_EVENTS")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_WORKER_ID")
        .env_remove("COSMON_STATE_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(provider) = provider {
        cmd.args(["--provider", provider]);
    }
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
    out
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

fn fire_lifecycle(state: &Path, session: &str, sequence: u64, event: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(state)
        .args([
            "sessions",
            "lifecycle",
            "--session",
            session,
            "--sequence",
            &sequence.to_string(),
            "--event",
            event,
        ])
        .env_remove("COSMON_NO_OPERATOR_EVENTS")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_WORKER_ID")
        .output()
        .expect("run lifecycle ingress");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn seed_providerless_presence(state: &Path, session: &str) {
    let row = serde_json::json!({
        "session_id": session,
        "galaxy": "orchard",
        "cwd": "/fixture/provider-session",
        "pid": 4242,
        "started_at": "2026-10-08T08:00:00Z",
        "heartbeat_at": "2026-10-08T08:01:00Z",
        "headline": "",
        "role": "primary",
        "capabilities": []
    });
    let dir = state.join("presence");
    std::fs::create_dir_all(&dir).expect("presence dir");
    std::fs::write(
        dir.join(format!("session-{session}.json")),
        serde_json::to_vec_pretty(&row).expect("json"),
    )
    .expect("fixture snapshot");
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
    assert_eq!(row["source"], "live_lifecycle");
    assert_eq!(row["schema_version"], 1);
    assert!(row["timestamp"].is_string(), "{row}");
    assert!(row.get("molecule_id").is_none(), "{row}");
}

#[test]
fn codex_lifecycle_ingress_orders_events_and_degrades_on_disconnect() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    seed_providerless_presence(&state, "pilot-live");

    fire_lifecycle(&state, "pilot-live", 1, "turn-start");
    fire_lifecycle(&state, "pilot-live", 1, "turn-complete");
    fire_lifecycle(&state, "pilot-live", 0, "turn-complete");
    fire_lifecycle(&state, "pilot-live", 2, "approval-request");
    fire_lifecycle(&state, "pilot-live", 3, "turn-complete");
    fire_lifecycle(&state, "pilot-live", 4, "disconnect");
    // A reconnect continues the client-owned sequence; resetting it to one is
    // stale and cannot overwrite the disconnect reading.
    fire_lifecycle(&state, "pilot-live", 1, "turn-start");
    fire_lifecycle(&state, "pilot-live", 5, "turn-start");

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert_eq!(
        rows.iter()
            .map(|row| row["source"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        [
            "live_lifecycle",
            "live_lifecycle",
            "live_lifecycle",
            "unavailable",
            "live_lifecycle"
        ]
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.get("state").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>(),
        [
            Some("working"),
            Some("waiting_permission"),
            Some("idle"),
            None,
            Some("working")
        ]
    );

    let record = presence_record(&state, "pilot-live");
    assert_eq!(record["lifecycle_source"], "live_lifecycle");
    assert_eq!(record["lifecycle_sequence"], 5);
    assert_eq!(record["state"], "working");
    assert_eq!(record["provider"], "codex");
}

#[test]
fn codex_lifecycle_refuses_to_invent_presence_provenance() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    let path = state
        .join("presence")
        .join("session-never-established.json");

    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(&state)
        .args([
            "sessions",
            "lifecycle",
            "--session",
            "never-established",
            "--sequence",
            "1",
            "--event",
            "turn-start",
        ])
        .output()
        .expect("run lifecycle ingress");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "no presence row; establish it with sessions attach or a provider hook first"
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!path.exists(), "lifecycle ingress invented a presence row");
    assert!(presence_rows(&state).is_empty());
}

#[test]
fn lifecycle_ingress_preserves_every_non_lifecycle_presence_field() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    let before = serde_json::json!({
        "session_id": "metadata-session",
        "galaxy": "forwarded-galaxy",
        "cwd": "/forwarded/worktree",
        "pid": 4242,
        "started_at": "2026-10-08T08:00:00Z",
        "heartbeat_at": "2026-10-08T08:01:00Z",
        "current_molecule": "task-20261008-abcd",
        "headline": "preserve me",
        "tty": "ttys099",
        "provider": "codex",
        "native_session_id": "anonymous-native-fixture",
        "role": "primary",
        "follows": "peer-fixture",
        "capabilities": ["observe", "message"],
        "checkpoint_id": "checkpoint-fixture",
        "mission": "task-20261008-cdef",
        "lease_epoch": 7,
        "state": "idle",
        "lifecycle_source": "turn_end_only",
        "detail": "old detail"
    });
    let dir = state.join("presence");
    std::fs::create_dir_all(&dir).expect("presence dir");
    std::fs::write(
        dir.join("session-metadata-session.json"),
        serde_json::to_vec_pretty(&before).expect("json"),
    )
    .expect("fixture snapshot");

    fire_lifecycle(&state, "metadata-session", 1, "approval-request");
    let after = presence_record(&state, "metadata-session");
    for key in [
        "session_id",
        "galaxy",
        "cwd",
        "pid",
        "started_at",
        "current_molecule",
        "headline",
        "tty",
        "provider",
        "native_session_id",
        "role",
        "follows",
        "capabilities",
        "checkpoint_id",
        "mission",
        "lease_epoch",
    ] {
        assert_eq!(after[key], before[key], "field {key} changed");
    }
    assert_ne!(after["heartbeat_at"], before["heartbeat_at"]);
    assert_eq!(after["state"], "waiting_permission");
    assert_eq!(after["lifecycle_source"], "live_lifecycle");
    assert_eq!(after["lifecycle_sequence"], 1);
    assert!(after.get("detail").is_none(), "{after}");
}

#[test]
fn codex_lifecycle_refuses_a_presence_row_owned_by_another_provider() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    let row = serde_json::json!({
        "session_id": "claude-owned",
        "galaxy": "orchard",
        "cwd": "/fixture",
        "pid": 4242,
        "started_at": "2026-10-08T08:00:00Z",
        "heartbeat_at": "2026-10-08T08:01:00Z",
        "headline": "unchanged",
        "provider": "claude",
        "role": "copilot",
        "capabilities": [],
        "state": "idle",
        "lifecycle_source": "live_lifecycle"
    });
    let dir = state.join("presence");
    std::fs::create_dir_all(&dir).expect("presence dir");
    let path = dir.join("session-claude-owned.json");
    let bytes = serde_json::to_vec_pretty(&row).expect("json");
    std::fs::write(&path, &bytes).expect("fixture snapshot");

    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(&state)
        .args([
            "sessions",
            "lifecycle",
            "--session",
            "claude-owned",
            "--sequence",
            "1",
            "--event",
            "turn-start",
        ])
        .output()
        .expect("run lifecycle ingress");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("presence provider is 'claude'"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&path).expect("snapshot"), bytes);
    assert!(presence_rows(&state).is_empty());
}

#[test]
fn codex_notify_is_explicitly_turn_end_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());
    fire_provider_with(
        &state,
        "ignored-launch-id",
        "turn-end",
        None,
        Some("codex"),
        r#"{"thread-id":"native-codex-fixture"}"#,
    );

    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["state"], "idle");
    assert_eq!(rows[0]["source"], "turn_end_only");
    let record = presence_record(&state, "native-codex-fixture");
    assert_eq!(record["lifecycle_source"], "turn_end_only");
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

#[test]
fn codex_notify_uses_hyphenated_thread_id_over_an_inherited_generic_session_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    for thread_id in ["thread-alpha", "thread-bravo"] {
        fire_provider_with(
            &state,
            "shared-inherited-session",
            "turn-end",
            None,
            Some("codex"),
            &serde_json::json!({ "thread-id": thread_id }).to_string(),
        );
    }

    let records =
        ["thread-alpha", "thread-bravo"].map(|thread_id| presence_record(&state, thread_id));
    assert_eq!(records[0]["provider"], "codex");
    assert_eq!(records[0]["native_session_id"], "thread-alpha");
    assert_eq!(records[1]["native_session_id"], "thread-bravo");
    assert!(
        !state
            .join("presence")
            .join("session-shared-inherited-session.json")
            .exists(),
        "a generic inherited id must not collapse Codex pilots"
    );
}

#[test]
fn codex_pilot_without_a_thread_id_publishes_no_ambiguous_presence() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    let output = fire_provider_with(
        &state,
        "shared-inherited-session",
        "turn-end",
        None,
        Some("codex"),
        r#"{"session_id":"not-a-codex-thread"}"#,
    );

    assert!(
        presence_rows(&state).is_empty(),
        "a payload without the native Codex identity must not publish"
    );
    assert!(
        !state.join("presence").exists(),
        "a payload without the native Codex identity must not create a row"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("native identity"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn codex_pilot_does_not_admit_an_underscore_thread_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    let output = fire_provider_with(
        &state,
        "shared-inherited-session",
        "turn-end",
        None,
        Some("codex"),
        r#"{"thread_id":"underscore-only-thread"}"#,
    );

    assert!(presence_rows(&state).is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("native identity"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn codex_worker_keeps_its_dispatch_session_identity_when_thread_id_is_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = galaxy(tmp.path());

    fire_provider_with(
        &state,
        "worker-dispatch-session",
        "turn-end",
        Some("task-20261003-0d5c"),
        Some("codex"),
        r#"{}"#,
    );

    let record = presence_record(&state, "worker-dispatch-session");
    assert_eq!(record["provider"], "codex");
    assert!(record.get("native_session_id").is_none(), "{record}");
    let rows = presence_rows(&state);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["role"], "worker");
    assert_eq!(rows[0]["molecule_id"], "task-20261003-0d5c");
}
