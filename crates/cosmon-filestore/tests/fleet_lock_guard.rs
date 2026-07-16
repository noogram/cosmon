// SPDX-License-Identifier: AGPL-3.0-only

//! Tests for the object-safe fleet/trunk locking port (ADR-131 Decision 2).
//!
//! The production commands no longer call the generic-closure
//! `FileStore::with_fleet_lock(|s| …)`; they hold an RAII guard returned by
//! the `StateStore::lock_fleet` / `lock_trunk` port methods:
//!
//! ```ignore
//! let _g = store.lock_fleet()?;   // flock held until `_g` drops
//! // … load → mutate → save through the port …
//! ```
//!
//! These tests pin the two properties that the crash-recovery core depends on,
//! exercised through the **object-safe** `&dyn StateStore` surface (not the
//! concrete `FileStore`), so they cover exactly the path the converted call
//! sites use:
//!
//! 1. **RMW atomicity** — concurrent `load_fleet → mutate → save_fleet`
//!    cycles, each under `lock_fleet`, never lose an update. Without the lock
//!    this is the textbook lost-update race.
//! 2. **RAII release** — the guard releases the lock on drop, so a later
//!    acquisition does not block forever.

use std::sync::Arc;
use std::thread;

use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::id::{AgentId, WorkerId};
use cosmon_core::worker::WorkerStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{StateStore, WorkerData};

/// Build a `WorkerData` whose id encodes `(thread, round)` so every insertion
/// across the whole test is unique — a lost update shows up as a missing key.
fn worker(tid: usize, round: usize) -> WorkerData {
    let id = WorkerId::new(format!("worker-{tid}-{round}")).expect("worker id");
    let agent_id = AgentId::new("fleet-lock-test").expect("agent id");
    WorkerData::new(
        id,
        agent_id,
        AgentRole::Implementation,
        Clearance::Write,
        WorkerStatus::Active,
    )
}

/// RMW atomicity under contention. `THREADS` workers each run `ROUNDS`
/// read-modify-write cycles against the *same* fleet.json, holding
/// `lock_fleet` (the object-safe port method) for the load→insert→save window.
///
/// The exclusive guard serialises the cycles, so every one of the
/// `THREADS * ROUNDS` insertions must survive — the final fleet carries
/// exactly that many distinct workers. Drop the guard and this assertion
/// fails intermittently with lost updates.
#[test]
fn lock_fleet_serialises_concurrent_rmw() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 12;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Arc::new(tmp.path().to_path_buf());

    let handles: Vec<_> = (0..THREADS)
        .map(|tid| {
            let root = Arc::clone(&root);
            thread::spawn(move || {
                // Each thread builds its own adapter and drives it through the
                // object-safe port, exactly as the converted call sites do.
                let store: &dyn StateStore = &FileStore::new(root.as_path());
                for round in 0..ROUNDS {
                    let _g = store.lock_fleet().expect("acquire fleet lock");
                    let mut fleet = store.load_fleet().expect("load fleet");
                    let w = worker(tid, round);
                    fleet.workers.insert(w.id.clone(), w);
                    store.save_fleet(&fleet).expect("save fleet");
                    // `_g` drops here, releasing the flock for the next cycle.
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    let store = FileStore::new(root.as_path());
    let fleet = store.load_fleet().expect("final load");
    assert_eq!(
        fleet.workers.len(),
        THREADS * ROUNDS,
        "lost update: fleet lock did not serialise concurrent read-modify-write \
         (expected {} workers, found {})",
        THREADS * ROUNDS,
        fleet.workers.len(),
    );
}

/// The fleet guard releases on drop: a sequential re-acquire after the first
/// guard is dropped must succeed promptly (the flock is per-OFD and would
/// otherwise self-deadlock the second exclusive acquire in the same process).
#[test]
fn lock_fleet_guard_releases_on_drop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: &dyn StateStore = &FileStore::new(tmp.path());

    {
        let _g = store.lock_fleet().expect("first acquire");
        // First guard holds the lock here.
    } // released on drop

    // Second acquire succeeds because the first released — no hang.
    let _g2 = store.lock_fleet().expect("re-acquire after drop");
}

/// The trunk guard is reachable through the object-safe port too, returning a
/// working RAII guard that releases on drop (the trunk-lock cross-process
/// semantics themselves are covered by `trunk_lock_concurrent.rs`).
#[test]
fn lock_trunk_via_port_returns_working_guard() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: &dyn StateStore = &FileStore::new(tmp.path());

    {
        let _g = store
            .lock_trunk("cs done test")
            .expect("acquire trunk lock");
    } // released on drop

    let _g2 = store
        .lock_trunk("cs done test")
        .expect("re-acquire trunk after drop");
}
