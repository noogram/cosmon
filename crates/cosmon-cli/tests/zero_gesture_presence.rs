// SPDX-License-Identifier: AGPL-3.0-only

//! Zero-gesture presence (issue #163, items 1–3).
//!
//! The property: a session registers its presence without anyone typing a
//! command. Each test runs the hook the way the harness would — the command
//! string read out of the settings overlay a dispatch writes, executed through
//! a shell with the harness payload on stdin — and then reads the presence
//! record back.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use cosmon_transport::briefing_receipt::{write_settings_overlay_for_work, ReceiptStation};

const MOLECULE: &str = "task-20261003-cd5e";

fn cs_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_cs"))
}

fn overlay(dir: &Path) -> serde_json::Value {
    let path = dir.join("settings.json");
    let station = ReceiptStation::at(dir.join("receipts"));
    write_settings_overlay_for_work(&path, cs_bin(), &station, false).expect("overlay");
    serde_json::from_slice(&std::fs::read(path).expect("read")).expect("json")
}

/// Every command registered under `event` in the overlay.
fn commands(doc: &serde_json::Value, event: &str) -> Vec<String> {
    doc["hooks"][event]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn presence_command(doc: &serde_json::Value, event: &str) -> String {
    commands(doc, event)
        .into_iter()
        .find(|c| c.contains("sessions hook run --event"))
        .unwrap_or_else(|| panic!("no presence hook under {event}: {doc}"))
}

#[test]
fn the_worker_overlay_registers_a_presence_hook_for_every_lifecycle_moment() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let doc = overlay(tmp.path());
    for (event, token) in [
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "turn-start"),
        ("Stop", "turn-end"),
        ("Notification", "waiting"),
        ("PreToolUse", "asking"),
    ] {
        let cmd = presence_command(&doc, event);
        assert!(cmd.contains(&format!("--event {token}")), "{event}: {cmd}");
    }
    // The receipt hook is still there, beside ours rather than replaced by it.
    assert!(
        commands(&doc, "UserPromptSubmit")
            .iter()
            .any(|c| c.contains("briefing-receipt-hook")),
        "{doc}"
    );
    // The asking hook is scoped to the ask-the-user tool, never every tool call.
    assert_eq!(doc["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion");
}

#[test]
fn a_simulated_worker_hook_run_writes_a_fresh_presence_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).expect("state");
    let doc = overlay(tmp.path());

    let before = chrono::Utc::now();
    let cmd = format!(
        "{} --config '{}'",
        presence_command(&doc, "UserPromptSubmit"),
        state.display()
    );
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        // A worker has no exported session id and no tty: the payload's own
        // session id is all it has.
        .env_remove("COSMON_SESSION_ID")
        .env_remove("CLAUDE_SESSION_ID")
        .env("COSMON_PARENT_MOL_ID", MOLECULE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"session_id":"native-worker-1","hook_event_name":"UserPromptSubmit"}"#)
        .expect("payload");
    let out = child.wait_with_output().expect("exit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let store = cosmon_filestore::presence_store::PresenceStore::new(&state);
    let rows = store.scan().expect("scan");
    assert_eq!(rows.len(), 1, "one record for one session: {rows:?}");
    let record = &rows[0];
    assert!(record.heartbeat_at >= before, "the record is fresh");
    assert!(record.is_live(chrono::Utc::now()));
    assert_eq!(record.native_session_id.as_deref(), Some("native-worker-1"));
    assert_eq!(
        record.current_molecule.as_ref().map(|m| m.as_str()),
        Some(MOLECULE)
    );
    assert_eq!(record.headline, "worker: turn-start");
    assert!(!record.role.is_primary(), "a hook never claims a seat");

    let canonical_snapshot = state.join("presence/session-native-worker-1.json");
    assert!(
        canonical_snapshot.exists(),
        "the hook must use the shared canonical filename"
    );

    let whisper = Command::new(cs_bin())
        .args(["--config"])
        .arg(&state)
        .args([
            "--json",
            "whisper",
            "--to-session",
            "native-worker-1",
            "--message",
            "hello",
        ])
        .output()
        .expect("whisper");
    assert!(
        whisper.status.success(),
        "{}",
        String::from_utf8_lossy(&whisper.stderr)
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&whisper.stdout).expect("whisper receipt");
    assert_eq!(receipt["stale_session"], false);
    assert!(state.join("presence/session-native-worker-1.log").exists());
}

#[test]
fn two_sessions_without_an_exported_id_do_not_share_a_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();
    for native in ["native-a", "native-b"] {
        let mut child = Command::new(cs_bin())
            .args(["--config"])
            .arg(state)
            .args(["sessions", "hook", "run", "--event", "turn-start"])
            .env_remove("COSMON_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let payload = format!(r#"{{"session_id":"{native}"}}"#);
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("payload");
        assert!(child.wait().expect("exit").success());
    }
    let store = cosmon_filestore::presence_store::PresenceStore::new(state.to_path_buf());
    assert_eq!(store.scan().expect("scan").len(), 2);
}
