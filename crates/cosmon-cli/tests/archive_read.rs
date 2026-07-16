// SPDX-License-Identifier: AGPL-3.0-only

//! CLI integration tests for `cs archive list / show / verify` (ADR-030 M3).
//!
//! The `DoD` mandates three integration tests:
//!
//! 1. `list` on an empty archive — exit 0, JSON reports `count: 0`.
//! 2. `list` on a populated archive — one entry nucleated + collapsed
//!    through the CLI shows up in the table and the JSON payload.
//! 3. `verify` — happy path passes, then tampering a response file makes
//!    `verify` exit 1 with a `fail` payload.
//!
//! All three tests drive the real `cs` binary (via `CARGO_BIN_EXE_cs`) so
//! walk-up config resolution, the `--json` surface, and the on-disk
//! archive layout are exercised end-to-end. We reuse the same CLI dance
//! as `tests/archive_wiring.rs`: a temp project with `[archive] enabled
//! = true`, a copy of the real `task-work` formula, and `cs collapse`
//! as the terminal transition (avoids the tmux/worktree dance of
//! `cs tackle` / `cs done`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_isolated(state_dir: &Path) -> Command {
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

fn setup_project(tmp: &Path, archive_enabled: bool) -> PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    let config = if archive_enabled {
        "[project]\nproject_id = \"test-archive-read\"\n\n[archive]\nenabled = true\n"
    } else {
        "[project]\nproject_id = \"test-archive-read-off\"\n"
    };
    fs::write(cosmon_dir.join("config.toml"), config).unwrap();
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&src, formulas_dir.join("task-work.formula.toml"))
        .expect("copy task-work formula into test fixture");
    fs::write(state_dir.join("fleet.json"), "{}\n").unwrap();
    state_dir
}

fn nucleate_and_collapse(state_dir: &Path, topic: &str, reason: &str) -> String {
    let nuc = cosmon_bin_isolated(state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("task-work")
        .arg("--var")
        .arg(format!("topic={topic}"))
        .output()
        .expect("nucleate");
    assert!(
        nuc.status.success(),
        "cs nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = nuc_json["id"].as_str().unwrap().to_owned();

    let col = cosmon_bin_isolated(state_dir)
        .arg("--json")
        .arg("collapse")
        .arg(&mol_id)
        .arg("--reason")
        .arg(reason)
        .output()
        .expect("collapse");
    assert!(
        col.status.success(),
        "cs collapse failed: {}",
        String::from_utf8_lossy(&col.stderr)
    );
    mol_id
}

/// Walk under `.cosmon/state/archive/YYYY/MM/<id>/` and return the full path.
fn find_entry_dir(state_dir: &Path, mol_id: &str) -> PathBuf {
    let root = state_dir.join("archive");
    for year in fs::read_dir(&root).unwrap().flatten() {
        if !year.path().is_dir() || year.file_name() == "events" {
            continue;
        }
        for month in fs::read_dir(year.path()).unwrap().flatten() {
            for entry in fs::read_dir(month.path()).unwrap().flatten() {
                if entry.file_name() == mol_id {
                    return entry.path();
                }
            }
        }
    }
    panic!(
        "archive entry for {mol_id} not found under {}",
        root.display()
    );
}

// ---------------------------------------------------------------------------
// Test 1 — list on an empty archive.
// ---------------------------------------------------------------------------

#[test]
fn archive_list_empty_when_no_terminal_transitions() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), true);

    let out = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("archive")
        .arg("list")
        .output()
        .expect("archive list");
    assert!(
        out.status.success(),
        "cs archive list should succeed on empty archive: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "archive list JSON parse failed: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(v["count"], 0, "empty archive → count:0, got {v}");
    assert!(
        v["entries"].as_array().is_some_and(Vec::is_empty),
        "entries should be empty array"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — list after one terminal transition.
// ---------------------------------------------------------------------------

#[test]
fn archive_list_surfaces_collapsed_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), true);

    let mol_id = nucleate_and_collapse(&state_dir, "archive list populated", "populated test");

    let json_out = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("archive")
        .arg("list")
        .output()
        .expect("archive list --json");
    assert!(json_out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(v["count"], 1, "one collapsed molecule → count:1, got {v}");
    let entries = v["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["molecule_id"], mol_id);
    assert_eq!(entries[0]["status"], "collapsed");
    assert_eq!(entries[0]["formula"], "task-work");

    // Plaintext surface must show the same molecule id (cheap smoke test).
    let plain = cosmon_bin_isolated(&state_dir)
        .arg("archive")
        .arg("list")
        .output()
        .expect("archive list plain");
    assert!(plain.status.success());
    let stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(
        stdout.contains(&mol_id),
        "plain list output should contain molecule id\n{stdout}"
    );
    assert!(
        stdout.contains("collapsed"),
        "plain list output should include status column\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — verify happy + tampered.
// ---------------------------------------------------------------------------

#[test]
fn archive_verify_detects_tamper() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), true);

    let mol_id = nucleate_and_collapse(&state_dir, "archive verify smoke", "verify test");

    // The bare `cs collapse` path doesn't lay down a responses/ dir, so
    // response_hashes is empty on the manifest. Inject a response + a
    // sealed hash by hand so the verify paths have a real hash to
    // recompute — this is what an archive actually looks like after a
    // deliberation molecule's completion (responses/<persona>.md + the
    // matching sha256 in manifest.json::response_hashes).
    let entry_dir = find_entry_dir(&state_dir, &mol_id);
    let responses = entry_dir.join("responses");
    fs::create_dir_all(&responses).unwrap();
    let body = b"# torvalds\n\nack\n";
    fs::write(responses.join("torvalds.md"), body).unwrap();

    let manifest_path = entry_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let hash = {
        let mut h = Sha256::new();
        h.update(body);
        let bytes = h.finalize();
        let mut s = String::new();
        for b in bytes {
            use std::fmt::Write as _;
            write!(s, "{b:02x}").unwrap();
        }
        s
    };
    manifest["response_hashes"] = serde_json::json!({ "torvalds.md": hash });
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    // Happy path — manifest matches disk, verify exits 0.
    let ok = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("archive")
        .arg("verify")
        .arg(&mol_id)
        .output()
        .expect("archive verify (happy)");
    assert!(
        ok.status.success(),
        "verify should pass on untouched archive: stderr={}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(v["status"], "pass");

    // Tamper — rewrite the response bytes and re-run verify.
    fs::write(responses.join("torvalds.md"), b"TAMPERED").unwrap();
    let bad = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("archive")
        .arg("verify")
        .arg(&mol_id)
        .output()
        .expect("archive verify (tampered)");
    assert!(
        !bad.status.success(),
        "verify must exit non-zero after tamper — stderr={}, stdout={}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert_eq!(v["status"], "fail");
    let checks = v["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "torvalds.md" && c["status"] == "FAIL"),
        "tamper row missing from checks: {v}"
    );
}
