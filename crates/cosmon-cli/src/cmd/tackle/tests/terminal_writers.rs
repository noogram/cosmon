// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use cosmon_core::{collapse_contract::MoleculeRevision, error::CosmonError};
use cosmon_state::{Fleet, FleetGuard, MoleculeFilter};
use std::cell::Cell;

// A real FileStore with a deterministic intervention at the production port.
// It either commits before the final read or between that read and its save.
struct Intervention {
    store: FileStore,
    status: MoleculeStatus,
    after_read: bool,
    fired: Cell<bool>,
    committed: std::cell::RefCell<Vec<u8>>,
}

impl Intervention {
    fn commit(&self, id: &MoleculeId) {
        let mut current = self.store.load_molecule(id).unwrap();
        current.status = self.status;
        if self.status == MoleculeStatus::Collapsed {
            current
                .completed_steps
                .push(cosmon_core::id::StepId::new("answer").unwrap());
        }
        current.collapse_reason = Some("intervening transition".into());
        self.store.save_molecule_durable(id, &current).unwrap();
        *self.committed.borrow_mut() =
            fs::read(self.store.molecule_dir(id).join("state.json")).unwrap();
        self.fired.set(true);
    }
}

impl StateStore for Intervention {
    fn load_fleet(&self) -> Result<Fleet, CosmonError> {
        self.store.load_fleet()
    }
    fn save_fleet(&self, f: &Fleet) -> Result<(), CosmonError> {
        self.store.save_fleet(f)
    }
    fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
        self.store.load_molecule(id)
    }
    fn save_molecule(&self, id: &MoleculeId, m: &MoleculeData) -> Result<(), CosmonError> {
        self.store.save_molecule(id, m)
    }
    fn list_molecules(&self, f: &MoleculeFilter) -> Result<Vec<MoleculeData>, CosmonError> {
        self.store.list_molecules(f)
    }
    fn molecule_dir(&self, id: &MoleculeId) -> std::path::PathBuf {
        self.store.molecule_dir(id)
    }
    fn lock_fleet(&self) -> Result<Box<dyn FleetGuard + '_>, CosmonError> {
        self.store.lock_fleet()
    }
    fn load_molecule_revisioned(
        &self,
        id: &MoleculeId,
    ) -> Result<(MoleculeData, MoleculeRevision), CosmonError> {
        if !self.after_read && !self.fired.get() {
            self.commit(id);
        }
        self.store.load_molecule_revisioned(id)
    }
    fn save_molecule_if_revision(
        &self,
        id: &MoleculeId,
        m: &MoleculeData,
        revision: &MoleculeRevision,
    ) -> Result<(), CosmonError> {
        if self.after_read && !self.fired.get() {
            self.commit(id);
        }
        self.store.save_molecule_if_revision(id, m, revision)
    }
}

fn formula() -> Formula {
    Formula::parse(
        r#"
formula = "terminal-writer"
version = 1
[[steps]]
id = "answer"
title = "Answer"
description = "Answer"
response_artifact = "result.md"
[[steps]]
id = "verify"
title = "Verify"
description = "Verify"
"#,
    )
    .unwrap()
}

fn stale_outcome(gate: bool) {
    for status in [MoleculeStatus::Completed, MoleculeStatus::Collapsed] {
        for after_read in [false, true] {
            let (tmp, state) = make_store();
            let store = FileStore::new(&state);
            let mol = sample_molecule("task-20261017-race", MoleculeStatus::Running);
            store.save_molecule(&mol.id, &mol).unwrap();
            let intervention = Intervention {
                store,
                status,
                after_read,
                fired: Cell::new(false),
                committed: Default::default(),
            };
            let f = formula();
            let result = if gate {
                advance_gate_step(
                    &Context {
                        verbose: false,
                        json: true,
                        config: Some(state.clone()),
                    },
                    &intervention,
                    &mol.id,
                    &mol,
                    &f,
                    tmp.path(),
                )
            } else {
                accept_worker_turn(
                    &intervention,
                    &state,
                    &mol.id,
                    &f,
                    0,
                    "accepted text",
                    true,
                    false,
                    std::time::SystemTime::now(),
                    tmp.path(),
                    "direct",
                )
            };
            assert!(intervention.fired.get());
            assert!(
                result.is_err(),
                "stale outcome accepted: {status:?}, after_read={after_read}"
            );
            assert_eq!(
                fs::read(intervention.store.molecule_dir(&mol.id).join("state.json")).unwrap(),
                *intervention.committed.borrow()
            );
            assert!(
                !state.join("events.jsonl").exists(),
                "refusal emitted completion events"
            );
        }
    }
}

#[test]
fn gate_completion_rejects_intervening_terminal_state() {
    stale_outcome(true);
}

#[test]
fn worker_acceptance_rejects_intervening_terminal_state() {
    stale_outcome(false);
}

#[test]
fn duplicate_acceptance_requires_a_recorded_step_and_never_collapsed() {
    for status in [
        MoleculeStatus::Running,
        MoleculeStatus::Completed,
        MoleculeStatus::Collapsed,
    ] {
        let (tmp, state) = make_store();
        let store = FileStore::new(&state);
        let mut mol = sample_molecule("task-20261017-dupe", status);
        mol.completed_steps
            .push(cosmon_core::id::StepId::new("answer").unwrap());
        store.save_molecule(&mol.id, &mol).unwrap();
        let before = fs::read(store.molecule_dir(&mol.id).join("state.json")).unwrap();
        let result = accept_worker_turn(
            &store,
            &state,
            &mol.id,
            &formula(),
            0,
            "retry",
            true,
            false,
            std::time::SystemTime::now(),
            tmp.path(),
            "direct",
        );
        assert_eq!(result.is_ok(), status != MoleculeStatus::Collapsed);
        assert_eq!(
            fs::read(store.molecule_dir(&mol.id).join("state.json")).unwrap(),
            before
        );
    }
}
