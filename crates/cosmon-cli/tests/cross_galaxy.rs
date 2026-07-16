// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for cross-galaxy edges (Phase 1 of ADR-035).
//!
//! These exercise the end-to-end CLI flow:
//!
//! 1. `cs nucleate --blocked-by <alias>:<mol_id>` parses the new
//!    syntax, records a `CrossGalaxyBlockedBy` link in the local
//!    `state.json`, and emits a stderr warning when the target galaxy
//!    cannot be reached.
//! 2. `cs deps --json` surfaces the cross-galaxy edge under the
//!    `cross_galaxy_upstream` / `cross_galaxy_downstream` arrays so
//!    operators (and the future runtime) can observe the DAG without
//!    walking remote state.
//! 3. The override file `~/.cosmon/galaxy-aliases.toml` is consulted by
//!    the resolver — verified through the in-process unit tests in
//!    `cosmon_cli::cmd::cross_galaxy::tests`. End-to-end use of the
//!    override file is not exercised here because the binary reads
//!    `$HOME` directly; isolating that requires running the CLI in a
//!    subprocess with `HOME=` overridden, which the `purge` and
//!    `init` test helpers also avoid.

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

const FORMULA_TOML: &str = r#"
formula = "xg-test"
version = 1
description = "Cross-galaxy edges E2E formula"
id_prefix = "xg"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#;

#[test]
fn nucleate_with_cross_galaxy_blocked_by_records_link_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(formulas_dir.join("xg-test.formula.toml"), FORMULA_TOML).unwrap();

    // Nucleate a molecule whose only upstream blocker lives in another
    // galaxy. The galaxy alias is intentionally fictitious so the
    // resolver returns `GalaxyUnknown` — the CLI must still record the
    // edge and exit cleanly with a warning, matching the spec's
    // "Phase 1 best-effort" mode.
    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "xg-test",
            "--var",
            "topic=cross",
            "--blocked-by",
            "test-galaxy-xyz-nonexistent:delib-20260425-39c1",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate with cross-galaxy blocker should run");
    assert!(
        out.status.success(),
        "nucleate failed unexpectedly: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning")
            && stderr.contains("test-galaxy-xyz-nonexistent")
            && stderr.contains("recorded anyway"),
        "expected best-effort reachability warning, got: {stderr}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let new_id = parsed["id"].as_str().unwrap().to_owned();

    let state_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&new_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let typed_links = state_json["typed_links"].as_array().unwrap();
    assert_eq!(typed_links.len(), 1, "exactly one cross-galaxy link");
    assert_eq!(typed_links[0]["rel"], "cross_galaxy_blocked_by");
    assert_eq!(
        typed_links[0]["source"]["galaxy"],
        "test-galaxy-xyz-nonexistent"
    );
    assert_eq!(typed_links[0]["source"]["mol_id"], "delib-20260425-39c1");
}

#[test]
fn nucleate_mixes_local_and_cross_galaxy_blockers() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(formulas_dir.join("xg-test.formula.toml"), FORMULA_TOML).unwrap();

    // Step 1 — create a local blocker.
    let upstream = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "xg-test",
            "--var",
            "topic=upstream",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate upstream failed");
    assert!(upstream.status.success());
    let upstream_id: String =
        serde_json::from_str::<serde_json::Value>(String::from_utf8_lossy(&upstream.stdout).trim())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();

    // Step 2 — nucleate a child that is blocked by BOTH the local
    // upstream and a cross-galaxy reference. Both edges must land.
    let child = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "xg-test",
            "--var",
            "topic=child",
            "--blocked-by",
            &upstream_id,
            "--blocked-by",
            "test-galaxy-xyz-other:delib-20260425-54aa",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate child failed");
    assert!(
        child.status.success(),
        "child nucleate failed: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let child_id: String =
        serde_json::from_str::<serde_json::Value>(String::from_utf8_lossy(&child.stdout).trim())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();

    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            state_dir
                .join("fleets/default/molecules")
                .join(&child_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let links = state["typed_links"].as_array().unwrap();
    assert_eq!(links.len(), 2, "one local + one cross-galaxy edge");
    let has_local = links
        .iter()
        .any(|l| l["rel"] == "blocked_by" && l["source"] == upstream_id);
    let has_cross = links.iter().any(|l| {
        l["rel"] == "cross_galaxy_blocked_by"
            && l["source"]["galaxy"] == "test-galaxy-xyz-other"
            && l["source"]["mol_id"] == "delib-20260425-54aa"
    });
    assert!(has_local, "local BlockedBy missing: {links:?}");
    assert!(has_cross, "cross-galaxy BlockedBy missing: {links:?}");
}

#[test]
fn deps_json_emits_cross_galaxy_arrays() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(formulas_dir.join("xg-test.formula.toml"), FORMULA_TOML).unwrap();

    let nucleated = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "xg-test",
            "--var",
            "topic=src",
            "--blocked-by",
            "test-galaxy-xyz-nonexistent:delib-20260425-39c1",
            "--blocks",
            "test-galaxy-xyz-other@delib-20260425-54aa",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed");
    assert!(
        nucleated.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nucleated.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&nucleated.stdout).trim()).unwrap();
    let mol_id = parsed["id"].as_str().unwrap().to_owned();

    let deps_out = cosmon_bin()
        .args([
            "--json",
            "deps",
            &mol_id,
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("deps failed");
    assert!(
        deps_out.status.success(),
        "deps failed: {}",
        String::from_utf8_lossy(&deps_out.stderr)
    );
    let deps_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&deps_out.stdout).trim()).unwrap();

    let upstream = deps_json["cross_galaxy_upstream"].as_array().unwrap();
    let downstream = deps_json["cross_galaxy_downstream"].as_array().unwrap();
    assert_eq!(upstream.len(), 1);
    assert_eq!(downstream.len(), 1);
    assert_eq!(upstream[0]["galaxy"], "test-galaxy-xyz-nonexistent");
    assert_eq!(upstream[0]["mol_id"], "delib-20260425-39c1");
    assert_eq!(
        upstream[0]["ref"],
        "test-galaxy-xyz-nonexistent:delib-20260425-39c1"
    );
    assert_eq!(downstream[0]["galaxy"], "test-galaxy-xyz-other");
    assert_eq!(downstream[0]["mol_id"], "delib-20260425-54aa");
    // The resolver could not reach the fictitious galaxy on disk, so
    // the resolution must surface as `galaxy_unknown` (or any non-
    // resolved variant if the developer happens to have a real
    // `/srv/cosmon/mailroom/`). We only assert non-resolved here so
    // the test stays robust to local environments.
    assert!(
        upstream[0]["resolution"]["kind"].as_str() != Some("resolved"),
        "expected non-resolved cross-galaxy upstream in test env, got: {}",
        upstream[0]
    );
}

#[test]
fn nucleate_rejects_malformed_cross_galaxy_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(formulas_dir.join("xg-test.formula.toml"), FORMULA_TOML).unwrap();

    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "xg-test",
            "--var",
            "topic=err",
            "--blocked-by",
            "mailroom:not-a-real-id",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate should run");
    assert!(
        !out.status.success(),
        "nucleate must reject malformed cross-galaxy refs"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("cross-galaxy") || combined.contains("invalid molecule id"),
        "error should explain the cross-galaxy parse failure, got: {combined}"
    );
}
