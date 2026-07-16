// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the **constellation** molecule kind + `Refines`
//! link surface.
//!
//! Two claims are locked down here:
//!
//! 1. `cs nucleate constellation --kind constellation --var citations="a,b,c"`
//!    produces a molecule that carries exactly three `Refines` edges, one
//!    per citation, and every cited molecule gains a symmetric
//!    `RefinedBy` back-edge. This is the scenario named by the briefing:
//!    the operator does not pass three `--refines` flags; the
//!    comma-separated `citations` variable is the ergonomic surface and
//!    the CLI fans it out.
//!
//! 2. `cs deps <constellation_id>` lists the three cited molecules under
//!    `downstream` — the fil-rouge is visible to the graph walker that
//!    the future resident runtime will consume.
//!
//! The test spawns the `cs` binary (`CARGO_BIN_EXE_cs`) against a throwaway
//! `.cosmon/` layout rather than calling the command functions inline —
//! the flag-parsing + env-plumbing + persistence round-trip is exactly
//! what we want the regression guard to cover.

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
        .expect("state_dir must live under .cosmon/")
        .join("config.toml");
    let mut cmd = cosmon_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        .current_dir(state_dir);
    cmd
}

/// Throwaway `.cosmon/` layout pre-populated with the `task-work` and
/// `constellation` formulas copied from the in-tree files. Using the real
/// formula definitions keeps this test aligned with production semantics.
fn setup_project(tmp: &std::path::Path) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"test-constellation\"\n",
    )
    .unwrap();
    for formula in ["task-work", "constellation"] {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cosmon/formulas")
            .join(format!("{formula}.formula.toml"));
        fs::copy(&src, formulas_dir.join(format!("{formula}.formula.toml")))
            .unwrap_or_else(|e| panic!("copy {formula}.formula.toml: {e}"));
    }
    fs::write(state_dir.join("fleet.json"), "{}\n").unwrap();
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

#[test]
fn constellation_citations_var_emits_three_refines_edges() {
    use cosmon_state::StateStore;
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    // Three plain task molecules to cite — these become the targets of
    // the constellation's `Refines` edges.
    let a = nucleate_task(&state_dir, "first cited molecule");
    let b = nucleate_task(&state_dir, "second cited molecule");
    let c = nucleate_task(&state_dir, "third cited molecule");

    let citations = format!("{a},{b},{c}");
    let nuc = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("constellation")
        .arg("--kind")
        .arg("constellation")
        .arg("--var")
        .arg("pattern=three molecules share a hidden primitive")
        .arg("--var")
        .arg(format!("citations={citations}"))
        .output()
        .expect("spawn cs nucleate constellation");
    assert!(
        nuc.status.success(),
        "cs nucleate constellation failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let const_id = nuc_json["id"]
        .as_str()
        .expect("constellation id")
        .to_owned();

    // Load the constellation from disk and assert its `typed_links` contain
    // one `Refines` edge per citation.
    let store = cosmon_filestore::FileStore::new(&state_dir);
    let parsed_id = cosmon_core::id::MoleculeId::new(&const_id).unwrap();
    let mol = store.load_molecule(&parsed_id).expect("load constellation");
    assert_eq!(
        mol.kind,
        Some(cosmon_core::kind::MoleculeKind::Constellation),
        "constellation molecule must carry kind=Constellation"
    );
    let refines: Vec<String> = mol
        .typed_links
        .iter()
        .filter_map(cosmon_core::interaction::MoleculeLink::refines_target)
        .map(|id| id.as_str().to_owned())
        .collect();
    assert_eq!(
        refines,
        vec![a.clone(), b.clone(), c.clone()],
        "exactly three Refines edges, in citation order"
    );

    // Each citation gains a symmetric `RefinedBy` back-edge.
    for cited in [&a, &b, &c] {
        let cited_id = cosmon_core::id::MoleculeId::new(cited).unwrap();
        let cited_mol = store.load_molecule(&cited_id).expect("load citation");
        let back: Vec<String> = cited_mol
            .typed_links
            .iter()
            .filter_map(cosmon_core::interaction::MoleculeLink::refined_by_source)
            .map(|id| id.as_str().to_owned())
            .collect();
        assert!(
            back.contains(&const_id),
            "citation {cited} must carry RefinedBy back-edge to {const_id}, got {back:?}"
        );
    }

    // `cs deps <constellation> --json` lists every citation under `downstream`.
    let deps = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("deps")
        .arg(&const_id)
        .output()
        .expect("spawn cs deps");
    assert!(
        deps.status.success(),
        "cs deps failed: {}",
        String::from_utf8_lossy(&deps.stderr)
    );
    let deps_json: serde_json::Value = serde_json::from_slice(&deps.stdout).unwrap();
    let downstream: Vec<String> = deps_json["downstream"]
        .as_array()
        .expect("downstream array")
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let mut expected = vec![a, b, c];
    expected.sort();
    let mut got = downstream.clone();
    got.sort();
    assert_eq!(
        got, expected,
        "cs deps must surface the three citations as downstream, got {downstream:?}"
    );
}

#[test]
fn constellation_explicit_refines_flag_also_works() {
    use cosmon_state::StateStore;
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    let a = nucleate_task(&state_dir, "alpha");
    let b = nucleate_task(&state_dir, "beta");

    let nuc = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("constellation")
        .arg("--kind")
        .arg("constellation")
        .arg("--var")
        .arg("pattern=two molecules explicit flag form")
        .arg("--refines")
        .arg(&a)
        .arg("--refines")
        .arg(&b)
        .output()
        .expect("spawn cs nucleate constellation --refines");
    assert!(
        nuc.status.success(),
        "cs nucleate constellation --refines failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let const_id = nuc_json["id"].as_str().unwrap().to_owned();

    let store = cosmon_filestore::FileStore::new(&state_dir);
    let parsed_id = cosmon_core::id::MoleculeId::new(&const_id).unwrap();
    let mol = store.load_molecule(&parsed_id).unwrap();
    let refines: Vec<String> = mol
        .typed_links
        .iter()
        .filter_map(cosmon_core::interaction::MoleculeLink::refines_target)
        .map(|id| id.as_str().to_owned())
        .collect();
    assert_eq!(refines, vec![a, b]);
}

#[test]
fn constellation_rejects_dangling_citation() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    let real = nucleate_task(&state_dir, "real molecule");
    // A syntactically valid molecule id that was never created.
    let ghost = "task-20260422-dead";

    let nuc = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("constellation")
        .arg("--kind")
        .arg("constellation")
        .arg("--var")
        .arg("pattern=dangling reference guard")
        .arg("--var")
        .arg(format!("citations={real},{ghost}"))
        .output()
        .expect("spawn cs nucleate");
    assert!(
        !nuc.status.success(),
        "cs nucleate must refuse a dangling citation — nothing should be persisted"
    );
    let stderr = String::from_utf8_lossy(&nuc.stderr);
    assert!(
        stderr.contains(ghost) || stderr.contains("--refines"),
        "error must point at the offending citation, got: {stderr}"
    );
}
