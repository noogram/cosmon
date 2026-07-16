// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test — `cs complete --override-mindguard-down` and the
//! red-light `MindguardRefused` path, end-to-end through the CLI.
//!
//! Built on top of a real git repo + a real `FileStore` so the gate
//! exercises the same code path as production. The ledger lives in a
//! per-test tempdir via `$COSMON_MINDGUARD_OVERRIDE_LEDGER` so the
//! developer's `~/.cosmon/audit/` is never touched.
//!
//! Covers briefing acceptance criteria #1 and #2.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    // The integration runner itself may be a cosmon worker (parent
    // task-20260527-f835); clear inheritance so `cs nucleate` does
    // not try to auto-link to a molecule absent from our per-test
    // state store.
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_RUNTIME_ACTIVE");
    cmd
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Set up a project with: a git repo on `main`, a `.cosmon/state/`
/// directory holding one Pending molecule, and an HTML file committed
/// on a feature branch so `git diff main...HEAD` flags
/// `surface=touched`.
///
/// Returns `(project_root, state_dir, molecule_id)`.
fn setup_surface_touched_project() -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().to_path_buf();

    git(&project, &["init", "-q", "--initial-branch=main"]);
    git(&project, &["config", "user.email", "test@local"]);
    git(&project, &["config", "user.name", "mindguard-test"]);
    git(&project, &["config", "commit.gpgsign", "false"]);
    git(&project, &["commit", "--allow-empty", "-q", "-m", "seed"]);

    let state_dir = project.join(".cosmon").join("state");
    let formulas_dir = project.join(".cosmon").join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "surface-touching"
version = 1
description = "One-step formula for the mindguard integration test"
id_prefix = "stc"

[[steps]]
id = "only"
title = "Only step"
description = "Solo step touching the visual surface"
acceptance = "done"
"#;
    fs::write(
        formulas_dir.join("surface-touching.formula.toml"),
        formula_toml,
    )
    .unwrap();

    let nucleate = cs()
        .args([
            "--json",
            "nucleate",
            "surface-touching",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .current_dir(&project)
        .output()
        .expect("cs nucleate");
    assert!(
        nucleate.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nucleate.stderr)
    );
    let nuc_json: serde_json::Value =
        serde_json::from_slice(&nucleate.stdout).expect("nucleate JSON");
    let mol_id = nuc_json["id"].as_str().expect("nuc[id]").to_owned();

    // Touch the surface with a real HTML commit on a feature branch.
    git(&project, &["checkout", "-q", "-b", "feat/touch-surface"]);
    fs::write(project.join("deck.html"), "<html><body/></html>").unwrap();
    git(&project, &["add", "deck.html"]);
    git(&project, &["commit", "-q", "-m", "touch deck"]);

    (tmp, state_dir, mol_id)
}

/// Acceptance criterion #1 — `cs complete` refuses with
/// `MindguardRefused` when the molecule touched the visual surface but
/// no `verify-surface` molecule has landed GREEN.
#[test]
fn cs_complete_refuses_on_surface_touched_without_verify() {
    let (_tmp, state_dir, mol_id) = setup_surface_touched_project();

    let out = cs()
        .args([
            "complete",
            &mol_id,
            "--ops-dir",
            state_dir.to_str().unwrap(),
            "--reason",
            "test attempt",
        ])
        .output()
        .expect("cs complete");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The CLI itself returns 0 (per-molecule errors are reported on the
    // stderr/JSON channel, the outer Ok is unchanged) — we assert the
    // gate fired by checking the stderr banner and confirming the
    // molecule did NOT transition to Completed.
    assert!(
        stderr.contains("mindguard")
            && (stderr.contains("refused") || stderr.contains("verify-surface")),
        "expected mindguard refusal banner, got stderr:\n{stderr}"
    );

    // State must still be Pending — the gate refused the transition.
    let store = cosmon_filestore::FileStore::new(&state_dir);
    let mol_typed = cosmon_core::id::MoleculeId::new(&mol_id).unwrap();
    let reloaded = cosmon_state::StateStore::load_molecule(&store, &mol_typed).unwrap();
    assert_ne!(
        reloaded.status,
        cosmon_core::molecule::MoleculeStatus::Completed,
        "molecule must NOT be Completed when the gate refused"
    );
}

/// Acceptance criterion #2 — `--override-mindguard-down --justification`
/// proceeds when the mindguard is unavailable AND writes a record to
/// the append-only ledger.
///
/// We force `Unavailable` (not `Refused`) by pointing the config loader
/// at a malformed TOML file via `$COSMON_MINDGUARD_SURFACE_CONFIG` —
/// the only `Unavailable` path the override flag is allowed to bypass.
#[test]
fn cs_complete_override_writes_ledger_when_mindguard_down() {
    let (_tmp, state_dir, mol_id) = setup_surface_touched_project();

    // Per-test ledger and bad config, in their own tempdirs.
    let env_tmp = tempfile::tempdir().unwrap();
    let ledger_path = env_tmp.path().join("audit/mindguard-overrides.jsonl");
    let bad_config = env_tmp.path().join("bad-mindguard.toml");
    fs::write(&bad_config, "[[[ not valid toml").unwrap();

    let out = cs()
        .env("COSMON_MINDGUARD_SURFACE_CONFIG", &bad_config)
        .env("COSMON_MINDGUARD_OVERRIDE_LEDGER", &ledger_path)
        .args([
            "complete",
            &mol_id,
            "--ops-dir",
            state_dir.to_str().unwrap(),
            "--reason",
            "test override",
            "--override-mindguard-down",
            "--justification",
            "gate machinery itself broken in this integration test",
        ])
        .output()
        .expect("cs complete --override-mindguard-down");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "cs complete --override should succeed; stdout={stdout} stderr={stderr}"
    );

    // Ledger exists and has exactly one JSONL record.
    let ledger = fs::read_to_string(&ledger_path)
        .unwrap_or_else(|e| panic!("ledger not written at {}: {e}", ledger_path.display()));
    let lines: Vec<&str> = ledger.lines().collect();
    assert_eq!(lines.len(), 1, "expected one ledger line, got {lines:?}");
    let record: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(record["gate"], "surface_visual");
    assert_eq!(record["molecule_id"], mol_id);
    assert!(record["justification"]
        .as_str()
        .unwrap_or("")
        .contains("broken"));

    // Molecule transitioned to Completed (override worked).
    let store = cosmon_filestore::FileStore::new(&state_dir);
    let mol_typed = cosmon_core::id::MoleculeId::new(&mol_id).unwrap();
    let reloaded = cosmon_state::StateStore::load_molecule(&store, &mol_typed).unwrap();
    assert_eq!(
        reloaded.status,
        cosmon_core::molecule::MoleculeStatus::Completed,
        "override must transition the molecule"
    );
}

/// `--override-mindguard-down` without `--justification` is rejected at
/// argument-parsing time. The override must always be auditable.
#[test]
fn cs_complete_override_requires_justification() {
    let (_tmp, state_dir, mol_id) = setup_surface_touched_project();

    let out = cs()
        .args([
            "complete",
            &mol_id,
            "--ops-dir",
            state_dir.to_str().unwrap(),
            "--override-mindguard-down",
        ])
        .output()
        .expect("cs complete --override without justification");
    assert!(
        !out.status.success(),
        "must reject --override-mindguard-down without --justification"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("justification"),
        "expected mention of --justification, got stderr:\n{stderr}"
    );
}
