// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end regression for the `archived ⇒ status.is_terminal()`
//! invariant: detection via `cs verify --invariants` and healing via
//! `cs reconcile --heal-invariants`.
//!
//! The defect this guards against: a molecule torn down out-of-band
//! (e.g. `cs done --force` on a never-completed row) can land on disk
//! as `{archived: true, status: running}` — a *ghost* that keeps
//! rendering as live work. Two claims are locked down:
//!
//! 1. `cs verify --invariants` (fleet-wide) FAILs and exits non-zero
//!    when such a ghost exists, and names it.
//! 2. `cs reconcile --heal-invariants` rewrites the ghost's status to
//!    `collapsed` on disk, after which `cs verify --invariants` PASSes
//!    and exits zero — the acceptance criterion verbatim.
//!
//! The test fabricates the ghost by mutating `state.json` directly (the
//! out-of-band teardown this invariant exists to catch), then drives the
//! real `cs` binary so flag-parsing, persistence, and event emission are
//! all exercised.

use std::fs;
use std::process::Command;

use cosmon_state::StateStore;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_isolated(state_dir: &std::path::Path) -> Command {
    let config_path = state_dir
        .parent()
        .expect("state_dir must live under .cosmon/")
        .join("config.toml");
    let mut cmd = cosmon_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        .current_dir(state_dir);
    cmd
}

/// Throwaway `.cosmon/` layout with the `task-work` formula and a
/// minimal `surfaces.toml` (so `cs reconcile` projects rather than
/// early-returning on the missing-config path).
fn setup_project(tmp: &std::path::Path) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"test-invariant\"\n",
    )
    .unwrap();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&src, formulas_dir.join("task-work.formula.toml"))
        .unwrap_or_else(|e| panic!("copy task-work.formula.toml: {e}"));
    fs::write(
        cosmon_dir.join("surfaces.toml"),
        "[[surface]]\nreferent = \"project.status\"\nkind = \"markdown\"\npath = \"STATUS.md\"\n",
    )
    .unwrap();
    fs::write(
        state_dir.join("fleet.json"),
        "{\"workers\":{},\"repos\":{}}\n",
    )
    .unwrap();
    state_dir
}

fn nucleate_task(state_dir: &std::path::Path, topic: &str) -> String {
    let out = cosmon_bin_isolated(state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("task-work")
        .arg("--var")
        .arg(format!("topic={topic}"))
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate task-work failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["id"].as_str().expect("id").to_owned()
}

/// Force a molecule into the ghost shape on disk: `{archived: true,
/// status: Running}` — exactly what an out-of-band teardown leaves.
fn make_ghost(state_dir: &std::path::Path, id: &str) {
    let store = cosmon_filestore::FileStore::new(state_dir);
    let mol_id = cosmon_core::id::MoleculeId::new(id).unwrap();
    let mut mol = store.load_molecule(&mol_id).unwrap();
    mol.status = cosmon_core::molecule::MoleculeStatus::Running;
    mol.archived = true;
    store.save_molecule(&mol_id, &mol).unwrap();
}

#[test]
fn verify_invariants_detects_and_reconcile_heals_archived_ghost() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    let id = nucleate_task(&state_dir, "ghost candidate");
    make_ghost(&state_dir, &id);

    // 1. Detection — fleet-wide `cs verify --invariants` must FAIL.
    let verify_before = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("verify")
        .arg("--invariants")
        .output()
        .expect("spawn cs verify --invariants");
    assert!(
        !verify_before.status.success(),
        "cs verify --invariants must exit non-zero when a ghost exists"
    );
    let v: serde_json::Value = serde_json::from_slice(&verify_before.stdout).unwrap();
    assert_eq!(v["scope"], "invariants");
    assert_eq!(v["status"], "fail");
    let names: Vec<String> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        names.iter().any(|n| n.contains(&id)),
        "the failing report must name the ghost {id}, got {names:?}"
    );

    // 2. Heal — `cs reconcile --heal-invariants` rewrites status on disk.
    let heal = cosmon_bin_isolated(&state_dir)
        .arg("reconcile")
        .arg("--heal-invariants")
        .output()
        .expect("spawn cs reconcile --heal-invariants");
    assert!(
        heal.status.success(),
        "cs reconcile --heal-invariants failed: {}",
        String::from_utf8_lossy(&heal.stderr)
    );

    // On-disk status is now terminal.
    let store = cosmon_filestore::FileStore::new(&state_dir);
    let mol_id = cosmon_core::id::MoleculeId::new(&id).unwrap();
    let healed = store.load_molecule(&mol_id).unwrap();
    assert!(
        healed.status.is_terminal(),
        "healed molecule must be terminal, got {}",
        healed.status
    );
    assert_eq!(
        healed.status,
        cosmon_core::molecule::MoleculeStatus::Collapsed
    );
    assert!(healed.archived, "heal must not un-archive the molecule");

    // 3. Acceptance — `cs verify --invariants` now PASSes (exit 0).
    let verify_after = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("verify")
        .arg("--invariants")
        .output()
        .expect("spawn cs verify --invariants (after heal)");
    assert!(
        verify_after.status.success(),
        "cs verify --invariants must exit 0 after a heal pass: {}",
        String::from_utf8_lossy(&verify_after.stdout)
    );
    let v2: serde_json::Value = serde_json::from_slice(&verify_after.stdout).unwrap();
    assert_eq!(v2["status"], "pass");
}

#[test]
fn heal_invariants_is_idempotent_on_clean_galaxy() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    // A normal pending molecule — no ghost.
    let _id = nucleate_task(&state_dir, "healthy");

    // Two heal passes in a row both succeed and mutate nothing.
    for _ in 0..2 {
        let out = cosmon_bin_isolated(&state_dir)
            .arg("reconcile")
            .arg("--heal-invariants")
            .output()
            .expect("spawn cs reconcile --heal-invariants");
        assert!(
            out.status.success(),
            "idempotent heal pass failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Fleet-wide invariant audit is clean.
    let verify = cosmon_bin_isolated(&state_dir)
        .arg("verify")
        .arg("--invariants")
        .output()
        .expect("spawn cs verify --invariants");
    assert!(
        verify.status.success(),
        "clean galaxy must pass the invariant audit"
    );
}
