// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use cosmon_core::{collapse_contract::MoleculeRevision, error::CosmonError};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeFilter};
use std::cell::Cell;

struct InterveningStore {
    store: FileStore,
    status: MoleculeStatus,
    fired: Cell<bool>,
}
impl StateStore for InterveningStore {
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
    fn load_molecule_revisioned(
        &self,
        id: &MoleculeId,
    ) -> Result<(MoleculeData, MoleculeRevision), CosmonError> {
        let snapshot = self.store.load_molecule_revisioned(id)?;
        if !self.fired.replace(true) {
            let mut terminal = snapshot.0.clone();
            terminal.status = self.status;
            self.store.save_molecule_durable(id, &terminal)?;
        }
        Ok(snapshot)
    }
    fn save_molecule_if_revision(
        &self,
        id: &MoleculeId,
        m: &MoleculeData,
        r: &MoleculeRevision,
    ) -> Result<(), CosmonError> {
        self.store.save_molecule_if_revision(id, m, r)
    }
}

#[test]
fn dispatch_claim_rejects_intervening_terminal_commit() {
    for status in [MoleculeStatus::Completed, MoleculeStatus::Collapsed] {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::new(tmp.path());
        let mut mol = running_mol_with_session("task-20261017-claim", None);
        mol.status = MoleculeStatus::Pending;
        store.save_molecule(&mol.id, &mol).unwrap();
        let runtime = Runtime::new(
            Box::new(InterveningStore {
                store: store.clone(),
                status,
                fired: Cell::new(false),
            }),
            Box::new(NoOpPolicy),
            Box::new(NoOpExecutor),
            RuntimeConfig::default(),
        );
        assert!(runtime.apply_evolve(&mol.id, "dispatch").is_err());
        mol.status = status;
        assert_eq!(
            serde_json::to_value(store.load_molecule(&mol.id).unwrap()).unwrap(),
            serde_json::to_value(mol).unwrap()
        );
    }
}

struct TerminalExecutor {
    store: FileStore,
    status: MoleculeStatus,
    committed: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}
impl Executor for TerminalExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        let mut current = self.store.load_molecule(id)?;
        // Keep timestamp and step unchanged: terminal status itself must block
        // rollback, even when no other progress field happens to distinguish it.
        current.status = self.status;
        self.store.save_molecule_durable(id, &current)?;
        *self.committed.lock().unwrap() =
            std::fs::read(self.store.molecule_dir(id).join("state.json")).unwrap();
        Err(RuntimeError::Dispatch {
            id: id.clone(),
            reason: "dispatch failed after terminal commit".into(),
        })
    }
}

#[test]
fn dispatch_rollback_preserves_concurrent_terminal_state() {
    for status in [MoleculeStatus::Completed, MoleculeStatus::Collapsed] {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::new(tmp.path());
        let mut mol = running_mol_with_session("task-20261017-rollback", None);
        mol.status = MoleculeStatus::Pending;
        store.save_molecule(&mol.id, &mol).unwrap();
        let committed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runtime = Runtime::new(
            Box::new(store.clone()),
            Box::new(NoOpPolicy),
            Box::new(TerminalExecutor {
                store: store.clone(),
                status,
                committed: committed.clone(),
            }),
            RuntimeConfig::default(),
        );
        assert!(runtime.apply_evolve(&mol.id, "dispatch").is_err());
        assert_eq!(
            std::fs::read(store.molecule_dir(&mol.id).join("state.json")).unwrap(),
            *committed.lock().unwrap()
        );
    }
}

#[test]
fn dispatch_skips_existing_terminal_state_without_writes() {
    for status in [MoleculeStatus::Completed, MoleculeStatus::Collapsed] {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::new(tmp.path());
        let mut mol = running_mol_with_session("task-20261017-terminal", None);
        mol.status = status;
        store.save_molecule(&mol.id, &mol).unwrap();
        let before = std::fs::read(store.molecule_dir(&mol.id).join("state.json")).unwrap();
        let runtime = Runtime::new(
            Box::new(store.clone()),
            Box::new(NoOpPolicy),
            Box::new(NoOpExecutor),
            RuntimeConfig::default(),
        );
        runtime.apply_evolve(&mol.id, "dispatch").unwrap();
        assert_eq!(
            std::fs::read(store.molecule_dir(&mol.id).join("state.json")).unwrap(),
            before
        );
    }
}
