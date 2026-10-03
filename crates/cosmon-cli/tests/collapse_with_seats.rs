// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #162 — `cs collapse --with-seats` settles a declared work in one
//! command: every live member gets the same motive, completed members are
//! left for `cs done`, nothing is merged, and a rerun converges.

use std::collections::HashMap;
use std::process::{Command, Output};

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};

const IMPL: &str = "task-20261001-a001";
const REVIEW: &str = "task-20261001-a002";
const DONE_SEAT: &str = "task-20261001-a003";
const OUTSIDE: &str = "task-20261001-a004";

fn molecule(id: &str, status: MoleculeStatus) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 2,
        current_step: 1,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: Vec::new(),
        protected_paths: Vec::new(),
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

struct Galaxy {
    dir: tempfile::TempDir,
}

impl Galaxy {
    fn state(&self) -> std::path::PathBuf {
        self.dir.path().join(".cosmon/state")
    }

    fn store(&self) -> FileStore {
        FileStore::new(self.state())
    }

    fn status(&self, id: &str) -> MoleculeData {
        self.store()
            .load_molecule(&MoleculeId::new(id).expect("id"))
            .expect("load molecule")
    }

    fn cs(&self, args: &[&str]) -> Output {
        let state = self.state();
        Command::new(env!("CARGO_BIN_EXE_cs"))
            .current_dir(self.dir.path())
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .args(["--config", state.to_str().expect("utf-8")])
            .args(args)
            .arg("--ops-dir")
            .arg(&state)
            .output()
            .expect("run cs")
    }

    fn declare(&self, owner: &str, seats: &[(&str, &str)]) {
        let state = self.state();
        let mut command = Command::new(env!("CARGO_BIN_EXE_cs"));
        command
            .current_dir(self.dir.path())
            .env_remove("COSMON_MOL_DIR")
            .args(["--config", state.to_str().expect("utf-8")])
            .args(["work", "declare", owner]);
        for (seat, molecule) in seats {
            command.args(["--seat", &format!("{seat}={molecule}")]);
        }
        let output = command.output().expect("declare");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// An implementer (owner, collapsed on its own), a live reviewer, a completed
/// third seat, and a molecule outside the work.
fn galaxy() -> Galaxy {
    let galaxy = Galaxy {
        dir: tempfile::tempdir().expect("tempdir"),
    };
    let store = galaxy.store();
    let mut implementer = molecule(IMPL, MoleculeStatus::Collapsed);
    implementer.collapse_reason = Some("gate failed outside the diff".to_owned());
    for data in [
        implementer,
        molecule(REVIEW, MoleculeStatus::Running),
        molecule(DONE_SEAT, MoleculeStatus::Completed),
        molecule(OUTSIDE, MoleculeStatus::Running),
    ] {
        store.save_molecule(&data.id, &data).expect("save molecule");
    }
    galaxy.declare(
        IMPL,
        &[("impl", IMPL), ("review", REVIEW), ("audit", DONE_SEAT)],
    );
    galaxy
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn collapses_the_live_seat_with_the_owners_reason_and_leaves_the_rest() {
    let galaxy = galaxy();
    let output = galaxy.cs(&["collapse", IMPL, "--with-seats"]);
    assert!(output.status.success(), "{}", text(&output));

    let review = galaxy.status(REVIEW);
    assert_eq!(review.status, MoleculeStatus::Collapsed);
    let reason = review.collapse_reason.expect("reason recorded");
    assert!(
        reason.starts_with("gate failed outside the diff (seat review of work task-20261001-a001@"),
        "{reason}"
    );
    // A completed seat is skipped, never collapsed, and the harvest is named.
    assert_eq!(galaxy.status(DONE_SEAT).status, MoleculeStatus::Completed);
    assert!(
        text(&output).contains(&format!("cs done {DONE_SEAT}")),
        "{}",
        text(&output)
    );
    // The owner is left as it was and nothing outside the work is touched.
    assert_eq!(
        galaxy.status(IMPL).collapse_reason.as_deref(),
        Some("gate failed outside the diff")
    );
    assert_eq!(galaxy.status(OUTSIDE).status, MoleculeStatus::Running);
}

#[test]
fn naming_a_member_resolves_the_same_work_and_an_explicit_reason_is_used_verbatim() {
    let galaxy = galaxy();
    let output = galaxy.cs(&[
        "collapse",
        REVIEW,
        "--with-seats",
        "--reason",
        "superseded by a new pair",
        "--reason-kind",
        "manual_abort",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    let review = galaxy.status(REVIEW);
    assert_eq!(review.status, MoleculeStatus::Collapsed);
    assert_eq!(
        review.collapse_reason.as_deref(),
        Some("superseded by a new pair")
    );
    assert_eq!(
        review.collapse_reason_kind.as_ref().map(|k| k.as_str()),
        Some("manual_abort")
    );
}

#[test]
fn rerun_converges_without_changing_anything() {
    let galaxy = galaxy();
    assert!(galaxy
        .cs(&["collapse", IMPL, "--with-seats"])
        .status
        .success());
    let first = galaxy.status(REVIEW);
    let again = galaxy.cs(&["collapse", IMPL, "--with-seats"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(
        text(&again).contains("already collapsed"),
        "{}",
        text(&again)
    );
    let second = galaxy.status(REVIEW);
    assert_eq!(first.collapse_reason, second.collapse_reason);
    assert_eq!(first.updated_at, second.updated_at);
}

#[test]
fn without_a_reason_and_with_a_live_owner_it_refuses_and_changes_nothing() {
    let galaxy = galaxy();
    let mut owner = galaxy.status(IMPL);
    owner.status = MoleculeStatus::Running;
    owner.collapse_reason = None;
    galaxy
        .store()
        .save_molecule(&owner.id.clone(), &owner)
        .expect("save owner");
    let output = galaxy.cs(&["collapse", IMPL, "--with-seats"]);
    assert!(!output.status.success());
    assert_eq!(galaxy.status(REVIEW).status, MoleculeStatus::Running);
    assert_eq!(galaxy.status(IMPL).status, MoleculeStatus::Running);
}

#[test]
fn one_failing_seat_does_not_stop_the_others_and_the_exit_is_non_zero() {
    let galaxy = galaxy();
    // A live seat whose state cannot be read, listed before the reviewer.
    let broken = "task-20261001-a000";
    let mut data = molecule(broken, MoleculeStatus::Running);
    data.id = MoleculeId::new(broken).expect("id");
    galaxy
        .store()
        .save_molecule(&data.id, &data)
        .expect("save broken");
    galaxy.declare(
        IMPL,
        &[
            ("impl", IMPL),
            ("review", REVIEW),
            ("audit", DONE_SEAT),
            ("broken", broken),
        ],
    );
    let state_json = galaxy
        .state()
        .join("fleets/default/molecules")
        .join(broken)
        .join("state.json");
    std::fs::write(state_json, b"not json").expect("corrupt");

    let output = galaxy.cs(&["collapse", IMPL, "--with-seats"]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains(broken), "{}", text(&output));
    assert_eq!(galaxy.status(REVIEW).status, MoleculeStatus::Collapsed);
}

#[test]
fn a_molecule_outside_any_work_is_refused() {
    let galaxy = galaxy();
    let output = galaxy.cs(&["collapse", OUTSIDE, "--with-seats", "--reason", "x"]);
    assert!(!output.status.success());
    assert_eq!(galaxy.status(OUTSIDE).status, MoleculeStatus::Running);
}
