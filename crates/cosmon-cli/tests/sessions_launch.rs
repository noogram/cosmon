// SPDX-License-Identifier: AGPL-3.0-only

//! Real provider subprocesses must share one launch identity with their hooks
//! and authority checks. Provider executables below are hermetic shell fixtures.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use cosmon_minisign_testkit::Operator;

fn cs(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(root)
        .env("HOME", root.join("isolated-home"));
    for key in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_WORKER_ID",
        "COSMON_SESSION_ID",
        "COSMON_SESSION_LAUNCH_ID",
        "CLAUDE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "COSMON_STATE_DIR",
        "COSMON_TAKEOVER_PUBKEY",
        "COSMON_NO_OPERATOR_EVENTS",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("COSMON_SESSIONS_CLAUDE_ROOT", root.join("absent-claude"))
        .env("COSMON_SESSIONS_CODEX_ROOT", root.join("absent-codex"));
    cmd
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn galaxy(root: &Path) {
    success(cs(root).arg("init").output().expect("init"));
}

fn provider(root: &Path, name: &str, script: &str) {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin directory");
    let path = bin.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{script}\n")).expect("provider script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("executable");
}

fn launch(root: &Path, name: &str) -> Command {
    let mut cmd = cs(root);
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    cmd.args(["sessions", "launch", name, "--"])
        .env("PATH", path)
        .env("TEST_CS", env!("CARGO_BIN_EXE_cs"))
        .env("TEST_PROVIDER", name)
        .env("COSMON_SESSION_ID", "inherited-parent")
        .env("COSMON_SESSION_LAUNCH_ID", "inherited-parent");
    cmd
}

fn record(root: &Path, sid: &str) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(
            root.join(".cosmon/state/presence")
                .join(format!("session-{sid}.json")),
        )
        .expect("presence record"),
    )
    .expect("presence JSON")
}

#[test]
fn each_provider_launch_gets_a_fresh_id_and_preserves_arguments_and_exit_status() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut identities = Vec::new();
    for name in ["claude", "codex"] {
        provider(
            tmp.path(),
            name,
            r#"
printf '%s\n' "$COSMON_SESSION_ID" "$COSMON_SESSION_LAUNCH_ID" "$@"
exit 23
"#,
        );
        for _ in 0..2 {
            let out = launch(tmp.path(), name)
                .args(["--model", "a value", ""])
                .output()
                .expect("launch");
            assert_eq!(out.status.code(), Some(23));
            let text = String::from_utf8(out.stdout).expect("UTF-8");
            let lines: Vec<_> = text.lines().collect();
            assert_eq!(lines.len(), 5, "{text:?}");
            assert_eq!(lines[0], lines[1]);
            assert!(!lines[0].is_empty());
            assert_ne!(lines[0], "inherited-parent");
            assert_eq!(&lines[2..], &["--model", "a value", ""]);
            assert!(!identities.contains(&lines[0].to_owned()));
            identities.push(lines[0].to_owned());
        }
    }
}

fn grant(root: &Path, mission: &str, sid: &str) {
    let operator = Operator::from_seed(19);
    fs::write(
        root.join(".cosmon/takeover.pub"),
        operator.public_key_file(),
    )
    .expect("trust root");
    let challenge = success(
        cs(root)
            .args([
                "sessions",
                "takeover",
                "challenge",
                "--mission",
                mission,
                "--to",
                sid,
                "--by",
                "operator",
            ])
            .output()
            .expect("challenge"),
    );
    let signature = root.join("grant.minisig");
    fs::write(&signature, operator.sign(&challenge.stdout)).expect("signature");
    success(
        cs(root)
            .args([
                "sessions",
                "takeover",
                "grant",
                "--mission",
                mission,
                "--to",
                sid,
                "--by",
                "operator",
                "--attestation",
            ])
            .arg(signature)
            .output()
            .expect("grant"),
    );
}

#[test]
fn hook_takeover_check_and_lifecycle_guard_share_the_resolved_identity() {
    for (name, native_environment) in [("claude", false), ("codex", false), ("codex", true)] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        galaxy(root);
        fs::create_dir_all(root.join(".cosmon/formulas")).expect("formulas");
        fs::write(
            root.join(".cosmon/formulas/flight.formula.toml"),
            r#"
formula = "flight"
version = 1
description = "Identity acceptance fixture"
id_prefix = "task"
[[steps]]
id = "one"
title = "One"
description = "One step"
"#,
        )
        .expect("formula");
        let out = success(
            cs(root)
                .args(["--json", "nucleate", "flight", "--kind", "task"])
                .output()
                .expect("nucleate"),
        );
        let molecule: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("molecule JSON");
        let mission = molecule["id"].as_str().expect("molecule id");
        provider(
            root,
            name,
            r#"
if test "$TEST_NATIVE_ENVIRONMENT" = yes; then
    export CODEX_THREAD_ID=native-codex-fixture
    export CODEX_SESSION_ID=native-codex-fixture
    printf '%s\n' "$CODEX_THREAD_ID"
else
    printf '%s\n' "$COSMON_SESSION_ID"
fi
read -r ready
"$TEST_CS" presence ping --role primary --mission "$TEST_MISSION" --epoch 1 >/dev/null
printf '%s' '{"session_id":"native-claude-fixture","thread-id":"native-codex-fixture"}' | "$TEST_CS" sessions hook run --provider "$TEST_PROVIDER" --event turn-end
"$TEST_CS" sessions takeover check --mission "$TEST_MISSION" --epoch 1
"$TEST_CS" collapse "$TEST_MISSION" --reason 'launch identity accepted'
"#,
        );
        let mut child = launch(root, name)
            .env("TEST_MISSION", mission)
            .env(
                "TEST_NATIVE_ENVIRONMENT",
                if native_environment { "yes" } else { "no" },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("launch");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut sid = String::new();
        stdout.read_line(&mut sid).expect("launch handshake");
        let sid = sid.trim();
        assert!(!sid.is_empty());
        grant(root, mission, sid);
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"ready\n")
            .expect("continue provider");
        success(child.wait_with_output().expect("provider exits"));
        let presence = record(root, sid);
        assert_eq!(
            presence["native_session_id"],
            format!("native-{name}-fixture")
        );
        let observed = success(
            cs(root)
                .args(["--json", "observe", mission])
                .output()
                .expect("observe"),
        );
        let observed: serde_json::Value =
            serde_json::from_slice(&observed.stdout).expect("observation");
        assert_eq!(observed["status"], "collapsed");
        assert!(!root
            .join(".cosmon/state/presence/session-inherited-parent.json")
            .exists());
    }
}

fn hook(root: &Path, provider: &str, worker: bool) {
    let mut cmd = cs(root);
    cmd.args([
        "sessions",
        "hook",
        "run",
        "--provider",
        provider,
        "--event",
        "turn-end",
    ])
    .env("COSMON_SESSION_ID", "inherited-fixture")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if worker {
        cmd.env("COSMON_PARENT_MOL_ID", "task-20261007-abcd")
            .env("COSMON_SESSION_LAUNCH_ID", "stale-pilot-marker")
            .env("CODEX_THREAD_ID", "native-worker-one")
            .env("CODEX_SESSION_ID", "native-worker-two");
    }
    let mut child = cmd.spawn().expect("hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"session_id":"native-claude-fixture","thread-id":"native-codex-fixture"}"#)
        .expect("payload");
    success(child.wait_with_output().expect("hook exit"));
}

#[test]
fn unmarked_pilots_use_native_identity_and_workers_keep_dispatch_identity() {
    for name in ["claude", "codex"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        galaxy(tmp.path());
        hook(tmp.path(), name, false);
        assert_eq!(
            record(tmp.path(), &format!("native-{name}-fixture"))["provider"],
            name
        );
        hook(tmp.path(), name, true);
        assert_eq!(record(tmp.path(), "inherited-fixture")["provider"], name);
    }
}

#[test]
fn hooks_outside_a_galaxy_exit_zero_without_creating_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for name in ["claude", "codex"] {
        hook(tmp.path(), name, false);
    }
    assert_eq!(fs::read_dir(tmp.path()).expect("directory").count(), 0);
}

#[test]
fn an_inherited_state_override_does_not_turn_an_outside_hook_into_a_writer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    let state = tmp.path().join("inherited-state");
    for name in ["claude", "codex"] {
        let mut child = cs(&outside)
            .args([
                "sessions",
                "hook",
                "run",
                "--provider",
                name,
                "--event",
                "turn-end",
            ])
            .env("COSMON_STATE_DIR", &state)
            .env("COSMON_SESSION_ID", "marked-outside")
            .env("COSMON_SESSION_LAUNCH_ID", "marked-outside")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("outside hook");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(
                br#"{"session_id":"native-claude-fixture","thread-id":"native-codex-fixture"}"#,
            )
            .expect("payload");
        success(child.wait_with_output().expect("outside hook exit"));
    }
    assert!(
        !state.exists(),
        "outside hooks must not materialize inherited state"
    );
    assert_eq!(fs::read_dir(outside).expect("outside directory").count(), 0);
}

#[test]
fn concurrent_launches_keep_distinct_presence_rows_without_overwriting_the_first() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    galaxy(root);
    let mut children = Vec::new();
    let mut identities = Vec::new();
    let mut first_snapshot = Vec::new();
    for name in ["claude", "codex"] {
        provider(
            root,
            name,
            r#"
printf '%s' '{"session_id":"native-claude-fixture","thread-id":"native-codex-fixture"}' | "$TEST_CS" sessions hook run --provider "$TEST_PROVIDER" --event turn-end
printf '%s\n' "$COSMON_SESSION_ID"
read -r ready
"#,
        );
        let mut child = launch(root, name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("concurrent launch");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut sid = String::new();
        stdout.read_line(&mut sid).expect("hook finished handshake");
        let sid = sid.trim().to_owned();
        assert!(!sid.is_empty());
        assert_eq!(record(root, &sid)["provider"], name);
        identities.push(sid);
        children.push(child);
        let first_path = root
            .join(".cosmon/state/presence")
            .join(format!("session-{}.json", identities[0]));
        if identities.len() == 1 {
            first_snapshot = fs::read(first_path).expect("first presence bytes");
        } else {
            assert_eq!(
                fs::read(first_path).expect("first presence remains"),
                first_snapshot
            );
            assert_ne!(identities[0], identities[1]);
        }
        for child in &mut children {
            assert!(
                child.try_wait().expect("provider status").is_none(),
                "both providers must remain live during the observation"
            );
        }
    }
    for mut child in children {
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"done\n")
            .expect("finish provider");
        success(child.wait_with_output().expect("provider exit"));
    }
}

#[test]
fn a_new_launch_cannot_heartbeat_the_inherited_previous_holder_row() {
    for name in ["claude", "codex"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        galaxy(root);
        let previous = "previous-holder-fixture";
        success(
            cs(root)
                .args([
                    "presence",
                    "ping",
                    "--session",
                    previous,
                    "--provider",
                    name,
                ])
                .output()
                .expect("seed previous presence"),
        );
        let previous_path = root
            .join(".cosmon/state/presence")
            .join(format!("session-{previous}.json"));
        let before = fs::read(&previous_path).expect("previous presence bytes");
        provider(
            root,
            name,
            r#"
printf '%s' '{"session_id":"native-claude-fixture","thread-id":"native-codex-fixture"}' | "$TEST_CS" sessions hook run --provider "$TEST_PROVIDER" --event turn-end
printf '%s' "$COSMON_SESSION_ID"
"#,
        );
        let out = success(
            launch(root, name)
                .env("COSMON_SESSION_ID", previous)
                .env("COSMON_SESSION_LAUNCH_ID", previous)
                .env("CODEX_THREAD_ID", previous)
                .env("CODEX_SESSION_ID", previous)
                .output()
                .expect("new provider launch"),
        );
        let current = String::from_utf8(out.stdout).expect("launch identity");
        assert_eq!(
            fs::read(&previous_path).expect("previous presence after new heartbeat"),
            before,
            "a new launch must leave the inherited holder's entire row unchanged"
        );
        assert_ne!(current, previous);
        assert_eq!(record(root, &current)["provider"], name);
    }
}

#[test]
fn launcher_clears_both_inherited_codex_native_variables_before_provider_entry() {
    for name in ["claude", "codex"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        provider(
            tmp.path(),
            name,
            r#"
test "${CODEX_THREAD_ID+x}" != x
test "${CODEX_SESSION_ID+x}" != x
"#,
        );
        success(
            launch(tmp.path(), name)
                .env("CODEX_THREAD_ID", "stale-native-thread")
                .env("CODEX_SESSION_ID", "stale-native-session")
                .output()
                .expect("launch with stale native identity"),
        );
    }
}

#[test]
fn codex_native_environment_replaces_the_launch_identity_for_callbacks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    galaxy(root);
    provider(
        root,
        "codex",
        r#"
export CODEX_THREAD_ID=native-codex-fixture
export CODEX_SESSION_ID=native-codex-fixture
printf '%s' '{"thread-id":"native-codex-fixture"}' | "$TEST_CS" sessions hook run --provider codex --event turn-end
printf '%s' "$COSMON_SESSION_ID"
"#,
    );
    let out = success(launch(root, "codex").output().expect("native provider"));
    let launch_sid = String::from_utf8(out.stdout).expect("launch identity");
    let native = record(root, "native-codex-fixture");
    assert_eq!(native["native_session_id"], "native-codex-fixture");
    assert!(
        !root
            .join(".cosmon/state/presence")
            .join(format!("session-{launch_sid}.json"))
            .exists(),
        "native environment must outrank the still-inherited launch marker"
    );
}

#[test]
fn contradictory_codex_native_identity_does_not_publish_presence_or_pass_check() {
    for (thread, session, payload) in [
        ("native-one", "native-two", "native-one"),
        ("native-one", "native-one", "native-other"),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        galaxy(root);
        let mut child = cs(root)
            .args([
                "sessions",
                "hook",
                "run",
                "--provider",
                "codex",
                "--event",
                "turn-end",
            ])
            .env("COSMON_SESSION_ID", "marked-launch")
            .env("COSMON_SESSION_LAUNCH_ID", "marked-launch")
            .env("CODEX_THREAD_ID", thread)
            .env("CODEX_SESSION_ID", session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("contradictory hook");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(
                serde_json::json!({"thread-id": payload})
                    .to_string()
                    .as_bytes(),
            )
            .expect("payload");
        let out = success(child.wait_with_output().expect("hook exit"));
        let presence = root.join(".cosmon/state/presence");
        assert!(
            !presence.exists() || fs::read_dir(presence).expect("presence dir").count() == 0,
            "contradictory identity must not write a presence row"
        );
        assert!(
            !out.stderr.is_empty(),
            "identity refusal must be diagnosable"
        );
        if thread != session {
            let check = cs(root)
                .args([
                    "sessions",
                    "takeover",
                    "check",
                    "--mission",
                    "task-20261007-abcd",
                    "--epoch",
                    "1",
                ])
                .env("COSMON_SESSION_ID", "marked-launch")
                .env("COSMON_SESSION_LAUNCH_ID", "marked-launch")
                .env("CODEX_THREAD_ID", thread)
                .env("CODEX_SESSION_ID", session)
                .output()
                .expect("contradictory check");
            assert!(
                !check.status.success(),
                "conflicting native identities must refuse check"
            );
            let diagnostic = format!(
                "{}{}",
                String::from_utf8_lossy(&check.stdout),
                String::from_utf8_lossy(&check.stderr)
            );
            assert!(
                diagnostic.contains("CODEX_THREAD_ID") && diagnostic.contains("CODEX_SESSION_ID"),
                "check must refuse the native conflict, not an unrelated guard: {diagnostic}"
            );
        }
    }
}
