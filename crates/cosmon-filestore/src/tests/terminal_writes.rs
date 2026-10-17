// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use cosmon_core::collapse_contract::{
    CallerContext, CollapseCondition, CollapsibleStatus, IdempotencyKey, ReceiptLookup,
};
use cosmon_state::ops::collapse::{execute, lookup_receipt, CollapseRequest, NoLeaseCheck};

fn collapse(store: &FileStore, id: &MoleculeId) {
    let (_, revision) = store.load_molecule_revisioned(id).unwrap();
    execute(
        store,
        &store.root,
        &NoLeaseCheck,
        &CallerContext::Local { session: None },
        id,
        CollapseRequest::for_reason("approved collapse"),
        Some(CollapseCondition {
            key: IdempotencyKey::parse("terminal-write-test-key-0001").unwrap(),
            expect_status: CollapsibleStatus::Running,
            expect_revision: revision,
            approval_digest: None,
            not_after: None,
        }),
        Utc::now(),
    )
    .unwrap();
}

#[test]
fn stale_regular_and_durable_saves_preserve_collapsed_receipt() {
    for durable in [false, true] {
        let (_tmp, store) = make_store();
        let stale = sample_molecule("sticky", MoleculeStatus::Running, None);
        store.save_molecule(&stale.id, &stale).unwrap();
        collapse(&store, &stale.id);
        let path = store.molecule_path(&stale.id);
        let committed = fs::read(&path).unwrap();
        let result = if durable {
            store.save_molecule_durable(&stale.id, &stale)
        } else {
            store.save_molecule(&stale.id, &stale)
        };
        assert!(result.is_err());
        // A stale writer may itself decide to collapse. Keeping the status
        // cannot authorize erasing the embedded committed receipt either.
        let mut stale_collapse = stale.clone();
        stale_collapse.status = MoleculeStatus::Collapsed;
        assert!(store
            .write_molecule(&stale.id, &stale_collapse, durable)
            .is_err());
        assert_eq!(fs::read(path).unwrap(), committed);
        assert!(matches!(
            lookup_receipt(
                &store,
                &stale.id,
                &IdempotencyKey::parse("terminal-write-test-key-0001").unwrap()
            )
            .unwrap(),
            ReceiptLookup::Committed { .. }
        ));
        let mut metadata = store.load_molecule(&stale.id).unwrap();
        metadata.archived = true;
        store.save_molecule(&stale.id, &metadata).unwrap();
        assert!(store.load_molecule(&stale.id).unwrap().archived);
    }
}

#[test]
fn completed_to_frozen_and_legacy_state_remain_legal() {
    let (_tmp, store) = make_store();
    let mut mol = sample_molecule("legacy", MoleculeStatus::Completed, None);
    store.save_molecule(&mol.id, &mol).unwrap();
    let path = store.molecule_path(&mol.id);
    // Old records do not carry a schema_version or collapse_receipt.
    let mut json = serde_json::to_value(&mol).unwrap();
    json.as_object_mut().unwrap().remove("collapse_receipt");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let legacy = store
        .root
        .join("ops/molecules")
        .join(mol.id.as_str())
        .join("state.json");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::rename(&path, &legacy).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
    mol.status = MoleculeStatus::Frozen;
    store.save_molecule_durable(&mol.id, &mol).unwrap();
    assert_eq!(
        store.load_molecule(&mol.id).unwrap().status,
        MoleculeStatus::Frozen
    );
    fs::write(&path, b"{broken").unwrap();
    for durable in [false, true] {
        assert!(store.write_molecule(&mol.id, &mol, durable).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
    }
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(store.save_molecule(&mol.id, &mol).is_err());
}

#[test]
fn serialized_check_through_rename_excludes_second_writer() {
    use std::sync::mpsc::channel;
    // Intervene after the actual guard check, before the actual rename.
    // Probe lock ownership, then allow the first write before collapsing.
    // With serialization neutralized the second writer commits its collapse
    // before the first rename, exposing precisely the check/save race.
    for durable in [false, true] {
        let (_tmp, store) = make_store();
        let stale = sample_molecule("race", MoleculeStatus::Running, None);
        store.save_molecule(&stale.id, &stale).unwrap();
        let (checked_tx, checked_rx) = channel();
        let (resume_tx, resume_rx) = channel();
        let writer = store.clone();
        let candidate = stale.clone();
        let handle = std::thread::spawn(move || {
            writer.write_molecule_checked(&candidate.id, &candidate, durable, None, || {
                checked_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            })
        });
        checked_rx.recv().unwrap();
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                store
                    .root
                    .join("molecule-write-locks")
                    .join(format!("{}.lock", stale.id)),
            )
            .unwrap();
        let serialized = probe.try_lock_exclusive().is_err();
        drop(probe);
        if !serialized {
            collapse(&store, &stale.id);
        }
        resume_tx.send(()).unwrap();
        handle.join().unwrap().unwrap();
        if serialized {
            collapse(&store, &stale.id);
        }
        assert_eq!(
            store.load_molecule(&stale.id).unwrap().status,
            MoleculeStatus::Collapsed,
            "stale rename overwrote the intervening collapse"
        );
        assert!(serialized, "check and rename must share the writer lock");
    }
}

#[test]
fn legacy_collapsed_cannot_reopen_via_fleet_migration() {
    let (_tmp, store) = make_store();
    let stale = sample_molecule("legacysticky", MoleculeStatus::Running, None);
    let mut terminal = stale.clone();
    terminal.status = MoleculeStatus::Collapsed;
    let legacy = store
        .root
        .join("ops/molecules")
        .join(stale.id.as_str())
        .join("state.json");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec(&terminal).unwrap();
    fs::write(&legacy, &bytes).unwrap();
    assert!(store.save_molecule(&stale.id, &stale).is_err());
    assert!(store.save_molecule_durable(&stale.id, &stale).is_err());
    assert_eq!(fs::read(legacy).unwrap(), bytes);
    assert!(!store
        .root
        .join("fleets/default/molecules")
        .join(stale.id.as_str())
        .exists());
}
