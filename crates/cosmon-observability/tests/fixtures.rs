// SPDX-License-Identifier: AGPL-3.0-only

//! Fixture invariants — asserts the canonical snapshot shape.
//!
//! The snapshot itself now lives in `src/fixture.rs` so it is reachable
//! across crate boundaries (the TUI and the HTTP dashboard both consume
//! it). This integration test only pins the high-level invariants.

use cosmon_observability::{fixture::canonical_snapshot, MoleculeId, SessionFilter, WorkerId};

#[test]
fn snapshot_covers_two_projects() {
    let snap = canonical_snapshot();
    assert_eq!(snap.session_count(), 2);
    assert_eq!(snap.molecule_count(), 2);
}

#[test]
fn filter_by_project_selects_one_session() {
    let snap = canonical_snapshot();
    let filter = SessionFilter {
        project_root: Some("/proj/alpha".into()),
        ..SessionFilter::default()
    };
    let got = snap.list_sessions(&filter);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "cosmon-alpha");
}

#[test]
fn filter_by_socket_spans_projects() {
    let snap = canonical_snapshot();
    let filter = SessionFilter {
        socket: Some("/private/tmp/tmux-501/fleet-b".into()),
        ..SessionFilter::default()
    };
    assert_eq!(snap.list_sessions(&filter).len(), 1);
}

#[test]
fn events_for_alpha_has_two_entries() {
    let snap = canonical_snapshot();
    let events = snap.events_for(&MoleculeId("mol-alpha".into()));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "nucleated");
}

#[test]
fn energy_of_alpha_worker_totals_1500() {
    let snap = canonical_snapshot();
    let e = snap.energy_for(&WorkerId("w-alpha".into())).unwrap();
    assert_eq!(e.total(), 1_500);
}
