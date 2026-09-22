// SPDX-License-Identifier: AGPL-3.0-only

//! `cs status` must carry the staleness signals a session needs at the moment
//! it opens.
//!
//! Four properties, one test each, all asserted on `--json` so the assertion
//! is about the fact and not about the colour of the glyph that renders it:
//!
//! 1. **Age.** The oldest backlog molecule and how many are past the
//!    threshold. The same fixture aged over and under 48h must flip the
//!    count — which is what pins it to a threshold rather than to "it
//!    happened to be old".
//! 2. **Honest freshness.** `surfaces.up_to_date` says nothing about when the
//!    projection ran, so reconcile age is its own field and a project that
//!    has never reconciled reads as stale, not as clean.
//! 3. **A lease is not backlog.** A molecule named by the pilot-lease ledger
//!    is excluded from every backlog counter — discovered from the ledger,
//!    never from its id.
//! 4. **Unmerged growth.** The gauge keeps a sample so the count's movement
//!    can be reported, not only its level.
//!
//! These exist because each signal was absent once and cost something: a
//! molecule pending 39 days behind `4 alive`, a `✅` beside a 19-day-old
//! reconcile, `28 🔀 to merge` that had been `20` the same afternoon, and a
//! lease counted as work forever.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{Duration, Utc};
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore};

/// A pending molecule nucleated `age` ago.
fn pending(id: &str, age: Duration) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status: MoleculeStatus::Pending,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now() - age,
        updated_at: Utc::now() - age,
        total_steps: 2,
        current_step: 0,
        completed_steps: Vec::new(),
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
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        base_branch: None,
        pending_step: None,
        merged_at: None,
        non_integration: None,
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
    }
}

/// Lay down a state dir with the given molecules.
fn seed(state_dir: &Path, molecules: &[MoleculeData]) {
    let store = FileStore::new(state_dir);
    store.save_fleet(&Fleet::default()).expect("save fleet");
    for mol in molecules {
        store.save_molecule(&mol.id, mol).expect("save molecule");
    }
}

/// Declare a molecule a lease mission by giving it a ledger, exactly as the
/// live mechanism does. The content is irrelevant to the question "is this a
/// cockpit mission" — its *existence* is the declaration.
fn declare_lease(state_dir: &Path, id: &str) {
    let dir = state_dir.join("pilot-lease");
    fs::create_dir_all(&dir).expect("create lease dir");
    fs::write(dir.join(format!("{id}.grants.jsonl")), "").expect("write ledger");
}

/// Run `cs status --json` against a state dir, from a cwd with no git repo so
/// the contribution probe finds nothing and cannot depend on the harness's
/// own branches.
fn status_json(tmp: &Path, state_dir: &Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "--config",
            state_dir.to_str().expect("utf-8 state dir"),
            "status",
        ])
        .output()
        .expect("run cs status");
    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cs status --json emits JSON")
}

/// Signal 1 — the age of the oldest actionable molecule, and the count past
/// the threshold, with the same fixture on both sides of the boundary.
#[test]
fn backlog_age_flips_on_the_threshold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");

    seed(
        &state_dir,
        &[pending("task-20260101-aaaa", Duration::days(39))],
    );
    let over = status_json(tmp.path(), &state_dir);
    assert_eq!(over["backlog"]["count"], 1);
    assert_eq!(over["backlog"]["stale"], 1, "39d must read as stale");
    assert_eq!(over["backlog"]["oldest_id"], "task-20260101-aaaa");
    assert!(
        over["backlog"]["oldest_age_seconds"]
            .as_i64()
            .expect("an age")
            > Duration::days(38).num_seconds(),
        "the age must be the molecule's, not a placeholder"
    );

    // Same fixture, under the threshold.
    let fresh = tempfile::tempdir().expect("tempdir");
    let fresh_state = fresh.path().join("state");
    seed(
        &fresh_state,
        &[pending("task-20260101-aaaa", Duration::hours(1))],
    );
    let under = status_json(fresh.path(), &fresh_state);
    assert_eq!(under["backlog"]["count"], 1);
    assert_eq!(under["backlog"]["stale"], 0, "1h must not read as stale");
}

/// Signal 2 — the surface tick asserts only what it checked. A project that
/// has never reconciled is not fresh, and reconcile age is its own field.
#[test]
fn reconcile_freshness_is_its_own_signal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(
        &state_dir,
        &[pending("task-20260101-aaaa", Duration::days(1))],
    );

    let out = status_json(tmp.path(), &state_dir);
    assert!(
        out["surfaces"]["reconcile_stale"]
            .as_bool()
            .expect("a boolean"),
        "never reconciled must not read as fresh"
    );
    assert!(
        !out["surfaces"]["projected"].as_bool().expect("a boolean"),
        "nothing has been projected"
    );
    assert!(
        out["surfaces"].get("reconcile_age_seconds").is_some(),
        "reconcile age must have a field of its own, not hide behind the tick"
    );
}

/// Signal 3 — a lease molecule is not backlog, and the ledger is what says
/// so. The same fixture with and without a ledger is the whole assertion.
#[test]
fn a_lease_molecule_is_not_counted_as_backlog() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(
        &state_dir,
        &[
            pending("task-20260101-aaaa", Duration::days(39)),
            pending("task-20260101-bbbb", Duration::hours(2)),
        ],
    );

    let before = status_json(tmp.path(), &state_dir);
    assert_eq!(before["backlog"]["count"], 2);
    assert_eq!(before["molecules"]["leases"], 0);

    declare_lease(&state_dir, "task-20260101-aaaa");

    let after = status_json(tmp.path(), &state_dir);
    assert_eq!(after["backlog"]["count"], 1, "the lease left the backlog");
    assert_eq!(
        after["backlog"]["stale"], 0,
        "and took the stale count with it"
    );
    assert_eq!(after["backlog"]["leases_excluded"], 1);
    assert_eq!(after["molecules"]["leases"], 1);
    assert_eq!(after["molecules"]["alive_excluding_leases"], 1);
    assert_eq!(
        after["molecules"]["alive"], 2,
        "`alive` keeps meaning what it always meant — the key is not re-pointed under a caller"
    );
}

/// Signal 4 — the unmerged count carries its movement, sampled on disk.
#[test]
fn unmerged_gauge_reports_movement() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(
        &state_dir,
        &[pending("task-20260101-aaaa", Duration::hours(1))],
    );

    let first = status_json(tmp.path(), &state_dir);
    assert_eq!(first["unmerged"]["branches"], 0);
    assert!(
        first["unmerged"]["delta"].is_null(),
        "the first run has nothing to compare against and must not invent a delta"
    );

    // The sample is on disk, so the next run has a baseline.
    let gauge = state_dir.join("status-gauge.json");
    assert!(gauge.exists(), "the gauge must persist its sample");

    // Backdate the sample and raise its recorded level: the next run must
    // report the fall, and the window it fell in.
    let mut sample: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&gauge).expect("read gauge"))
            .expect("parse gauge");
    sample["unmerged_branches"] = serde_json::json!(20);
    sample["sampled_at"] = serde_json::json!((Utc::now() - Duration::hours(3)).to_rfc3339());
    fs::write(&gauge, sample.to_string()).expect("write gauge");

    let second = status_json(tmp.path(), &state_dir);
    assert_eq!(second["unmerged"]["previous_branches"], 20);
    assert_eq!(second["unmerged"]["delta"], -20);
    assert!(
        second["unmerged"]["since_seconds"]
            .as_i64()
            .expect("a window")
            > Duration::hours(2).num_seconds(),
        "the delta must name the window it was measured over"
    );
}
