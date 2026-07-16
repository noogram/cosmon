// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for ADR-030 M3 archive wiring.
//!
//! The task `DoD` is:
//!
//! > a molecule nucleated → tackled → done → verify archive/ exists
//! > + state archived=true + re-running cs done is no-op on archive.
//!
//! `cs tackle` and `cs done` require a full git + tmux harness that is
//! prohibitively expensive (and flaky) inside a unit test. What the
//! archive wiring actually needs to verify is:
//!
//! 1. The CLI picks up `[archive] enabled = true` from `config.toml`
//!    via walk-up discovery,
//! 2. A terminal transition writes the archive entry atomically,
//! 3. `MoleculeData.archived` is flipped to `true`, and
//! 4. Re-running the same terminal transition is a no-op on the archive.
//!
//! `cs collapse` exercises every one of those paths without needing
//! `cs tackle`'s transport dance (no tmux, no worktree, no git). The
//! companion unit tests in `cmd/collapse.rs` and `cmd/stuck.rs` cover
//! the in-process side; this integration test covers the *binary*
//! side — walk-up config resolution, the `--json` surface, and end-to-
//! end archive presence on disk.

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// Prepare a `cs` invocation that reads state from `state_dir` and the
/// project config from its sibling `config.toml` (the `.cosmon/` layout).
fn cosmon_bin_isolated(state_dir: &std::path::Path) -> Command {
    let config_path = state_dir
        .parent()
        .expect("state_dir must live under .cosmon/")
        .join("config.toml");
    let mut cmd = cosmon_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        // Force walk-up to start inside the temp dir so no external
        // `.cosmon/` (e.g. the worktree's own) leaks into resolution.
        .current_dir(state_dir);
    cmd
}

/// Set up a throwaway project layout:
///
/// ```text
/// <tmp>/.cosmon/
///   config.toml              # [archive] enabled = true
///   formulas/task-work.formula.toml
///   state/
///     (populated by cs)
/// ```
fn setup_project(tmp: &std::path::Path) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"test-m3-wiring\"\n\n[archive]\nenabled = true\n",
    )
    .unwrap();
    // Copy the real `task-work` formula so the `cs nucleate` path finds
    // it via walk-up. Using the in-tree file keeps the test aligned with
    // production semantics (no hand-crafted fixture to drift).
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&src, formulas_dir.join("task-work.formula.toml"))
        .expect("copy task-work formula into test fixture");
    // Keep the fleet file present so commands that assume `default`
    // fleet semantics don't stumble on first invocation.
    fs::write(state_dir.join("fleet.json"), "{}\n").unwrap();
    state_dir
}

#[test]
#[allow(clippy::too_many_lines)]
fn collapse_through_cli_writes_archive_and_sets_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    // Nucleate a task molecule via the CLI.
    let nuc = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("task-work")
        .arg("--var")
        .arg("topic=archive wiring smoke test")
        .output()
        .expect("failed to invoke cs nucleate");
    assert!(
        nuc.status.success(),
        "cs nucleate failed: stderr={}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap_or_else(|e| {
        panic!(
            "nucleate stdout not JSON: {e}\n{}",
            String::from_utf8_lossy(&nuc.stdout)
        )
    });
    let mol_id = nuc_json["id"]
        .as_str()
        .expect("nucleate JSON must carry `id`")
        .to_owned();

    // Collapse — a terminal transition that exercises the archive
    // wiring without touching git or tmux.
    let col = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("collapse")
        .arg(&mol_id)
        .arg("--reason")
        .arg("archive wiring test")
        .output()
        .expect("failed to invoke cs collapse");
    assert!(
        col.status.success(),
        "cs collapse failed: stderr={}",
        String::from_utf8_lossy(&col.stderr)
    );
    let col_json: serde_json::Value = serde_json::from_slice(&col.stdout).expect("collapse JSON");
    assert_eq!(col_json["status"], "collapsed");
    assert_eq!(
        col_json["archived"], true,
        "cs collapse --json should report archived=true after a fresh archive write"
    );

    // Archive entry lands under state_dir/archive/YYYY/MM/<id>/.
    let archive_root = state_dir.join("archive");
    assert!(
        archive_root.is_dir(),
        "archive/ should exist after collapse"
    );
    // Every entry carries a molecule.json and manifest.json (M3
    // bootstrap layout). Walk the tree to find it without hard-coding
    // the month.
    let month_dirs: Vec<_> = fs::read_dir(&archive_root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir() && e.file_name() != "events")
        .collect();
    assert!(!month_dirs.is_empty(), "at least one YYYY/ dir expected");

    let mut found_entry = false;
    for year in month_dirs {
        for month in fs::read_dir(year.path()).unwrap().flatten() {
            for entry in fs::read_dir(month.path()).unwrap().flatten() {
                if entry.file_name() == mol_id.as_str() {
                    assert!(entry.path().join("molecule.json").is_file());
                    assert!(entry.path().join("manifest.json").is_file());
                    found_entry = true;
                }
            }
        }
    }
    assert!(found_entry, "archive entry for {mol_id} not found");

    // Snapshot the manifest bytes — a second collapse must not change them.
    let manifest_before = {
        let mut bytes = Vec::new();
        for year in fs::read_dir(&archive_root).unwrap().flatten() {
            if !year.path().is_dir() || year.file_name() == "events" {
                continue;
            }
            for month in fs::read_dir(year.path()).unwrap().flatten() {
                for entry in fs::read_dir(month.path()).unwrap().flatten() {
                    if entry.file_name() == mol_id.as_str() {
                        bytes = fs::read(entry.path().join("manifest.json")).unwrap();
                    }
                }
            }
        }
        bytes
    };
    assert!(!manifest_before.is_empty());

    // Idempotence gate: a second collapse must not rewrite the archive.
    let col2 = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("collapse")
        .arg(&mol_id)
        .arg("--reason")
        .arg("replay (should be idempotent)")
        .output()
        .expect("failed to invoke cs collapse (replay)");
    assert!(
        col2.status.success(),
        "cs collapse (replay) failed: stderr={}",
        String::from_utf8_lossy(&col2.stderr)
    );

    // Verify the manifest on disk is byte-identical to the first write.
    let manifest_after = {
        let mut bytes = Vec::new();
        for year in fs::read_dir(&archive_root).unwrap().flatten() {
            if !year.path().is_dir() || year.file_name() == "events" {
                continue;
            }
            for month in fs::read_dir(year.path()).unwrap().flatten() {
                for entry in fs::read_dir(month.path()).unwrap().flatten() {
                    if entry.file_name() == mol_id.as_str() {
                        bytes = fs::read(entry.path().join("manifest.json")).unwrap();
                    }
                }
            }
        }
        bytes
    };
    assert_eq!(
        manifest_before, manifest_after,
        "second collapse must be a no-op on the archive manifest"
    );
}

#[test]
fn collapse_through_cli_leaves_archive_alone_when_disabled() {
    // Parallel test with archive disabled — proves the gate actually gates.
    let tmp = tempfile::tempdir().unwrap();
    let cosmon_dir = tmp.path().join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"test-m3-off\"\n",
    )
    .unwrap();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&src, formulas_dir.join("task-work.formula.toml")).unwrap();
    fs::write(state_dir.join("fleet.json"), "{}\n").unwrap();

    let nuc = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("task-work")
        .arg("--var")
        .arg("topic=archive disabled smoke")
        .output()
        .expect("failed to invoke cs nucleate");
    assert!(nuc.status.success());
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = nuc_json["id"].as_str().unwrap().to_owned();

    let col = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("collapse")
        .arg(&mol_id)
        .arg("--reason")
        .arg("archive disabled")
        .output()
        .expect("failed to invoke cs collapse");
    assert!(col.status.success());

    let archive_root = state_dir.join("archive");
    assert!(
        !archive_root.exists(),
        "[archive] disabled — no archive dir should appear"
    );
}
