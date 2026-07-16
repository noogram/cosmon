// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical fleet snapshot — shared, `pub` fixture for anti-drift tests.
//!
//! The TUI (`cs peek`) and the HTTP dashboard (`cosmon-cockpit-http`) are
//! two adapters over the same observability port. Pinning one snapshot
//! here lets each adapter prove, in its own test, that it renders the
//! same underlying facts. When this fixture changes, every adapter must
//! update in lock-step — and the anti-drift integration test (see
//! `tests/anti_drift.rs`) makes divergence a build failure.
//!
//! This fixture lives in the library (not under `tests/`) precisely so
//! that downstream crates can `use cosmon_observability::fixture::…`.
//! The previous placement in `tests/fixtures.rs` made it unreachable
//! across crate boundaries, which defeated the purpose.

use chrono::{TimeZone, Utc};

use crate::aggregate::FleetSnapshot;
use crate::event::Event;
use crate::molecule::{Molecule, MoleculeStatus};
use crate::session::Session;
use crate::worker::{EnergyBudget, Worker};

/// The canonical two-project, two-socket fleet snapshot used by adapter tests.
///
/// Deterministic: the timestamp is frozen so rendered output is reproducible
/// across runs.
#[must_use]
pub fn canonical_snapshot() -> FleetSnapshot {
    let t = Utc.with_ymd_and_hms(2026, 4, 12, 10, 0, 0).unwrap();
    let mut s = FleetSnapshot::new();

    s.push_session(Session {
        name: "cosmon-alpha".into(),
        socket: "/private/tmp/tmux-501/default".into(),
        project_root: "/proj/alpha".into(),
        molecule_id: Some("mol-alpha".into()),
        worker_id: Some("w-alpha".into()),
        last_activity: Some(t),
    });
    s.push_session(Session {
        name: "cosmon-beta".into(),
        socket: "/private/tmp/tmux-501/fleet-b".into(),
        project_root: "/proj/beta".into(),
        molecule_id: Some("mol-beta".into()),
        worker_id: Some("w-beta".into()),
        last_activity: Some(t),
    });

    s.insert_molecule(Molecule {
        id: "mol-alpha".into(),
        title: "Alpha task".into(),
        kind: "task".into(),
        status: MoleculeStatus::Running,
        project_root: "/proj/alpha".into(),
        session: Some("cosmon-alpha".into()),
        updated_at: t,
    });
    s.insert_molecule(Molecule {
        id: "mol-beta".into(),
        title: "Beta issue".into(),
        kind: "issue".into(),
        status: MoleculeStatus::Pending,
        project_root: "/proj/beta".into(),
        session: Some("cosmon-beta".into()),
        updated_at: t,
    });

    s.insert_worker(Worker {
        id: "w-alpha".into(),
        molecule_id: Some("mol-alpha".into()),
        session: "cosmon-alpha".into(),
        energy: EnergyBudget {
            input_tokens: 1_000,
            output_tokens: 500,
            cost_usd: 0.0,
            context_window: Some(1_000_000),
        },
        live: "working".into(),
        role: crate::worker::WorkerRole::Cognition,
    });
    s.insert_worker(Worker {
        id: "w-beta".into(),
        molecule_id: Some("mol-beta".into()),
        session: "cosmon-beta".into(),
        energy: EnergyBudget::default(),
        live: "idle".into(),
        role: crate::worker::WorkerRole::Cognition,
    });

    s.push_event(Event {
        molecule_id: "mol-alpha".into(),
        kind: "nucleated".into(),
        at: t,
        evidence: Some("alpha born".into()),
    });
    s.push_event(Event {
        molecule_id: "mol-alpha".into(),
        kind: "evolved".into(),
        at: t,
        evidence: Some("step 1/2".into()),
    });

    s
}

/// Canonical signals that must survive every adapter rendering.
///
/// This is the minimal set of strings that both the TUI and the HTTP
/// JSON projection are required to emit. A renderer that drops any of
/// these has drifted from the port contract.
#[must_use]
pub fn canonical_signals() -> Vec<&'static str> {
    vec![
        // sessions
        "cosmon-alpha",
        "cosmon-beta",
        // molecule ids
        "mol-alpha",
        "mol-beta",
        // molecule titles
        "Alpha task",
        "Beta issue",
        // worker ids
        "w-alpha",
        "w-beta",
        // liveness hints
        "working",
        "idle",
    ]
}
