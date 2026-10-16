// SPDX-License-Identifier: AGPL-3.0-only

//! `cs collapse --key` and `cs collapse-receipt`, end to end through the real
//! binary (issue #207, ADR-187).
//!
//! The concurrency and crash cases run against the library in
//! `cosmon-filestore/tests/keyed_collapse.rs`, where a stage can be held open
//! or crashed; this file pins what a broker sees from the shell: the outcome
//! labels, the exit codes, the receipt bytes, and that the lease refusal is
//! the one an unkeyed `cs collapse` gives.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cosmon_minisign_testkit::Operator;

const KEY: &str = "broker-req-cli-000001";
const REASON: &str = "operator approved the collapse";
const PRIMARY: &str = "claude-primary-0207";
const COPILOT: &str = "codex-copilot-0207";

/// A hermetic `cs`: no ambient session decides who the caller is.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "COSMON_DEFAULT_ADAPTER",
        "COSMON_DEFAULT_MODEL",
        "COSMON_TAKEOVER_PUBKEY",
        "COSMON_STATE_DIR",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
        "ANTHROPIC_MODEL",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn operator() -> Operator {
    Operator::from_seed(207)
}

fn galaxy(tmp: &Path) -> PathBuf {
    let root = tmp.join("galaxy");
    fs::create_dir_all(&root).unwrap();
    assert!(cs(&root).arg("init").status().unwrap().success());
    fs::write(
        root.join(".cosmon").join("takeover.pub"),
        operator().public_key_file(),
    )
    .unwrap();
    let formulas = root.join(".cosmon").join("formulas");
    fs::create_dir_all(&formulas).unwrap();
    fs::write(
        formulas.join("flight.formula.toml"),
        r#"formula = "flight"
version = 1
description = "One step."
id_prefix = "task"

[vars.topic]
description = "The task."
required = true

[[steps]]
id = "one"
title = "One"
description = "The only leg."
"#,
    )
    .unwrap();
    root
}

fn nucleate(root: &Path) -> String {
    let out = cs(root)
        .args([
            "--json",
            "nucleate",
            "flight",
            "--kind",
            "task",
            "--var",
            "topic=fly",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn observe(root: &Path, id: &str) -> serde_json::Value {
    let out = cs(root).args(["--json", "observe", id]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn revision(root: &Path, id: &str) -> String {
    observe(root, id)["revision"]
        .as_str()
        .expect("`cs observe --json` carries the revision")
        .to_owned()
}

fn state_bytes(root: &Path, id: &str) -> Vec<u8> {
    fs::read(
        root.join(".cosmon/state/fleets/default/molecules")
            .join(id)
            .join("state.json"),
    )
    .unwrap()
}

fn keyed(root: &Path, id: &str, reason: &str, status: &str, rev: &str) -> Output {
    cs(root)
        .args(["--json", "collapse", id, "--reason", reason, "--key", KEY])
        .args(["--expect-status", status, "--expect-revision", rev])
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not json ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Canonical bytes of a receipt value, as two callers would compare them.
fn receipt_bytes(v: &serde_json::Value) -> Vec<u8> {
    let receipt: cosmon_core::collapse_contract::CollapseReceipt =
        serde_json::from_value(v["receipt"].clone()).expect("a receipt");
    receipt.canonical_bytes().unwrap()
}

fn events_collapsed(root: &Path, id: &str) -> usize {
    fs::read_to_string(root.join(".cosmon/state/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            l.contains("molecule_status_changed")
                && l.contains(id)
                && l.contains("\"to\":\"collapsed\"")
        })
        .count()
}

#[test]
fn a_keyed_collapse_transitions_once_and_replays_the_same_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let id = nucleate(&root);
    let rev = revision(&root, &id);

    let first = keyed(&root, &id, REASON, "pending", &rev);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = json(&first);
    assert_eq!(first["outcome"], "transitioned");
    assert_eq!(observe(&root, &id)["status"], "collapsed");

    // A lost response: the identical request again, with the revision the
    // caller confirmed before (the collapse moved it since).
    let again = keyed(&root, &id, REASON, "pending", &rev);
    assert!(again.status.success());
    let again = json(&again);
    assert_eq!(again["outcome"], "replayed");
    assert_eq!(receipt_bytes(&first), receipt_bytes(&again));
    assert_eq!(first["digest"], again["digest"]);
    assert_eq!(events_collapsed(&root, &id), 1);

    let lookup = cs(&root)
        .args(["--json", "collapse-receipt", &id, KEY])
        .output()
        .unwrap();
    assert!(lookup.status.success());
    let lookup = json(&lookup);
    assert_eq!(lookup["state"], "committed");
    assert_eq!(receipt_bytes(&lookup), receipt_bytes(&first));
}

#[test]
fn an_unknown_key_reads_back_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let id = nucleate(&root);
    let lookup = cs(&root)
        .args(["--json", "collapse-receipt", &id, KEY])
        .output()
        .unwrap();
    assert!(lookup.status.success());
    assert_eq!(json(&lookup)["state"], "unknown");
}

#[test]
fn a_one_byte_reason_change_or_another_molecule_is_key_reused_with_no_write() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let id = nucleate(&root);
    let other = nucleate(&root);
    let rev = revision(&root, &id);
    assert!(keyed(&root, &id, REASON, "pending", &rev).status.success());
    let (id_before, other_before) = (state_bytes(&root, &id), state_bytes(&root, &other));

    let changed = keyed(&root, &id, &format!("{REASON}!"), "pending", &rev);
    assert_eq!(changed.status.code(), Some(20));
    assert_eq!(json(&changed)["outcome"], "idempotency_key_reused");

    let other_rev = revision(&root, &other);
    let elsewhere = keyed(&root, &other, REASON, "pending", &other_rev);
    assert_eq!(elsewhere.status.code(), Some(20));
    assert_eq!(json(&elsewhere)["outcome"], "idempotency_key_reused");

    assert_eq!(state_bytes(&root, &id), id_before);
    assert_eq!(state_bytes(&root, &other), other_before);
    assert_eq!(observe(&root, &other)["status"], "pending");

    // The other molecule's lookup does not disclose the key's binding.
    let lookup = cs(&root)
        .args(["--json", "collapse-receipt", &other, KEY])
        .output()
        .unwrap();
    assert_eq!(lookup.status.code(), Some(20));
    assert!(json(&lookup)["receipt"].is_null());
}

#[test]
fn stale_revision_and_unexpected_status_have_distinct_exit_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let id = nucleate(&root);
    let stale = revision(&root, &id);
    assert!(cs(&root)
        .args(["tag", &id, "--add", "temp:warm"])
        .status()
        .unwrap()
        .success());
    let current = revision(&root, &id);
    assert_ne!(stale, current, "a tag changes the revision");
    let before = state_bytes(&root, &id);

    let out = keyed(&root, &id, REASON, "pending", &stale);
    assert_eq!(out.status.code(), Some(25));
    assert_eq!(json(&out)["outcome"], "stale_revision");

    let out = keyed(&root, &id, REASON, "running", &current);
    assert_eq!(out.status.code(), Some(24));
    assert_eq!(json(&out)["outcome"], "unexpected_status");

    assert_eq!(state_bytes(&root, &id), before);
}

#[test]
fn key_without_its_conditions_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let id = nucleate(&root);
    let out = cs(&root)
        .args(["collapse", &id, "--reason", REASON, "--key", KEY])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "clap usage error");
    assert_eq!(observe(&root, &id)["status"], "pending");
}

/// Grant `to` the lease on `mission` (the operator's three steps).
fn grant(root: &Path, mission: &str, to: &str) {
    let challenge = cs(root)
        .args(["sessions", "takeover", "challenge", "--mission", mission])
        .args(["--to", to, "--by", "operator"])
        .output()
        .unwrap();
    assert!(
        challenge.status.success(),
        "{}",
        String::from_utf8_lossy(&challenge.stderr)
    );
    let sig = root.join(format!("grant-{to}.minisig"));
    fs::write(&sig, operator().sign(&challenge.stdout)).unwrap();
    let out = cs(root)
        .args(["sessions", "takeover", "grant", "--mission", mission])
        .args(["--to", to, "--by", "operator", "--attestation"])
        .arg(&sig)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The lease refusal of a keyed collapse is the unkeyed one: same exit code,
/// same words, and the guard's own `why` for this caller.
#[test]
fn a_keyed_collapse_gets_the_same_lease_refusal_as_an_unkeyed_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let id = nucleate(&root);
    grant(&root, &id, PRIMARY);
    let rev = revision(&root, &id);

    let unkeyed = cs(&root)
        .env("COSMON_SESSION_ID", COPILOT)
        .args(["collapse", &id, "--reason", REASON])
        .output()
        .unwrap();
    let keyed_out = cs(&root)
        .env("COSMON_SESSION_ID", COPILOT)
        .args(["collapse", &id, "--reason", REASON, "--key", KEY])
        .args(["--expect-status", "pending", "--expect-revision", &rev])
        .output()
        .unwrap();
    assert_eq!(unkeyed.status.code(), Some(16));
    assert_eq!(keyed_out.status.code(), Some(16));
    assert_eq!(
        String::from_utf8_lossy(&unkeyed.stderr),
        String::from_utf8_lossy(&keyed_out.stderr),
        "one guard, one refusal"
    );

    let state = fs::canonicalize(root.join(".cosmon/state")).unwrap();
    let why = cosmon_harvest::pilot_gesture::refuse_unleased_pilot_gesture(
        &state,
        &cosmon_core::id::MoleculeId::new(&id).unwrap(),
        "cs collapse",
        &|k: &str| (k == "COSMON_SESSION_ID").then(|| COPILOT.to_owned()),
    )
    .unwrap_err()
    .downcast::<cosmon_harvest::pilot_gesture::UnleasedPilotGesture>()
    .unwrap()
    .why;
    assert!(
        String::from_utf8_lossy(&keyed_out.stderr).contains(&why),
        "stderr: {}",
        String::from_utf8_lossy(&keyed_out.stderr)
    );
    assert_eq!(observe(&root, &id)["status"], "pending");
    assert!(
        !root.join(".cosmon/state/collapse-receipts").exists(),
        "a refusal reserves nothing"
    );
}
