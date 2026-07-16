// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests covering ADR-052's ghost detection surface across
//! the public `cosmon_core::run_state` API.
//!
//! Two concerns live here:
//!
//! 1. **Shape tests** — confirm that [`RunState::ghost`] composes correctly
//!    across crate boundaries (private fields are never reached).
//! 2. **The 18–19 April fixture table** — one entry per empirical ghost
//!    (3 cosmon + 6 mailroom). The table is what ADR-052 §"9 ghosts of
//!    18–19 April" names as the regression surface; a change to the
//!    detection logic that drops coverage on any of them must fail here
//!    before the unit tests catch it in a more abstract form.

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::run_state::{
    project_run_state, BranchState, GhostKind, Intent, Liveness, RunState, Terminus, Witness,
};
use cosmon_core::worker::TransportState;

/// Default probe TTL used by the shape-tests.
///
/// Real callers (`cs patrol`, `cs project`) pick this from config; for the
/// regression we want a value that is strictly longer than `1 µs` (so
/// fresh witnesses aren't flagged) and strictly shorter than the "past"
/// offset used to simulate stale probes.
const PROBE_TTL: Duration = Duration::from_secs(90);

// ---------------------------------------------------------------------------
// Shape 1 — the dfd8 / DeadPane family
//
// Pilot still says Run. The pane died. Whoever reads fleet.json sees
// "registered" but the kitchen is empty.
// ---------------------------------------------------------------------------

#[test]
fn dead_pane_ghost_is_detected_via_public_api() {
    let witness = Witness::new(Liveness::Dead, BranchState::Unmerged);
    let state = RunState::with_witness(Intent::Run, witness);

    assert_eq!(
        state.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::DeadPane)
    );
}

#[test]
fn dead_pane_serializes_across_a_reload() {
    let witness = Witness::new(Liveness::Dead, BranchState::Unmerged);
    let state = RunState::with_witness(Intent::Run, witness);

    let json = serde_json::to_string(&state).unwrap();
    let reloaded: RunState = serde_json::from_str(&json).unwrap();

    assert_eq!(
        reloaded.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::DeadPane),
    );
}

// ---------------------------------------------------------------------------
// Shape 2 — the 192a / UnHarvested family
//
// Pilot declared Completed. Branch never merged. Morning-after ghost.
// ---------------------------------------------------------------------------

#[test]
fn un_harvested_ghost_is_detected_via_public_api() {
    let witness = Witness::new(Liveness::Alive, BranchState::Unmerged);
    let state = RunState::with_witness(Intent::Terminal(Terminus::Completed), witness);

    assert_eq!(
        state.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::UnHarvested),
    );
}

#[test]
fn un_harvested_survives_probe_becoming_stale() {
    // Even if the probe is stale, Completed-but-unmerged wins (I5 is
    // more specific than I10). The ghost is UnHarvested, not StaleProbe.
    let past = Utc::now() - ChronoDuration::seconds(600);
    let witness = Witness::at(past, Liveness::Alive, BranchState::Unmerged);
    let state = RunState::with_witness(Intent::Terminal(Terminus::Completed), witness);

    assert_eq!(
        state.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::UnHarvested),
    );
}

// ---------------------------------------------------------------------------
// Shape 3 — the c1cb / UnnamedMerge family
//
// Pilot rebased + force-pushed inline. Branch shows Merged. The state
// machine never recorded the transition. This is the Gödel sentence —
// *stateable* but not *enforceable* from inside cosmon.
// ---------------------------------------------------------------------------

#[test]
fn unnamed_merge_ghost_is_detected_via_public_api() {
    let witness = Witness::new(Liveness::Alive, BranchState::Merged);
    let state = RunState::with_witness(Intent::Run, witness);

    assert_eq!(
        state.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::UnnamedMerge),
    );
}

#[test]
fn unnamed_merge_persists_after_pilot_tries_to_pause() {
    let witness = Witness::new(Liveness::Alive, BranchState::Merged);
    let mut state = RunState::with_witness(Intent::Run, witness);

    // Pilot pauses after the fact — ghost still surfaces because the
    // branch was merged outside the state machine.
    state.write_intent(Intent::Pause);
    assert_eq!(
        state.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::UnnamedMerge),
    );
}

// ---------------------------------------------------------------------------
// Happy path — the runtime is healthy.
// ---------------------------------------------------------------------------

#[test]
fn healthy_running_state_has_no_ghost() {
    let witness = Witness::new(Liveness::Alive, BranchState::Unmerged);
    let state = RunState::with_witness(Intent::Run, witness);

    assert_eq!(state.ghost(Utc::now(), PROBE_TTL), None);
}

#[test]
fn clean_merge_terminal_has_no_ghost() {
    let witness = Witness::new(Liveness::Dead, BranchState::Merged);
    let state = RunState::with_witness(Intent::Terminal(Terminus::Merged), witness);

    assert_eq!(state.ghost(Utc::now(), PROBE_TTL), None);
}

// ---------------------------------------------------------------------------
// The 18–19 April fixture table — one row per empirical ghost.
// ---------------------------------------------------------------------------

/// A single empirical ghost — the 18–19 April log broken into rows the
/// detection logic must regress on, forever.
///
/// The integration test that walks this table is **the** guard against
/// silent drift in [`RunState::ghost`]. If a developer tightens or
/// loosens the pattern match in a way that drops any of these nine
/// incidents onto the wrong variant (or onto `None`), the test fails
/// loudly with the molecule id that regressed.
struct GhostFixture {
    /// Galaxy of origin (cosmon / mailroom).
    galaxy: &'static str,
    /// Molecule id as recorded in the 18–19 April audit.
    molecule_id: &'static str,
    /// Human-readable sketch of the drift shape.
    shape: &'static str,
    /// The state to detect on — built via the public `run_state` API.
    state: RunState,
    /// The single `GhostKind` variant this incident must map to.
    expected: GhostKind,
}

fn fixtures() -> Vec<GhostFixture> {
    // All three cosmon shapes + the six identical mailroom shapes.
    let completed_unmerged = || {
        let w = Witness::new(Liveness::Alive, BranchState::Unmerged);
        RunState::with_witness(Intent::Terminal(Terminus::Completed), w)
    };
    let running_dead = || {
        let w = Witness::new(Liveness::Dead, BranchState::Unmerged);
        RunState::with_witness(Intent::Run, w)
    };
    let running_merged = || {
        let w = Witness::new(Liveness::Alive, BranchState::Merged);
        RunState::with_witness(Intent::Run, w)
    };
    let vanished = RunState::running;

    vec![
        GhostFixture {
            galaxy: "cosmon",
            molecule_id: "task-20260413-dfd8",
            shape: "runtime kept emitting Evolve against a phantom session",
            state: running_dead(),
            expected: GhostKind::DeadPane,
        },
        GhostFixture {
            galaxy: "cosmon",
            molecule_id: "task-20260416-192a",
            shape: "Completed molecule, fleet registered, branch never merged",
            state: completed_unmerged(),
            expected: GhostKind::UnHarvested,
        },
        GhostFixture {
            galaxy: "cosmon",
            molecule_id: "task-20260413-c1cb",
            shape: "pilot rebased + force-pushed inline, merge outside the SM",
            state: running_merged(),
            expected: GhostKind::UnnamedMerge,
        },
        GhostFixture {
            galaxy: "mailroom",
            molecule_id: "/ask-d902",
            shape: "nucleated, never tackled; pilot answered inline",
            state: vanished(),
            expected: GhostKind::VanishedWorker,
        },
        GhostFixture {
            galaxy: "mailroom",
            molecule_id: "/ask-93a7",
            shape: "nucleated, never tackled; pilot answered inline",
            state: vanished(),
            expected: GhostKind::VanishedWorker,
        },
        GhostFixture {
            galaxy: "mailroom",
            molecule_id: "/ask-af87",
            shape: "nucleated, never tackled; pilot answered inline",
            state: vanished(),
            expected: GhostKind::VanishedWorker,
        },
        GhostFixture {
            galaxy: "mailroom",
            molecule_id: "/ask-ffc1",
            shape: "nucleated, never tackled; pilot answered inline",
            state: vanished(),
            expected: GhostKind::VanishedWorker,
        },
        GhostFixture {
            galaxy: "mailroom",
            molecule_id: "/ask-b387",
            shape: "nucleated, never tackled; pilot answered inline",
            state: vanished(),
            expected: GhostKind::VanishedWorker,
        },
        GhostFixture {
            galaxy: "mailroom",
            molecule_id: "/ask-f2a3",
            shape: "nucleated, never tackled; pilot answered inline",
            state: vanished(),
            expected: GhostKind::VanishedWorker,
        },
    ]
}

#[test]
fn fixture_table_has_exactly_nine_entries() {
    let rows = fixtures();
    assert_eq!(
        rows.len(),
        9,
        "the 18–19 April log records exactly 9 ghosts"
    );
}

#[test]
fn every_april_ghost_maps_to_expected_variant() {
    for f in fixtures() {
        let got = f.state.ghost(Utc::now(), PROBE_TTL);
        assert_eq!(
            got,
            Some(f.expected),
            "ghost for {} ({}) — shape: {} — must map to {:?}, got {:?}",
            f.galaxy,
            f.molecule_id,
            f.shape,
            f.expected,
            got,
        );
    }
}

#[test]
fn fixture_covers_every_empirical_variant() {
    use std::collections::HashSet;

    let variants: HashSet<_> = fixtures().iter().map(|f| f.expected).collect();
    // DeadPane (dfd8), UnHarvested (192a), UnnamedMerge (c1cb), and
    // VanishedWorker (mailroom six) — four of the five defined
    // variants. StaleProbe has no empirical 18–19 April incident but is
    // a reachable variant by construction (see unit test
    // `ghost_bonus_stale_probe`).
    assert!(variants.contains(&GhostKind::DeadPane));
    assert!(variants.contains(&GhostKind::UnHarvested));
    assert!(variants.contains(&GhostKind::UnnamedMerge));
    assert!(variants.contains(&GhostKind::VanishedWorker));
    assert_eq!(
        variants.len(),
        4,
        "the 9 empirical ghosts collapse to 4 named variants",
    );
}

// ---------------------------------------------------------------------------
// Display-side projection — the path that `cs ensemble` / `cs observe`
// will exercise once they call into the run-state module.
// ---------------------------------------------------------------------------

#[test]
fn projection_from_legacy_state_detects_dead_pane() {
    let rs = project_run_state(
        MoleculeStatus::Running,
        TransportState::Dead,
        None,
        Utc::now(),
    );
    assert_eq!(rs.ghost(Utc::now(), PROBE_TTL), Some(GhostKind::DeadPane));
}

#[test]
fn projection_from_legacy_state_detects_un_harvested() {
    let rs = project_run_state(
        MoleculeStatus::Completed,
        TransportState::Alive,
        None,
        Utc::now(),
    );
    assert_eq!(
        rs.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::UnHarvested)
    );
}

#[test]
fn projection_from_legacy_state_detects_unnamed_merge() {
    let rs = project_run_state(
        MoleculeStatus::Running,
        TransportState::Alive,
        Some(Utc::now()),
        Utc::now(),
    );
    assert_eq!(
        rs.ghost(Utc::now(), PROBE_TTL),
        Some(GhostKind::UnnamedMerge)
    );
}

#[test]
fn projection_healthy_running_is_not_a_ghost() {
    let rs = project_run_state(
        MoleculeStatus::Running,
        TransportState::Alive,
        None,
        Utc::now(),
    );
    assert_eq!(rs.ghost(Utc::now(), PROBE_TTL), None);
}
