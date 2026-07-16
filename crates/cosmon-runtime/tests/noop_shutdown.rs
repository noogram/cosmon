// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end test for the sub-task-1 runtime skeleton (ADR-016 Phase 3).
//!
//! Spins up a [`cosmon_runtime::Runtime`] on top of a real
//! [`cosmon_filestore::FileStore`] backed by an empty temp dir, pairs it
//! with a [`cosmon_runtime::NoOpPolicy`], and asserts that the event loop:
//!
//! 1. Actually executes at least one tick (observes the empty fleet).
//! 2. Exits cleanly with [`ShutdownReason::PolicyDrained`] because the
//!    policy returns no actions on its first snapshot.
//! 3. Never applies any concrete action (the skeleton rejects anything
//!    other than `NoOp`).
//!
//! This proves the skeleton's central invariant: `Runtime::run` is a
//! strict client of the `StateStore` trait, and a policy that declines
//! to emit actions leads to graceful — not hung — shutdown.

use std::time::Duration;

use cosmon_filestore::FileStore;
use cosmon_runtime::{NoOpExecutor, NoOpPolicy, Runtime, RuntimeConfig, ShutdownReason};
use cosmon_state::StateStore;
use tempfile::TempDir;

#[test]
fn runtime_with_noop_policy_drains_gracefully_on_empty_fleet() {
    // Arrange: an empty FileStore in a throwaway tempdir.
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    // Sanity: the store is genuinely empty before the runtime touches it.
    let molecules = store
        .list_molecules(&cosmon_state::MoleculeFilter::default())
        .expect("list empty store");
    assert!(
        molecules.is_empty(),
        "precondition: fresh FileStore must have no molecules"
    );

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(1),
        max_runtime: Some(Duration::from_secs(2)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };

    let mut runtime = Runtime::new(
        Box::new(store),
        Box::new(NoOpPolicy),
        Box::new(NoOpExecutor),
        config,
    );

    // Act: run the loop.
    let report = runtime.run().expect("runtime should not error");

    // Assert: NoOpPolicy returns an empty action vector on the first tick,
    // so the loop must exit via PolicyDrained (not Deadline, not Signal).
    assert_eq!(
        report.reason,
        ShutdownReason::PolicyDrained,
        "empty NoOp policy should drain on first tick, got {:?}",
        report.reason
    );
    assert!(
        report.ticks >= 1,
        "runtime must execute at least one tick before drain, got {}",
        report.ticks
    );
    assert_eq!(
        report.actions_applied, 0,
        "NoOp policy never emits concrete actions, got {}",
        report.actions_applied
    );
}

#[test]
fn runtime_honors_shutdown_signal_before_policy_drains() {
    // Uses a trivial Policy that keeps the loop alive by emitting a NoOp.
    // This proves the shutdown signal actually stops the loop, independent
    // of the policy's drain decision.
    struct AlwaysNoOp;
    impl cosmon_runtime::Policy for AlwaysNoOp {
        fn next_actions(
            &mut self,
            _snapshot: &cosmon_runtime::FleetSnapshot,
        ) -> Vec<cosmon_runtime::RuntimeAction> {
            vec![cosmon_runtime::RuntimeAction::NoOp]
        }
    }

    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(1),
        // Deadline guard: if shutdown signal is broken, test still terminates.
        max_runtime: Some(Duration::from_secs(2)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };

    let mut runtime = Runtime::new(
        Box::new(store),
        Box::new(AlwaysNoOp),
        Box::new(NoOpExecutor),
        config,
    );
    let handle = runtime.shutdown_handle();

    // Trip the signal before starting — the first tick should observe it.
    handle.trip();

    let report = runtime.run().expect("runtime should not error");
    assert_eq!(
        report.reason,
        ShutdownReason::SignalTripped,
        "tripped signal must yield SignalTripped reason, got {:?}",
        report.reason
    );
}
