// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence around a question and a denial (issue #179).
//!
//! Each test installs the pilot hooks with `cs sessions hook install`, reads
//! the command out of the written settings file the way the harness does, and
//! feeds it payloads in the order Claude Code 2.1.291 was measured to emit
//! them. The presence record is read back after every step.
//!
//! Measured order for an `AskUserQuestion` prompt: `UserPromptSubmit`,
//! `PreToolUse(AskUserQuestion)`, `PermissionRequest`, `Notification`
//! (`permission_prompt`, about six seconds later), then `PostToolUse` when the
//! question is answered and `Stop` when the turn ends.
//!
//! Measured order for a permission prompt that the user denies:
//! `UserPromptSubmit`, `PreToolUse(Bash)`, `PermissionRequest`, `Notification`
//! (`permission_prompt`), and then nothing at all: no `Stop`, `PostToolUse`,
//! `PostToolUseFailure` or `PermissionDenied`, for at least 80 seconds.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use cosmon_core::presence::SessionState;

const NATIVE: &str = "native-pilot-1";

fn cs_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_cs"))
}

fn install(dir: &Path) -> serde_json::Value {
    let settings = dir.join("settings.local.json");
    let out = Command::new(cs_bin())
        .args(["sessions", "hook", "install", "--provider", "claude"])
        .arg("--settings")
        .arg(&settings)
        .arg("--cs-bin")
        .arg(cs_bin())
        .output()
        .expect("install");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&std::fs::read(settings).expect("read")).expect("json")
}

/// The hook command Claude would run for `event` when the tool is `tool`.
fn command_for(doc: &serde_json::Value, event: &str, tool: Option<&str>) -> Option<String> {
    doc["hooks"][event]
        .as_array()?
        .iter()
        .filter(|entry| match entry["matcher"].as_str() {
            None => true,
            Some(m) => tool == Some(m),
        })
        .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(ToOwned::to_owned))
        .find(|c| c.contains("sessions hook run --event"))
}

/// Deliver one recorded payload; returns the presence state afterwards.
fn deliver(
    doc: &serde_json::Value,
    state: &Path,
    payload: &serde_json::Value,
) -> Option<SessionState> {
    let event = payload["hook_event_name"].as_str().expect("event name");
    let tool = payload["tool_name"].as_str();
    if let Some(cmd) = command_for(doc, event, tool) {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("{cmd} --config '{}'", state.display()))
            .env_remove("COSMON_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("COSMON_PARENT_MOL_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.to_string().as_bytes())
            .expect("payload");
        assert!(child.wait().expect("exit").success());
    }
    let store = cosmon_filestore::presence_store::PresenceStore::new(state.to_path_buf());
    store
        .scan()
        .expect("scan")
        .first()
        .and_then(|record| record.state)
}

fn payload(event: &str, tool: Option<&str>, kind: Option<&str>) -> serde_json::Value {
    let mut p = serde_json::json!({"session_id": NATIVE, "hook_event_name": event});
    if let Some(tool) = tool {
        p["tool_name"] = tool.into();
    }
    if let Some(kind) = kind {
        p["notification_type"] = kind.into();
        p["message"] = "Claude needs your permission".into();
    }
    p
}

#[test]
fn the_pilot_installer_wires_asking_to_the_question_tool_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let doc = install(tmp.path());
    let pre = doc["hooks"]["PreToolUse"].as_array().expect("PreToolUse");
    assert_eq!(pre.len(), 1, "{doc}");
    assert_eq!(pre[0]["matcher"], "AskUserQuestion", "{doc}");
    assert!(
        pre[0]["hooks"][0]["command"]
            .as_str()
            .is_some_and(|c| c.contains("sessions hook run --event asking")),
        "{doc}"
    );
}

#[test]
fn a_question_on_a_pilot_session_stays_asking_until_it_is_answered() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).expect("state");
    let doc = install(tmp.path());

    let step = |p| deliver(&doc, &state, &p);
    assert_eq!(
        step(payload("UserPromptSubmit", None, None)),
        Some(SessionState::Working)
    );
    assert_eq!(
        step(payload("PreToolUse", Some("AskUserQuestion"), None)),
        Some(SessionState::Asking)
    );
    // The permission_prompt notification that accompanies the question dialog
    // must not turn the question back into a permission prompt.
    assert_eq!(
        step(payload("Notification", None, Some("permission_prompt"))),
        Some(SessionState::Asking)
    );
    assert_eq!(
        step(payload("PostToolUse", Some("AskUserQuestion"), None)),
        Some(SessionState::Working)
    );
    assert_eq!(step(payload("Stop", None, None)), Some(SessionState::Idle));
}

#[test]
fn a_permission_prompt_after_an_answered_question_is_a_permission_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).expect("state");
    let doc = install(tmp.path());

    let step = |p| deliver(&doc, &state, &p);
    step(payload("UserPromptSubmit", None, None));
    step(payload("PreToolUse", Some("AskUserQuestion"), None));
    step(payload("PostToolUse", Some("AskUserQuestion"), None));
    assert_eq!(
        step(payload("Notification", None, Some("permission_prompt"))),
        Some(SessionState::WaitingPermission)
    );
}

#[test]
fn a_denied_prompt_emits_no_hook_and_the_next_prompt_leaves_waiting() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).expect("state");
    let doc = install(tmp.path());

    let step = |p| deliver(&doc, &state, &p);
    step(payload("UserPromptSubmit", None, None));
    assert_eq!(
        step(payload("Notification", None, Some("permission_prompt"))),
        Some(SessionState::WaitingPermission)
    );
    // Denial: Claude Code fires no event, so there is nothing to deliver here.
    // The first event after it is the next prompt.
    assert_eq!(
        step(payload("UserPromptSubmit", None, None)),
        Some(SessionState::Working)
    );
}
