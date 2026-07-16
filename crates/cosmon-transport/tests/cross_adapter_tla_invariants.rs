// SPDX-License-Identifier: AGPL-3.0-only

//! Adversary TLA+ skeleton invariants over the Worker-Spawn Port.
//!
//! Three invariants are pinned by behavioural tests over the
//! `events.jsonl` produced by the Worker-Spawn Port emit helpers:
//!
//! - **I1 — `ws1_implies_ws5`**: every `WorkerSpawnAttempted` (WS-1)
//!   for a `(mol_id, worker_id)` pair is *eventually* followed by a
//!   terminal event for the same pair — `AdapterHandleReconciled`
//!   (WS-5), `WorkerSpawnRolledBack` (WS-1''), or `WorkerSpawnFailed`
//!   (WS-1'). The pre-W3 trail falsified I1 in two scenarios: a
//!   backend `spawn_worker` error (only WS-1) and a post-lock RMW
//!   rollback (only WS-1).
//!
//! - **I2 — `briefing_seal_preserved`**: every
//!   `AdapterBriefingConsumed` (WS-4) carries an `observed_seal` that
//!   equals the hash of the bytes the adapter actually read. The
//!   `consume_briefing` helper computes the observed seal under the
//!   exact byte sequence it returns to the caller; I2 says the
//!   helper does not double-read or partially-read.
//!
//! - **I3 — `no_rollback_without_terminal_event`**: every worker
//!   that reaches a Dead-equivalent state on the wire (rollback or
//!   spawn failure) has a terminal event for its `worker_id`. I3 is
//!   the structural sibling of I1 from the rollback perspective: I1
//!   says "every attempt has a terminus"; I3 says "every terminus is
//!   on the wire."
//!
//! The tests are deliberately property-flavoured rather than
//! exhaustive enumerations: they drive a handful of spawn / rollback /
//! kill sequences through the emit helpers and assert the three
//! predicates hold over the resulting envelope stream.

use std::fs;
use std::path::Path;

use chrono::Utc;
use cosmon_core::event_v2::{
    AdapterHandleState, AdapterProbeKind, AdapterProbeResult, Envelope, EventV2,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_state::events::worker_spawn as ws;
use tempfile::tempdir;

fn mol(id: &str) -> MoleculeId {
    MoleculeId::new(id).unwrap()
}

fn wkr(id: &str) -> WorkerId {
    WorkerId::new(id).unwrap()
}

fn envelopes(dir: &Path) -> Vec<Envelope> {
    let raw = fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Envelope::from_line(l).expect("envelope must parse"))
        .collect()
}

/// I1 evaluator — for every WS-1 in the trail, scan forward for a
/// terminal event (WS-5 / `rolled_back` / failed) carrying the same
/// `(mol_id, worker_id)` pair. Returns the index of the offending
/// WS-1 on failure, `None` on success.
fn first_dangling_ws1(events: &[Envelope]) -> Option<usize> {
    for (i, env) in events.iter().enumerate() {
        let EventV2::WorkerSpawnAttempted {
            mol_id, worker_id, ..
        } = &env.event
        else {
            continue;
        };
        let mut terminal = false;
        for env2 in &events[i..] {
            let (EventV2::AdapterHandleReconciled {
                mol_id: m,
                worker_id: w,
                ..
            }
            | EventV2::WorkerSpawnRolledBack {
                mol_id: m,
                worker_id: w,
                ..
            }
            | EventV2::WorkerSpawnFailed {
                mol_id: m,
                worker_id: w,
                ..
            }) = &env2.event
            else {
                continue;
            };
            if m == mol_id && w == worker_id {
                terminal = true;
                break;
            }
        }
        if !terminal {
            return Some(i);
        }
    }
    None
}

/// I1 — backend spawn refusal path: a `WorkerSpawnAttempted` for a
/// spawn that subsequently failed is closed by `WorkerSpawnFailed`.
///
/// Pre-W3 this case left WS-1 dangling forever (adversary F1.3); the
/// terminal partner was wired in `claude.rs` / `aider.rs`.
#[test]
fn invariant_i1_ws1_followed_by_terminal_on_spawn_failure() {
    let dir = tempdir().unwrap();
    let m = mol("task-20260519-aaaa");
    let w = wkr("polecat-i1-fail");
    ws::emit_worker_spawn_attempted(dir.path(), &m, &w, "claude", "/wt", "uuid-fail", 0, None);
    ws::emit_worker_spawn_failed(dir.path(), &m, &w, "claude", "tmux not on PATH");
    let events = envelopes(dir.path());
    assert!(
        first_dangling_ws1(&events).is_none(),
        "I1 falsified: WS-1 for {w} has no terminal partner"
    );
}

/// I1 — post-lock RMW rollback path: a `WorkerSpawnAttempted` for a
/// spawn that subsequently rolled back is closed by
/// `WorkerSpawnRolledBack`.
///
/// Pre-W3 this case left WS-1 dangling forever (adversary F4.1); the
/// terminal partner was wired in `cs tackle`.
#[test]
fn invariant_i1_ws1_followed_by_terminal_on_rollback() {
    let dir = tempdir().unwrap();
    let m = mol("task-20260519-bbbb");
    let w = wkr("polecat-i1-rb");
    ws::emit_worker_spawn_attempted(dir.path(), &m, &w, "claude", "/wt", "uuid-rb", 42, None);
    ws::emit_worker_spawn_rolled_back(dir.path(), &m, &w, "claude", "pending");
    let events = envelopes(dir.path());
    assert!(
        first_dangling_ws1(&events).is_none(),
        "I1 falsified: rolled-back WS-1 for {w} has no terminal partner"
    );
}

/// I1 — happy path: a `WorkerSpawnAttempted` followed by the normal
/// WS-5 reconciliation also satisfies the invariant. Sanity control.
#[test]
fn invariant_i1_ws1_followed_by_ws5_on_happy_path() {
    let dir = tempdir().unwrap();
    let m = mol("task-20260519-cccc");
    let w = wkr("polecat-i1-ok");
    let now = Utc::now();
    ws::emit_worker_spawn_attempted(dir.path(), &m, &w, "claude", "/wt", "uuid-ok", 7, None);
    ws::emit_adapter_handle_reconciled(
        dir.path(),
        &m,
        &w,
        "claude",
        AdapterHandleState::ReleasedClean,
        Some(now),
        now,
        0,
    );
    let events = envelopes(dir.path());
    assert!(first_dangling_ws1(&events).is_none(), "I1 falsified");
}

/// I1 — multi-worker enumeration: alternating WS-1 emissions for
/// different `(mol_id, worker_id)` pairs each close with their own
/// terminal event without crosstalk. Property-flavoured spawn-
/// rollback-kill enumeration named in the briefing.
#[test]
fn invariant_i1_holds_for_alternating_spawn_rollback_kill_sequence() {
    let dir = tempdir().unwrap();
    let m1 = mol("task-20260519-d111");
    let m2 = mol("task-20260519-d222");
    let m3 = mol("task-20260519-d333");
    let w1 = wkr("polecat-seq-a");
    let w2 = wkr("polecat-seq-b");
    let w3 = wkr("polecat-seq-c");
    let now = Utc::now();

    // Three attempts: happy / rolled-back / failed, interleaved.
    ws::emit_worker_spawn_attempted(dir.path(), &m1, &w1, "claude", "/wt", "u1", 1, None);
    ws::emit_worker_spawn_attempted(dir.path(), &m2, &w2, "aider", "/wt", "u2", 2, None);
    ws::emit_worker_spawn_attempted(dir.path(), &m3, &w3, "claude", "/wt", "u3", 3, None);

    ws::emit_worker_spawn_rolled_back(dir.path(), &m2, &w2, "aider", "queued");
    ws::emit_adapter_handle_reconciled(
        dir.path(),
        &m1,
        &w1,
        "claude",
        AdapterHandleState::ReleasedClean,
        Some(now),
        now,
        0,
    );
    ws::emit_worker_spawn_failed(dir.path(), &m3, &w3, "claude", "backend refused");

    let events = envelopes(dir.path());
    assert!(
        first_dangling_ws1(&events).is_none(),
        "I1 falsified for at least one worker in the alternating sequence"
    );
}

/// I2 — `briefing_seal_preserved`: every `AdapterBriefingConsumed`
/// event must carry an `observed_seal` equal to the BLAKE3 hash of the
/// bytes the helper read. Drives `claude::consume_briefing` over a
/// known fixture and asserts the on-wire seal matches.
#[test]
fn invariant_i2_briefing_seal_preserved_after_consume() {
    let dir = tempdir().unwrap();
    let briefing = dir.path().join("briefing.md");
    let bytes: &[u8] = b"### Brief\n- step 1\n- step 2\n";
    fs::write(&briefing, bytes).unwrap();
    let expected = blake3::hash(bytes).to_hex().to_string();

    let t = cosmon_transport::spawn::AdapterTelemetry::new(
        mol("task-20260519-i2cc"),
        wkr("polecat-i2"),
        dir.path().to_owned(),
        "uuid-i2",
    );
    cosmon_transport::claude::consume_briefing(&briefing, "recorded-i2", Some(&t))
        .expect("consume_briefing must read the file");

    let events = envelopes(dir.path());
    let consumed = events
        .iter()
        .find_map(|e| match &e.event {
            EventV2::AdapterBriefingConsumed {
                briefing_seal_observed,
                ..
            } => Some(briefing_seal_observed.clone()),
            _ => None,
        })
        .expect("AdapterBriefingConsumed must land");
    assert_eq!(
        consumed, expected,
        "I2 falsified: observed_seal on the wire does not match hash(bytes-read)"
    );
}

/// I3 — `no_rollback_without_terminal_event`: every worker that
/// transitions to Dead-equivalent (rollback or failure) must have a
/// terminal event on the wire matching its `worker_id`. The
/// invariant is stated over the set of "Dead-equivalent" workers;
/// here we encode the set as "any worker whose `WorkerSpawnAttempted`
/// is not followed by `AdapterHandleReconciled` with `ReleasedClean`"
/// and assert each carries a `WorkerSpawnRolledBack` /
/// `WorkerSpawnFailed`.
#[test]
fn invariant_i3_every_dead_worker_has_terminal_event() {
    let dir = tempdir().unwrap();
    let m_ok = mol("task-20260519-3aaa");
    let m_rb = mol("task-20260519-3bbb");
    let m_fail = mol("task-20260519-3ccc");
    let w_ok = wkr("polecat-i3-ok");
    let w_rb = wkr("polecat-i3-rb");
    let w_fail = wkr("polecat-i3-fail");
    let now = Utc::now();

    ws::emit_worker_spawn_attempted(dir.path(), &m_ok, &w_ok, "claude", "/wt", "uok", 11, None);
    ws::emit_adapter_handle_reconciled(
        dir.path(),
        &m_ok,
        &w_ok,
        "claude",
        AdapterHandleState::ReleasedClean,
        Some(now),
        now,
        0,
    );

    ws::emit_worker_spawn_attempted(dir.path(), &m_rb, &w_rb, "claude", "/wt", "urb", 22, None);
    ws::emit_worker_spawn_rolled_back(dir.path(), &m_rb, &w_rb, "claude", "pending");

    ws::emit_worker_spawn_attempted(
        dir.path(),
        &m_fail,
        &w_fail,
        "aider",
        "/wt",
        "ufail",
        0,
        None,
    );
    ws::emit_worker_spawn_failed(
        dir.path(),
        &m_fail,
        &w_fail,
        "aider",
        "tmux missing on PATH",
    );

    let events = envelopes(dir.path());

    // Workers whose terminal is rollback or failure must have a
    // matching event somewhere in the trail.
    for (worker, expected_terminal) in [(&w_rb, "rolled_back"), (&w_fail, "failed")] {
        let has_terminal = events.iter().any(|e| match &e.event {
            EventV2::WorkerSpawnRolledBack { worker_id, .. }
                if expected_terminal == "rolled_back" =>
            {
                worker_id == worker
            }
            EventV2::WorkerSpawnFailed { worker_id, .. } if expected_terminal == "failed" => {
                worker_id == worker
            }
            _ => false,
        });
        assert!(
            has_terminal,
            "I3 falsified: worker {worker} reached Dead-equivalent but no \
             {expected_terminal} terminal event on the wire"
        );
    }

    // The happy-path worker must NOT carry a rollback or failure
    // terminal — sanity guard against a false-positive emission.
    let healthy_terminal_count = events
        .iter()
        .filter(|e| match &e.event {
            EventV2::WorkerSpawnRolledBack { worker_id, .. }
            | EventV2::WorkerSpawnFailed { worker_id, .. } => worker_id == &w_ok,
            _ => false,
        })
        .count();
    assert_eq!(
        healthy_terminal_count, 0,
        "I3 false-positive: clean worker {w_ok} carries a Dead-equivalent \
         terminal event"
    );
}

/// **C8 cat-test sanity** — WS-2 / WS-3 / WS-4 are also emitted by
/// the unit-level adapters in C2; the C8 `kinds_for(adapter) ⊇
/// {ws1..ws5}` shape is
/// pinned at the integration perimeter by
/// `cross_adapter_event_lineage_equivalence_modulo_adapter_name`.
/// This test reproduces the WS-1..WS-5 shape without tmux/binaries
/// so the CI gate fails fast if a future emit helper goes silent.
#[test]
fn ws1_through_ws5_lineage_is_observable_via_emit_helpers_alone() {
    let dir = tempdir().unwrap();
    let m = mol("task-20260519-cat0");
    let w = wkr("polecat-cat-0");
    let now = Utc::now();

    ws::emit_worker_spawn_attempted(dir.path(), &m, &w, "claude", "/wt", "u", 100, None);
    ws::emit_adapter_liveness_probed(
        dir.path(),
        &m,
        &w,
        "claude",
        AdapterProbeKind::PaneSignature,
        AdapterProbeResult::Alive {
            evidence: "alive".into(),
        },
        0,
    );
    ws::emit_adapter_pane_signature_checked(
        dir.path(),
        &m,
        &w,
        "claude",
        &["claude".into()],
        "claude",
        true,
        cosmon_core::event_v2::PerturbationChannel::Propulsion,
    );
    // WS-4 is exercised in invariant_i2_* via consume_briefing; assert
    // here that the FSM-prescribed sequence is observable end-to-end
    // through the helpers alone.
    ws::emit_adapter_handle_reconciled(
        dir.path(),
        &m,
        &w,
        "claude",
        AdapterHandleState::ReleasedClean,
        Some(now),
        now,
        0,
    );

    let kinds: Vec<&'static str> = envelopes(dir.path())
        .iter()
        .map(|e| match e.event {
            EventV2::WorkerSpawnAttempted { .. } => "ws1",
            EventV2::AdapterLivenessProbed { .. } => "ws2",
            EventV2::AdapterPaneSignatureChecked { .. } => "ws3",
            EventV2::AdapterBriefingConsumed { .. } => "ws4",
            EventV2::AdapterHandleReconciled { .. } => "ws5",
            _ => "other",
        })
        .collect();
    assert!(kinds.contains(&"ws1"), "lineage: WS-1 absent ({kinds:?})");
    assert!(kinds.contains(&"ws2"), "lineage: WS-2 absent ({kinds:?})");
    assert!(kinds.contains(&"ws3"), "lineage: WS-3 absent ({kinds:?})");
    assert!(kinds.contains(&"ws5"), "lineage: WS-5 absent ({kinds:?})");
}
