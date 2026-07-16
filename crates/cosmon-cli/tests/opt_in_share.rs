// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `cs opt-in-share`.
//!
//! Exercise the three canonical surfaces: (1) first-run on a non-TTY pipe
//! auto-records a decline, (2) `--status` emits the current record, and
//! (3) a second invocation is a no-op (already decided). Each test scopes
//! `COSMON_CONFIG_HOME` to its own tempdir so tests never collide with the
//! developer's real `~/.config/cosmon/consent.toml`.

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cs"))
}

#[test]
fn first_run_without_tty_records_decline() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .arg("opt-in-share")
        .output()
        .expect("spawn cs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "cs opt-in-share failed: {stdout}");

    let consent_path = tmp.path().join("cosmon/consent.toml");
    assert!(
        consent_path.exists(),
        "consent.toml should be created: {}",
        consent_path.display()
    );

    let body = fs::read_to_string(&consent_path).expect("read consent.toml");
    assert!(
        body.contains("declined_at"),
        "expected declined_at, got:\n{body}"
    );
    assert!(
        !body.contains("accepted_at"),
        "must not contain accepted_at on non-tty"
    );
    assert!(body.contains("version = 1"));
}

#[test]
fn status_without_record_reports_deny_by_default() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .args(["opt-in-share", "--status"])
        .output()
        .expect("spawn cs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "cs opt-in-share --status failed");
    assert!(
        stdout.contains("deny-by-default"),
        "expected 'deny-by-default' in stdout, got:\n{stdout}"
    );
}

#[test]
fn explicit_accept_persists_accepted_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .args(["opt-in-share", "--accept"])
        .output()
        .expect("spawn cs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "cs opt-in-share --accept failed: {stdout}"
    );

    let body =
        fs::read_to_string(tmp.path().join("cosmon/consent.toml")).expect("read consent.toml");
    assert!(
        body.contains("accepted_at"),
        "expected accepted_at, got:\n{body}"
    );
    assert!(!body.contains("declined_at"));
}

#[test]
fn second_invocation_is_noop_on_already_decided() {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    // First call: persist a decline.
    let first = cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .args(["opt-in-share", "--decline"])
        .output()
        .expect("spawn cs");
    assert!(first.status.success());

    let path = tmp.path().join("cosmon/consent.toml");
    let first_body = fs::read_to_string(&path).expect("read first");

    // Second call without any flag: must not prompt, must not mutate.
    let second = cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .arg("opt-in-share")
        .output()
        .expect("spawn cs");
    assert!(second.status.success());
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("already declined"),
        "expected 'already declined' in stdout, got:\n{stdout}"
    );

    let second_body = fs::read_to_string(&path).expect("read second");
    assert_eq!(first_body, second_body, "consent file must be untouched");
}

#[test]
fn json_status_renders_structured_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .args(["opt-in-share", "--decline"])
        .output()
        .expect("spawn cs");

    let output = cosmon_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .args(["--json", "opt-in-share", "--status"])
        .output()
        .expect("spawn cs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "json status failed: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["command"], "opt-in-share");
    assert_eq!(v["mode"], "status");
    assert_eq!(v["recorded"], true);
    assert_eq!(v["accepted"], false);
}
