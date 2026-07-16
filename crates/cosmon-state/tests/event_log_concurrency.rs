// SPDX-License-Identifier: AGPL-3.0-only

//! ADR-052 §I7 (Gödel G5) — concurrent-writer stress test for `events.jsonl`.
//!
//! Spawns 10 OS threads, each opening its own [`EventLogWriter`] against a
//! shared file and emitting 1 000 events. The kernel's `flock(2)` is
//! advisory but honoured by every writer in this binary, so the assertions
//! that follow are the empirical witness for the invariant:
//!
//! - **No line interleaving.** Every line round-trips through serde as a
//!   complete `Envelope`. A torn write would surface as a parse error.
//! - **Strict global seq density.** The set of `seq` values is
//!   `{0, 1, …, N-1}` with no gaps and no duplicates.
//! - **Strict per-molecule seq density.** For each writer's molecule, the
//!   `mol_seq` values are `{0, 1, …, 999}` with no gaps and no duplicates.
//! - **Zero dropped writes.** The total event count equals
//!   `WRITERS * EVENTS_PER_WRITER`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::thread;

use cosmon_core::event_v2::{EventV2, Seq};
use cosmon_core::id::MoleculeId;
use cosmon_state::event_log::{read_all, EventLogWriter};
use tempfile::TempDir;

const WRITERS: usize = 10;
const EVENTS_PER_WRITER: usize = 1_000;

fn mol_id(idx: usize) -> MoleculeId {
    // 4-hex-digit suffix keeps the `MoleculeId` validator happy.
    MoleculeId::new(format!("cs-20260419-w{idx:03x}")).unwrap()
}

#[test]
fn ten_concurrent_writers_emit_one_thousand_events_each_without_corruption() {
    let dir = TempDir::new().unwrap();
    let path = Arc::new(dir.path().join("events.jsonl"));

    let mut handles = Vec::with_capacity(WRITERS);
    for w in 0..WRITERS {
        let path = Arc::clone(&path);
        handles.push(thread::spawn(move || {
            let mut writer = EventLogWriter::open(path.as_path())
                .unwrap_or_else(|e| panic!("writer {w} open: {e}"));
            let molecule = mol_id(w);
            for step in 0..EVENTS_PER_WRITER {
                let event = EventV2::MoleculeStepCompleted {
                    molecule_id: molecule.clone(),
                    step,
                    total: EVENTS_PER_WRITER,
                    duration_ms: Some(step as u64),
                    step_hash: None,
                };
                // The lock is non-blocking with a 500 ms ceiling. Under
                // 10-way contention some attempts will hit `WouldBlock`
                // and retry inside `acquire_lock`; if the retry budget is
                // exhausted the test fails — that is the signal we want.
                writer.emit(event, None).unwrap_or_else(|e| {
                    panic!("writer {w} step {step} emit: {e}");
                });
            }
            writer.sync().unwrap();
        }));
    }
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    let envs = read_all(path.as_path()).expect("read_all");
    let total_expected = WRITERS * EVENTS_PER_WRITER;
    assert_eq!(
        envs.len(),
        total_expected,
        "zero dropped writes: expected {total_expected}, got {}",
        envs.len()
    );

    // Strict global seq density: {0, 1, ..., N-1}.
    let global_seqs: BTreeSet<Seq> = envs.iter().map(|e| e.seq).collect();
    assert_eq!(
        global_seqs.len(),
        total_expected,
        "global seq must be unique across all writers"
    );
    assert_eq!(
        global_seqs.iter().next().copied(),
        Some(Seq(0)),
        "global seq starts at 0"
    );
    assert_eq!(
        global_seqs.iter().next_back().copied(),
        Some(Seq((total_expected - 1) as u64)),
        "global seq ends at N-1 (no gaps)"
    );

    // Strict per-molecule density: each writer's molecule sees {0..1000}.
    let mut by_mol: HashMap<MoleculeId, Vec<Seq>> = HashMap::new();
    for env in &envs {
        let mol = env
            .event
            .molecule_id()
            .cloned()
            .expect("every event in this test carries a molecule_id");
        let mol_seq = env
            .mol_seq
            .expect("every event in this test must carry a mol_seq");
        by_mol.entry(mol).or_default().push(mol_seq);
    }
    assert_eq!(by_mol.len(), WRITERS, "one bucket per writer molecule");
    for (mol, mut seqs) in by_mol {
        seqs.sort();
        assert_eq!(
            seqs.len(),
            EVENTS_PER_WRITER,
            "per-molecule emission count mismatch for {mol}",
        );
        for (i, s) in seqs.iter().enumerate() {
            assert_eq!(
                *s,
                Seq(i as u64),
                "per-molecule seq density broken for {mol} at index {i}",
            );
        }
    }
}
