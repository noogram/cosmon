// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end integration test for `cs verify-graph`.
//!
//! Exercises the full binary against a throwaway `.cosmon/` fixture so
//! that flag parsing, persistence round-trip, NDJSON output, and exit
//! codes are all locked down. Three flavours:
//!
//! 1. Clean DAG → exit 0, every relation reported PASS.
//! 2. Adversarial three-node `Blocks` cycle → exit 1, JSON marks the
//!    relation as `fail` and lists the cycle.
//! 3. `Refines` cycle alone → exit 0 (cycles permitted), but the
//!    relation is reported as `warn`.

use std::fs;
use std::process::Command;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::molecule_class::MoleculeClass;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};
use std::collections::{BTreeSet, HashMap};

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

fn setup_project(tmp: &std::path::Path) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"verify-graph-test\"\n",
    )
    .unwrap();
    fs::write(state_dir.join("fleet.json"), "{}\n").unwrap();
    state_dir
}

fn write_mol(store: &FileStore, id: &str, links: Vec<MoleculeLink>) {
    let mol = MoleculeData {
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Pending,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: MoleculeClass::default(),
        typed_links: links,
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        pending_step: None,
        merged_at: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    };
    store.save_molecule(&mol.id, &mol).expect("save molecule");
}

#[test]
fn clean_dag_exits_zero_with_pass_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());
    let store = FileStore::new(&state_dir);

    // Linear chain: a → b → c.
    write_mol(
        &store,
        "task-20260509-aaaa",
        vec![MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260509-bbbb").unwrap(),
        }],
    );
    write_mol(
        &store,
        "task-20260509-bbbb",
        vec![
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260509-aaaa").unwrap(),
            },
            MoleculeLink::Blocks {
                target: MoleculeId::new("task-20260509-cccc").unwrap(),
            },
        ],
    );
    write_mol(
        &store,
        "task-20260509-cccc",
        vec![MoleculeLink::BlockedBy {
            source: MoleculeId::new("task-20260509-bbbb").unwrap(),
        }],
    );

    let out = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("verify-graph")
        .arg("--all")
        .output()
        .expect("spawn cs verify-graph");
    assert!(
        out.status.success(),
        "expected exit 0 on clean DAG, got {}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("ndjson row"))
        .collect();
    // One row per registered relation kind.
    assert!(rows.len() >= 4, "expected one row per kind, got {rows:?}");
    let blocks = rows
        .iter()
        .find(|r| r["relation"] == "blocks")
        .expect("blocks row");
    assert_eq!(blocks["status"], "pass");
    assert_eq!(blocks["edges"], 2);
    assert_eq!(blocks["vertices"], 3);
    assert!(blocks["cycles"].as_array().unwrap().is_empty());
}

#[test]
fn three_node_blocks_cycle_fails_with_exit_one() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());
    let store = FileStore::new(&state_dir);

    // Adversarial fixture: known cycle of size 3.
    let a = "task-20260509-1111";
    let b = "task-20260509-2222";
    let c = "task-20260509-3333";
    write_mol(
        &store,
        a,
        vec![MoleculeLink::Blocks {
            target: MoleculeId::new(b).unwrap(),
        }],
    );
    write_mol(
        &store,
        b,
        vec![MoleculeLink::Blocks {
            target: MoleculeId::new(c).unwrap(),
        }],
    );
    write_mol(
        &store,
        c,
        vec![MoleculeLink::Blocks {
            target: MoleculeId::new(a).unwrap(),
        }],
    );

    let out = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("verify-graph")
        .arg("--relation")
        .arg("blocks")
        .output()
        .expect("spawn cs verify-graph");
    assert!(
        !out.status.success(),
        "expected exit 1 on Blocks cycle, got success\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(1));

    let row: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .expect("single ndjson row");
    assert_eq!(row["relation"], "blocks");
    assert_eq!(row["status"], "fail");
    assert_eq!(row["dag_required"], true);
    let cycles = row["cycles"].as_array().expect("cycles array");
    assert_eq!(cycles.len(), 1);
    let cycle: Vec<String> = cycles[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(cycle.len(), 3);
    for id in [a, b, c] {
        assert!(cycle.contains(&id.to_owned()), "cycle missing {id}");
    }
}

#[test]
fn refines_cycle_warns_but_exits_zero() {
    // Two constellations citing each other — legitimate per the
    // RelationKind::is_dag_required(false) policy.
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());
    let store = FileStore::new(&state_dir);

    let a = "task-20260509-aaaa";
    let b = "task-20260509-bbbb";
    write_mol(
        &store,
        a,
        vec![MoleculeLink::Refines {
            target: MoleculeId::new(b).unwrap(),
        }],
    );
    write_mol(
        &store,
        b,
        vec![MoleculeLink::Refines {
            target: MoleculeId::new(a).unwrap(),
        }],
    );

    let out = cosmon_bin_isolated(&state_dir)
        .arg("--json")
        .arg("verify-graph")
        .arg("--relation")
        .arg("refines")
        .output()
        .expect("spawn cs verify-graph");
    assert!(
        out.status.success(),
        "Refines cycle must NOT flip exit code (cycles permitted), got {:?}\nstdout: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout)
    );
    let row: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(row["relation"], "refines");
    assert_eq!(row["status"], "warn");
    assert_eq!(row["dag_required"], false);
    assert_eq!(row["cycles"].as_array().unwrap().len(), 1);
}

#[test]
fn unknown_relation_is_a_clean_error() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());
    let _store = FileStore::new(&state_dir);

    let out = cosmon_bin_isolated(&state_dir)
        .arg("verify-graph")
        .arg("--relation")
        .arg("oversee") // not yet a registered kind
        .output()
        .expect("spawn cs verify-graph");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("oversee") && stderr.contains("blocks"),
        "stderr should name the bad input and list known kinds, got: {stderr}"
    );
}

#[test]
fn missing_args_is_a_clean_error() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path());

    let out = cosmon_bin_isolated(&state_dir)
        .arg("verify-graph")
        .output()
        .expect("spawn cs verify-graph");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--relation") || stderr.contains("--all"),
        "stderr should mention the required flags, got: {stderr}"
    );
}
