// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for `cs archive list` and `cs archive verify`
//! (ADR-030 M5 — CI gate).
//!
//! The `DoD` requires:
//!   * `cs archive list` enumerates molecules archived in the last 7
//!     days so the CI workflow can iterate over them;
//!   * `cs archive verify <id>` exits 0 on a clean entry and non-zero
//!     on an intentional tamper (edit of a `synthesis.md` under
//!     `archive/`).
//!
//! Both surfaces are smoked here via the real `cs` binary so the
//! `archive-verify.yml` workflow and `just archive-verify` target
//! observe identical behaviour to tests.

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_isolated(state_dir: &std::path::Path) -> Command {
    let config_path = state_dir
        .parent()
        .expect("state_dir under .cosmon/")
        .join("config.toml");
    let mut cmd = cosmon_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        .current_dir(state_dir);
    cmd
}

fn setup_project_with_archive(tmp: &std::path::Path) -> (std::path::PathBuf, String) {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"test-m5-verify\"\n\n[archive]\nenabled = true\n",
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
        .arg("topic=archive verify smoke test")
        .output()
        .expect("cs nucleate");
    assert!(nuc.status.success(), "nucleate: {:?}", nuc.stderr);
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = nuc_json["id"].as_str().unwrap().to_owned();

    // Seed a synthesis.md so the archive writer captures both a copied
    // artifact and a sealed hash — this is the happy path the CI gate
    // actively defends against tamper.
    let mol_dir = state_dir.join("fleets/default/molecules").join(&mol_id);
    fs::create_dir_all(&mol_dir).unwrap();
    fs::write(
        mol_dir.join("synthesis.md"),
        "# Synthesis\n\nSealed at archive time.\n",
    )
    .unwrap();

    let col = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("collapse")
        .arg(&mol_id)
        .arg("--reason")
        .arg("smoke test terminal transition")
        .output()
        .expect("cs collapse");
    assert!(col.status.success(), "collapse: {:?}", col.stderr);

    (state_dir, mol_id)
}

#[test]
fn archive_list_prints_recently_archived_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let (state_dir, mol_id) = setup_project_with_archive(tmp.path());

    let ls = cosmon_bin_isolated(&state_dir)
        .arg("archive")
        .arg("list")
        .arg("--ids-only")
        .output()
        .expect("cs archive list");
    assert!(ls.status.success(), "list: {:?}", ls.stderr);
    let stdout = String::from_utf8_lossy(&ls.stdout);
    let ids: Vec<&str> = stdout.lines().collect();
    assert!(
        ids.contains(&mol_id.as_str()),
        "list output must include {mol_id}: {ids:?}"
    );
}

#[test]
fn archive_verify_passes_on_clean_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let (state_dir, mol_id) = setup_project_with_archive(tmp.path());

    let verify = cosmon_bin_isolated(&state_dir)
        .arg("archive")
        .arg("verify")
        .arg(&mol_id)
        .output()
        .expect("cs archive verify");
    assert!(
        verify.status.success(),
        "clean archive must verify PASS; stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(stdout.contains("PASS"), "expected PASS in output: {stdout}");
}

#[test]
fn archive_verify_fails_on_tampered_synthesis() {
    let tmp = tempfile::tempdir().unwrap();
    let (state_dir, mol_id) = setup_project_with_archive(tmp.path());

    // Locate the archive entry and rewrite synthesis.md — the exact
    // scenario the CI DoD calls out: "intentional tamper (edit a
    // synthesis.md in archive/) produces a CI failure with clear diff".
    let archive_root = state_dir.join("archive");
    let mut synth_path: Option<std::path::PathBuf> = None;
    for year in fs::read_dir(&archive_root).unwrap().flatten() {
        if !year.path().is_dir() || year.file_name() == "events" {
            continue;
        }
        for month in fs::read_dir(year.path()).unwrap().flatten() {
            let candidate = month.path().join(&mol_id).join("synthesis.md");
            if candidate.is_file() {
                synth_path = Some(candidate);
            }
        }
    }
    let synth = synth_path.expect("synthesis.md should have been archived");
    fs::write(&synth, "TAMPERED AFTER ARCHIVE\n").unwrap();

    let verify = cosmon_bin_isolated(&state_dir)
        .arg("archive")
        .arg("verify")
        .arg(&mol_id)
        .output()
        .expect("cs archive verify");
    assert!(
        !verify.status.success(),
        "tampered synthesis must exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(stdout.contains("FAIL"), "expected FAIL in output: {stdout}");
    assert!(
        stdout.contains("synthesis.md"),
        "output must point at synthesis.md: {stdout}"
    );
}
