// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit user-driven completion remains available independently of owned
//! worker acceptance.
//!
//! An owned loop now publishes and accepts the current formula step rather
//! than force-completing the formula. These tests pin the separate, unchanged
//! `cs complete` command: starting from a `Running` molecule, an explicit
//! operator invocation must produce:
//!
//! 1. a `Completed` molecule on disk (`state.json`), and
//! 2. a `MoleculeCompleted` row on `events.jsonl`, and
//! 3. a `cs wait <mol> --for completed` invocation that returns within
//!    its timeout (proving the GAP #8 cascade is closed).
//!
//! Formula-bound acceptance has its own end-to-end loopback and lifecycle
//! regressions; this file deliberately covers only explicit completion.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd);
    cmd
}

/// Set up a tempdir with a state store and one nucleated molecule
/// transitioned to `Running`, the state from which explicit completion acts.
fn setup_running_molecule() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "tackle-inprocess-completion-test"
version = 1
description = "One-step formula for the GAP #6 in-process completion test"
id_prefix = "ipc"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step — the in-process agent loop would do the work."
acceptance = "Done"
"#;
    fs::write(
        formulas_dir.join("tackle-inprocess-completion-test.formula.toml"),
        formula_toml,
    )
    .unwrap();

    let state_str = state_dir.to_str().unwrap();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "tackle-inprocess-completion-test",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    // Transition the nucleated (pending) molecule into Running directly
    // via the FileStore — mirrors the state `cs tackle` step 9 commits
    // before a lifecycle completion command acts.
    let store = cosmon_filestore::FileStore::new(&state_dir);
    let mol_id = cosmon_core::id::MoleculeId::new(&molecule_id).unwrap();
    let mut mol = cosmon_state::StateStore::load_molecule(&store, &mol_id).unwrap();
    mol.status = cosmon_core::molecule::MoleculeStatus::Running;
    cosmon_state::StateStore::save_molecule(&store, &mol_id, &mol).unwrap();

    (tmp, state_dir, molecule_id)
}

/// `cs complete` must flip `Running → Completed` and persist the new status on
/// `state.json`. This is GAP #6 part (1) — the state-machine fix.
#[test]
fn complete_flips_running_to_completed_on_disk() {
    let (tmp, state_dir, mol_id) = setup_running_molecule();

    let output = cosmon_bin_in(tmp.path())
        .args([
            "complete",
            &mol_id,
            "--reason",
            "in-process agent loop returned Ok (test)",
            "--ops-dir",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("cs complete failed to spawn");
    assert!(
        output.status.success(),
        "cs complete must succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let store = cosmon_filestore::FileStore::new(&state_dir);
    let mol_typed = cosmon_core::id::MoleculeId::new(&mol_id).unwrap();
    let reloaded = cosmon_state::StateStore::load_molecule(&store, &mol_typed).unwrap();
    assert_eq!(
        reloaded.status,
        cosmon_core::molecule::MoleculeStatus::Completed,
        "molecule.status must be Completed after the canonical \
         completion-emit sequence runs (GAP #6 — state.json fix)"
    );
}

/// `events.jsonl` must contain a `MoleculeCompleted` row after the
/// canonical completion emit. This is GAP #6 part (2) — the
/// event-stream fix that unblocks `cs wait` and `cs ensemble`.
#[test]
fn complete_emits_molecule_completed_on_events_jsonl() {
    let (tmp, state_dir, mol_id) = setup_running_molecule();

    let output = cosmon_bin_in(tmp.path())
        .args([
            "complete",
            &mol_id,
            "--reason",
            "in-process agent loop returned Ok (test)",
            "--ops-dir",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("cs complete failed to spawn");
    assert!(
        output.status.success(),
        "cs complete must succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let events_path = state_dir.join("events.jsonl");
    let events_raw = fs::read_to_string(&events_path).unwrap_or_default();

    // Event rows are flat JSON with a `type` discriminator (snake-case
    // EventV2 variant). The canonical completion sequence emits two
    // V2 rows we care about: `molecule_status_changed` (running →
    // completed) and `molecule_completed` (terminal-state marker).
    // Legacy V1 rows (`molecule_transitioned`, untyped
    // `molecule_completed` via the kind/molecule_id shape) are also
    // present but redundant for this contract; we pin the V2 surface
    // because that is what `cs wait`, `cs ensemble`, and downstream
    // observers consume.
    let rows: Vec<serde_json::Value> = events_raw
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();

    let has_molecule_completed_v2 = rows.iter().any(|row| {
        row.get("type").and_then(|t| t.as_str()) == Some("molecule_completed")
            && row.get("molecule_id").and_then(|id| id.as_str()) == Some(mol_id.as_str())
    });
    assert!(
        has_molecule_completed_v2,
        "events.jsonl must contain a V2 molecule_completed row for {mol_id} \
         after the canonical completion emit (GAP #6 — event-stream fix). \
         Events file:\n{events_raw}"
    );

    let has_status_changed_v2 = rows.iter().any(|row| {
        row.get("type").and_then(|t| t.as_str()) == Some("molecule_status_changed")
            && row.get("molecule_id").and_then(|id| id.as_str()) == Some(mol_id.as_str())
            && row.get("to").and_then(|t| t.as_str()) == Some("completed")
    });
    assert!(
        has_status_changed_v2,
        "events.jsonl must also contain V2 molecule_status_changed → completed \
         for {mol_id} — the canonical sequence is status_changed *then* \
         completed. Events file:\n{events_raw}"
    );
}

/// `cs wait` must return on completion (no longer time out) — this is
/// the GAP #6 → GAP #8 cascade. Pre-fix, `cs wait` would have polled
/// until its timeout because the in-process branch never emitted
/// `MoleculeCompleted`. Post-fix, an explicit completion-emit is the
/// signal `cs wait` is designed to consume.
///
/// We run `cs wait` with a generous timeout (10s) and a tight poll
/// (1s), fire `cs complete` immediately afterwards in the same shell,
/// then assert the wait process exited 0 within the budget.
#[test]
fn cs_wait_unblocks_after_explicit_completion_emit() {
    let (tmp, state_dir, mol_id) = setup_running_molecule();

    // Start cs wait in the background. It will poll every 1s and exit
    // when the molecule reaches Completed.
    let mut wait_child = cosmon_bin_in(tmp.path())
        .args([
            "wait",
            &mol_id,
            "--for",
            "completed",
            "--timeout",
            "10",
            "--poll-interval",
            "1",
            "--quiet",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("cs wait failed to spawn");

    // Give cs wait one poll cycle to attach, then fire the completion.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let complete_output = cosmon_bin_in(tmp.path())
        .args([
            "complete",
            &mol_id,
            "--reason",
            "GAP #6 cascade test — proves cs wait unblocks",
            "--ops-dir",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("cs complete failed to spawn");
    assert!(
        complete_output.status.success(),
        "cs complete must succeed: stderr={}",
        String::from_utf8_lossy(&complete_output.stderr),
    );

    // cs wait should now return within the next poll interval (1s) plus
    // some headroom. The full --timeout 10 is the upper bound — if we
    // hit it, the cascade is still broken.
    let wait_status = wait_child.wait().expect("cs wait child failed to join");
    assert!(
        wait_status.success(),
        "cs wait must exit 0 after MoleculeCompleted lands (GAP #6 → #8 \
         cascade). Exit status: {wait_status:?}"
    );
}
