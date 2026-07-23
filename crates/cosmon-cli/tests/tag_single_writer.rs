// SPDX-License-Identifier: AGPL-3.0-only

//! Single-writer integration test.
//!
//! The library-first promotion of `tag`
//! introduces the **first** in-process state writer in cosmon. This test
//! exercises the worst-case interleave the architecture invariants
//! warn about: an in-process call to [`cosmon_state::ops::tag`] running
//! concurrently with a `cs tag` subprocess against the same `state.json`.
//!
//! # What "single-writer" means here
//!
//! The acceptance criterion of T3 is **file-level integrity under
//! concurrent writes**. After both writers complete, `state.json`
//! must still be a parseable JSON document — never torn, never empty,
//! never half-overwritten. That property is what the atomic
//! tempfile + rename pattern in `cosmon-filestore::atomic_write`
//! guarantees on POSIX, and the test reifies it.
//!
//! # What this test does *not* assert
//!
//! It does **not** assert linearisability (lost-update freedom). The
//! load-mutate-save pattern in [`cosmon_state::ops::tag`] uses no lock
//! today; two concurrent writers can both read the pre-state, mutate
//! their copy, and save in sequence — the second writer's save
//! overwrites the first's tag. That is identical to the legacy
//! all-subprocess world (two `cs tag` calls race in the same way), so
//! T3 does **not** worsen the semantics. The eventual fix — `flock` on
//! `state.json` — is recorded in `tag-before-after.md` §verdict and
//! filed as a successor task; it is **not** blocking T3 acceptance.
//!
//! # Why this lives in `cosmon-cli`'s integration tests
//!
//! The `cs` binary is built by the cli crate and made available to the
//! crate's integration tests via `env!("CARGO_BIN_EXE_cs")`. Putting
//! the test in `cosmon-state` would force a cycle (state crate
//! depending on the cli binary). Putting it here keeps the dependency
//! direction clean.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_state::ops::tag as ops_tag;
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

/// Build a fresh molecule on disk under the default fleet so both
/// the in-process [`ops_tag`] and the `cs tag` subprocess can address
/// it through their respective discovery paths.
fn seed_molecule(state_dir: &Path, id: &str) -> MoleculeId {
    let store = FileStore::new(state_dir);
    let mol_id = MoleculeId::new(id).unwrap();
    let mol = MoleculeData {
        id: mol_id.clone(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Pending,
        variables: HashMap::new(),
        assigned_worker: Some(WorkerId::new("ruby").unwrap()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: vec![],
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: Vec::new(),
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
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
        adapter: None,
    }; // intentional newline to keep imports sorted above

    store.save_molecule(&mol_id, &mol).unwrap();
    mol_id
}

/// Drive the `cs tag` subprocess once. Errors return a panic with the
/// captured stderr because a non-success exit indicates the test
/// scaffolding (not the property under test) is broken.
fn cs_tag_subprocess(state_dir: &Path, mol_id: &str, add: &[&str]) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Steer the subprocess towards the test's tempdir state, not
        // the developer's local cosmon (walk-up discovery would
        // otherwise climb out of `target/debug/...` toward `/srv/cosmon/cosmon`).
        .arg("--config")
        .arg(state_dir)
        .arg("tag")
        .arg(mol_id);
    for t in add {
        cmd.arg("--add").arg(t);
    }
    let out = cmd.output().expect("spawn cs");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!("cs tag failed: status={:?} stderr={stderr}", out.status);
    }
}

/// Cross-process file-integrity test. Two concurrent writers, two add
/// operations each, against the same `state.json`. After both join,
/// the file is parsed and we assert it is a valid `MoleculeData`
/// document.
#[test]
fn single_writer_in_process_vs_subprocess_keeps_state_valid() {
    let tmp = TempDir::new().unwrap();
    let state_dir = Arc::new(tmp.path().to_path_buf());
    let mol_id = seed_molecule(&state_dir, "task-20260503-22ca");

    // The ops::tag function emits AuthzDecisionEvaluated to a NDJSON
    // sink under `{state_dir}/instrumentation/`; isolate to keep the
    // test self-contained (no global env mutation across runs).
    std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

    let dir_a = Arc::clone(&state_dir);
    let id_a = mol_id.clone();
    let in_proc = thread::spawn(move || {
        let store = FileStore::new(&*dir_a);
        let t1 = Tag::new("ip:one").unwrap();
        let t2 = Tag::new("ip:two").unwrap();
        let _ = ops_tag(
            &store,
            &dir_a,
            "operator",
            &id_a,
            std::slice::from_ref(&t1),
            &[],
        )
        .unwrap();
        let _ = ops_tag(
            &store,
            &dir_a,
            "operator",
            &id_a,
            std::slice::from_ref(&t2),
            &[],
        )
        .unwrap();
    });

    let dir_b = Arc::clone(&state_dir);
    let id_b = mol_id.as_str().to_owned();
    let subproc = thread::spawn(move || {
        cs_tag_subprocess(&dir_b, &id_b, &["sp:one"]);
        cs_tag_subprocess(&dir_b, &id_b, &["sp:two"]);
    });

    in_proc.join().unwrap();
    subproc.join().unwrap();

    // --- Property 1: state.json is parseable JSON --------------------
    let store = FileStore::new(&*state_dir);
    let final_mol = store
        .load_molecule(&mol_id)
        .expect("state.json must remain valid + loadable after concurrent writers");
    assert_eq!(final_mol.id, mol_id);

    // --- Property 2: at least one tag from each writer survives -----
    //
    // This is the *aspirational* invariant: under perfect linearisability
    // every add would land. Under load-mutate-save without a lock, the
    // last writer can blow away the first writer's contribution. The
    // assertion below does *not* fail the test — it logs the surviving
    // tag set so the operator can read the verdict in CI output.
    let names: BTreeSet<&str> = final_mol.tags.iter().map(Tag::as_str).collect();
    eprintln!("tag-single-writer surviving tags: {names:?}");

    // The empty case would mean every add was either rejected or
    // silently dropped — that *is* a real bug. The atomic-rename
    // contract guarantees at minimum the *last* writer's tags land,
    // and each writer adds ≥1 tag, so the post-condition is
    // `tags.len() >= 1`.
    assert!(
        !final_mol.tags.is_empty(),
        "the last writer's tag must always land"
    );

    // --- Property 3: no torn JSON --------------------------------------
    //
    // We re-read the raw file and parse it through serde to catch the
    // pathological case where `load_molecule` would silently fall back
    // to defaults. This double-check is cheap and pinpoints atomic-write
    // regressions immediately.
    let path = state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(mol_id.as_str())
        .join("state.json");
    let raw = std::fs::read_to_string(&path).expect("state.json readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("state.json must be parseable JSON, never torn");
    assert!(parsed.is_object(), "state.json root must be an object");
    assert_eq!(
        parsed.get("id").and_then(|v| v.as_str()),
        Some(mol_id.as_str()),
        "state.json id field must match the seeded molecule"
    );
}

/// Tighter sequential round-trip — same machinery, no concurrency, to
/// pin the wire-format compatibility between in-process and subprocess
/// writers. If this fails the test scaffolding (binary path, --config
/// resolution, etc.) is broken, regardless of the concurrency story.
#[test]
fn sequential_in_process_then_subprocess_round_trips() {
    let tmp = TempDir::new().unwrap();
    let state_dir = tmp.path().to_path_buf();
    let mol_id = seed_molecule(&state_dir, "task-20260503-22cb");
    std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

    // Step 1: in-process add.
    let store = FileStore::new(&state_dir);
    let t = Tag::new("temp:hot").unwrap();
    ops_tag(
        &store,
        &state_dir,
        "operator",
        &mol_id,
        std::slice::from_ref(&t),
        &[],
    )
    .unwrap();

    // Step 2: subprocess add of a different tag.
    cs_tag_subprocess(&state_dir, mol_id.as_str(), &["temp:warm-promoted"]);

    // Step 3: load and verify both tags landed (no concurrency = no loss).
    let mol = store.load_molecule(&mol_id).unwrap();
    let names: BTreeSet<&str> = mol.tags.iter().map(Tag::as_str).collect();
    assert!(
        names.contains("temp:hot"),
        "in-process tag missing: {names:?}"
    );
    assert!(
        names.contains("temp:warm-promoted"),
        "subprocess tag missing: {names:?}"
    );
}
