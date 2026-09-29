// SPDX-License-Identifier: AGPL-3.0-only

//! Durable harvest reservations remain distinct from completed effects.

use std::sync::{Arc, Barrier};

use cosmon_core::harvest_authorization::{
    ConsumptionRecord, HarvestEffect, HarvestJournalRecord, HarvestJournalStage, PermitId,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::OperatorKeyId;
use cosmon_filestore::harvest_authority::{FileConsumptionLedger, FileHarvestJournal};
use cosmon_filestore::FileStore;
use cosmon_state::StateStore;
use tempfile::tempdir;

fn entry(molecule: &str, permit_char: char) -> HarvestJournalRecord {
    let permit: PermitId =
        serde_json::from_value(serde_json::json!(permit_char.to_string().repeat(64)))
            .expect("fixture permit");
    let grant = serde_json::from_value(serde_json::json!("f".repeat(64)))
        .expect("fixture grant fingerprint");
    HarvestJournalRecord {
        version: 1,
        stage: HarvestJournalStage::Prepared,
        receipt: ConsumptionRecord {
            permit,
            grant,
            effect: HarvestEffect {
                galaxy: "fixture".into(),
                molecule: MoleculeId::new(molecule).expect("fixture molecule"),
                base: "main".into(),
            },
            key_id: OperatorKeyId::from_bytes([3; 8]),
            invocation_id: format!("operation-{molecule}"),
        },
        branch_head: Some("a".repeat(40)),
        pre_merge_base: "b".repeat(40),
        options_digest: "c".repeat(64),
        hook_digest: "e".repeat(64),
        merge_oid: None,
    }
}

#[test]
fn reserved_permit_is_not_a_landed_outcome() {
    let root = tempdir().expect("state root");
    let journal = FileHarvestJournal::at_state_root(root.path());
    let prepared = entry("task-20260929-1111", '1');
    journal.append(&prepared).expect("durable reservation");
    let recovered = journal
        .latest(&prepared.receipt.permit)
        .expect("read")
        .expect("attempt");
    assert_eq!(recovered.stage, HarvestJournalStage::Prepared);
    assert!(recovered.merge_oid.is_none());
    assert!(
        journal.append(&prepared).is_err(),
        "duplicate preparation cannot reopen a spend"
    );
}

#[test]
fn integration_then_finalization_preserves_one_operation_and_independent_member() {
    let root = tempdir().expect("state root");
    let journal = FileHarvestJournal::at_state_root(root.path());
    let prepared = entry("task-20260929-1111", '1');
    journal.append(&prepared).expect("prepare");
    let mut integrated = prepared.clone();
    integrated.stage = HarvestJournalStage::Integrated;
    integrated.merge_oid = Some("d".repeat(40));
    journal.append(&integrated).expect("integrate");
    let mut finalized = integrated.clone();
    finalized.stage = HarvestJournalStage::Finalized;
    journal.append(&finalized).expect("finalize");
    assert_eq!(
        journal.latest(&prepared.receipt.permit).expect("read"),
        Some(finalized)
    );
    assert!(
        journal.append(&integrated).is_err(),
        "an integrated operation cannot restart"
    );

    let member = entry("task-20260929-2222", '2');
    journal
        .append(&member)
        .expect("independent member has its own permit");
    assert_eq!(
        journal.latest(&member.receipt.permit).expect("read"),
        Some(member)
    );
}

#[test]
fn torn_and_conflicting_progress_are_not_treated_as_no_attempt() {
    let root = tempdir().expect("state root");
    let journal = FileHarvestJournal::at_state_root(root.path());
    let prepared = entry("task-20260929-1111", '1');
    journal.append(&prepared).expect("prepare");
    let mut changed = prepared.clone();
    changed.stage = HarvestJournalStage::Integrated;
    changed.merge_oid = Some("d".repeat(40));
    changed.options_digest = "e".repeat(64);
    assert!(journal.append(&changed).is_err());

    let path = root
        .path()
        .join(cosmon_filestore::harvest_authority::HARVEST_JOURNAL_REL);
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open")
        .write_all(b"{\"stage\":")
        .expect("torn line");
    assert!(journal.latest(&prepared.receipt.permit).is_err());
    assert!(journal
        .has_attempt_for(&prepared.receipt.effect.molecule)
        .is_err());
}

#[test]
fn legacy_receipt_without_progress_remains_reserved_unknown() {
    let root = tempdir().expect("state root");
    let prepared = entry("task-20260929-1111", '1');
    let ledger = FileConsumptionLedger::at_state_root(root.path());
    use cosmon_core::harvest_authorization::HarvestConsumptionLedger as _;
    ledger.consume(&prepared.receipt).expect("legacy receipt");
    assert!(ledger
        .has_receipt_for(&prepared.receipt.effect.molecule)
        .expect("read"));
    assert!(FileHarvestJournal::at_state_root(root.path())
        .latest(&prepared.receipt.permit)
        .expect("read")
        .is_none());
}

#[test]
fn duplicate_writers_under_the_trunk_lock_cannot_both_prepare() {
    let root = tempdir().expect("state root");
    let path = root.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let store = FileStore::new(&path);
                let journal = FileHarvestJournal::at_state_root(&path);
                let prepared = entry("task-20260929-1111", '1');
                barrier.wait();
                let _guard = store
                    .lock_trunk("duplicate harvest fixture")
                    .expect("trunk lock");
                journal.append(&prepared).is_ok()
            })
        })
        .collect();
    let wins: usize = handles
        .into_iter()
        .map(|h| usize::from(h.join().expect("writer")))
        .sum();
    assert_eq!(wins, 1);
}

#[test]
fn same_uid_state_deletion_remains_outside_the_journal_guarantee() {
    let root = tempdir().expect("disposable state root");
    let prepared = entry("task-20260929-1111", '1');
    let journal = FileHarvestJournal::at_state_root(root.path());
    let ledger = FileConsumptionLedger::at_state_root(root.path());
    use cosmon_core::harvest_authorization::HarvestConsumptionLedger as _;
    ledger.consume(&prepared.receipt).expect("receipt");
    journal.append(&prepared).expect("prepare");
    let receipt_bytes = std::fs::read(ledger.path()).expect("receipt bytes");
    let journal_path = root
        .path()
        .join(cosmon_filestore::harvest_authority::HARVEST_JOURNAL_REL);
    let journal_bytes = std::fs::read(&journal_path).expect("journal bytes");

    // This witness runs only in a disposable repository. A same-uid process
    // can delete and later restore both files; the journal is not a custody
    // boundary against such a process.
    std::fs::remove_file(ledger.path()).expect("delete receipts");
    std::fs::remove_file(&journal_path).expect("delete progress");
    assert!(ledger
        .recorded(&prepared.receipt.permit)
        .expect("lookup")
        .is_none());
    assert!(journal
        .latest(&prepared.receipt.permit)
        .expect("lookup")
        .is_none());
    std::fs::write(ledger.path(), receipt_bytes).expect("restore receipts");
    std::fs::write(&journal_path, journal_bytes).expect("restore progress");
    assert!(ledger
        .recorded(&prepared.receipt.permit)
        .expect("lookup")
        .is_some());
    assert!(journal
        .latest(&prepared.receipt.permit)
        .expect("lookup")
        .is_some());
}
