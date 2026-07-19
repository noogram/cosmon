// SPDX-License-Identifier: AGPL-3.0-only

//! Property-based invariants for `previous_status` correctness on the
//! collapse transition.
//!
//! Pins the invariant: for every reachable transition path that lands on
//! `Collapsed`, the wire-level `previous_status` rendered by
//! [`cosmon_state::ops::collapse::CollapseJson`] equals the operator's
//! gesture immediately before collapse — `"stuck"` when the molecule
//! reached `Frozen` via `cs stuck`, otherwise the physical status.
//!
//! Reproduces the observed bug (collapse-from-stuck returned
//! `previous_status: "frozen"` instead of `"stuck"`) and prevents regression.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use chrono::Utc;
use cosmon_core::auth::Subject;
use cosmon_core::error::CosmonError;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::ops::collapse::{collapse, CollapseJson, CollapseRequest};
use cosmon_state::{Fleet, MoleculeData, MoleculeFilter, StateStore};
use proptest::prelude::*;
use tempfile::TempDir;

#[derive(Default)]
struct FakeStore {
    molecules: Mutex<HashMap<MoleculeId, MoleculeData>>,
}

impl StateStore for FakeStore {
    fn load_fleet(&self) -> Result<Fleet, CosmonError> {
        Ok(Fleet::default())
    }
    fn save_fleet(&self, _fleet: &Fleet) -> Result<(), CosmonError> {
        Ok(())
    }
    fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
        self.molecules
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| CosmonError::MoleculeNotFound(id.clone()))
    }
    fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError> {
        self.molecules
            .lock()
            .unwrap()
            .insert(id.clone(), data.clone());
        Ok(())
    }
    fn list_molecules(&self, _filter: &MoleculeFilter) -> Result<Vec<MoleculeData>, CosmonError> {
        Ok(self.molecules.lock().unwrap().values().cloned().collect())
    }
}

fn mol(id: &str, status: MoleculeStatus, stuck: bool) -> MoleculeData {
    let now = Utc::now();
    MoleculeData {
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: now,
        updated_at: now,
        total_steps: 1,
        current_step: 0,
        completed_steps: vec![],
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: vec![],
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: vec![],
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: BTreeSet::new(),
        escalations: vec![],
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
        stuck_at: if stuck { Some(now) } else { None },
        tackled_by: None,
        tackled_at: None,
    }
}

fn arb_collapsable_status() -> impl Strategy<Value = MoleculeStatus> {
    prop_oneof![
        Just(MoleculeStatus::Pending),
        Just(MoleculeStatus::Running),
        Just(MoleculeStatus::Frozen),
        Just(MoleculeStatus::Starved),
    ]
}

proptest! {
    /// For every reachable collapsable status × stuck-flavor combination,
    /// the wire-level `previous_status` matches the operator's gesture:
    /// `"stuck"` iff the molecule was Frozen *and* reached that state via
    /// `cs stuck`, otherwise the physical status string.
    #[test]
    fn previous_status_matches_operator_gesture(
        status in arb_collapsable_status(),
        stuck_marker in proptest::bool::ANY,
    ) {
        let store = FakeStore::default();
        // Only Frozen molecules can carry the stuck-flavor marker on the
        // happy path; this proptest still feeds the combination through to
        // pin the wire-level rule (Frozen + marker → "stuck"; everything
        // else → physical status).
        let m = mol("task-20260509-zzzz", status, stuck_marker);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260509-zzzz").unwrap();

        let view = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("proptest"),
        )
        .unwrap();
        let json = CollapseJson::from_view(&view);

        let previous_was_stuck = status == MoleculeStatus::Frozen && stuck_marker;
        let expected = if previous_was_stuck {
            "stuck".to_owned()
        } else {
            status.to_string()
        };
        prop_assert_eq!(json.previous_status, expected);
        prop_assert_eq!(view.previous_was_stuck, previous_was_stuck);
    }
}

#[test]
fn collapse_from_stuck_renders_previous_status_as_stuck() {
    // Direct regression for tenant-demo `VALIDATION-COLIMA-2026-05-09.md` O1:
    // after `cs stuck` (which sets Frozen + stuck_at), `cs collapse` must
    // report `previous_status: "stuck"` on the wire — not `"frozen"`.
    let store = FakeStore::default();
    let m = mol("task-20260509-aaaa", MoleculeStatus::Frozen, true);
    store.save_molecule(&m.id, &m).unwrap();
    let tmp = TempDir::new().unwrap();
    let id = MoleculeId::new("task-20260509-aaaa").unwrap();

    let view = collapse(
        &store,
        tmp.path(),
        &Subject::operator(),
        &id,
        CollapseRequest::for_reason("regression"),
    )
    .unwrap();
    let json = CollapseJson::from_view(&view);

    assert_eq!(json.previous_status, "stuck");
    assert!(view.previous_was_stuck);
}

#[test]
fn collapse_from_freeze_still_renders_frozen() {
    // Symmetry guard: Frozen reached via `cs freeze` (no `stuck_at`) must
    // continue to render `"frozen"`. The fix for `task-20260509-177e` is
    // additive — it does not collapse the freeze path into stuck.
    let store = FakeStore::default();
    let m = mol("task-20260509-bbbb", MoleculeStatus::Frozen, false);
    store.save_molecule(&m.id, &m).unwrap();
    let tmp = TempDir::new().unwrap();
    let id = MoleculeId::new("task-20260509-bbbb").unwrap();

    let view = collapse(
        &store,
        tmp.path(),
        &Subject::operator(),
        &id,
        CollapseRequest::for_reason("freeze-then-collapse"),
    )
    .unwrap();
    let json = CollapseJson::from_view(&view);

    assert_eq!(json.previous_status, "frozen");
    assert!(!view.previous_was_stuck);
}
