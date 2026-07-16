// SPDX-License-Identifier: AGPL-3.0-only

//! Property tests for **I3 — ADDITIVE-COUNTERS** (ADR-110 §I3).
//!
//! ## The invariant
//!
//! ADR-110 §I3 (`docs/adr/110-single-writer-trunk-and-coordination-invariants.md`):
//!
//! > Shared counters (`surface_freeze`, fleet-wide telemetry,
//! > drain-level aggregates) are **strictly additive** — either CRDTs
//! > (G-Counter, [@shapiro2011crdt, §3.1.1]) or routed through a single
//! > sequencer. **Never** read-modify-write a JSON field from a worker
//! > process. […] The event-fold form is *commutative* and *idempotent*
//! > at the storage layer — losing or re-applying an event does not
//! > corrupt the aggregate, only delays its convergence.
//!
//! This file fuzzes that claim: for an additive counter modelled as a
//! fold over an append-only event log, the derived total is invariant
//! under **delivery order** and under **event duplication**, and it
//! never loses an increment — whereas the naïve read-modify-write shape
//! that I3 forbids *does* lose increments under interleaving.
//!
//! ## Provenance and a falsified premise (read this)
//!
//! Phase 3 / Étage 3 — ADR-110 §"Phase 3 — conditional" point 2.
//!
//! The original brief asked for a property test that "the
//! `surface_freeze` event-fold can never produce a non-additive
//! counter." **Executing the premise falsified it** —
//! see an internal audit erratum:
//!
//! - ADR-110 cites commit `950986727` as "already on main" enforcing
//!   I3. It was **not on main** at the time (on no branch reachable
//!   from `main`).
//! - That commit's `surface_freeze` event-fold is the *compile-time §8p
//!   API-surface list* (ADR-080), **not** a concurrent runtime counter.
//!
//! **RESOLVED (2026-05-26, Phase 1 recovery).** The
//! compile-time event-fold from `950986727` has now been re-derived onto
//! `main`: `frozen_api_surface()` projects from the append-only
//! `data/surface_events.txt` (29 routes, byte-identical), and the
//! hand-edited `assert_eq!(surface.len(), 29)` is gone — see
//! `tests/api_surface_freeze.rs` (`surface_length_matches_event_log`).
//! The compile-time additive-counter anti-pattern I3 names is now
//! actually abolished on `main`.
//!
//! The other half of §I3 — a concurrent **runtime** `surface_freeze`
//! counter (breakage #3) — was never implemented and is a distinct
//! mechanism from the compile-time surface fold. These tests therefore
//! still pin the **I3 invariant itself**, grounded in ADR-110's own
//! definition (G-Counter / grow-only set), so that whatever concrete
//! runtime additive counter lands next has an executable contract to
//! satisfy — and so the naïve shape it must avoid is demonstrated, not
//! merely asserted.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;

/// A grow-only counter folded from an append-only event log, in the
/// shape ADR-110 §I3 prescribes (G-Counter, [@shapiro2011crdt §3.1.1]).
///
/// Each replica reports a monotonically non-decreasing local count. The
/// fold keeps the per-replica maximum; the aggregate is the sum. `max`
/// is commutative, associative, and idempotent — so the fold converges
/// to the same total under any delivery order and any re-delivery of
/// events, which is precisely I3.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
struct GCounter {
    per_replica: BTreeMap<u8, u64>,
}

impl GCounter {
    /// Apply one observation: "replica `r` has reached local count `v`".
    fn apply(&mut self, r: u8, v: u64) {
        let slot = self.per_replica.entry(r).or_insert(0);
        *slot = (*slot).max(v);
    }

    /// Fold an event log into a counter.
    fn fold(events: &[(u8, u64)]) -> Self {
        let mut c = Self::default();
        for &(r, v) in events {
            c.apply(r, v);
        }
        c
    }

    /// The additive aggregate.
    fn total(&self) -> u64 {
        self.per_replica.values().copied().sum()
    }
}

/// Reference truth, computed independently of the fold: the additive
/// total is the sum over replicas of the maximum value each replica
/// reported. If the fold ever disagrees with this, I3 is violated.
fn reference_total(events: &[(u8, u64)]) -> u64 {
    let mut max_by_replica: BTreeMap<u8, u64> = BTreeMap::new();
    for &(r, v) in events {
        let slot = max_by_replica.entry(r).or_insert(0);
        *slot = (*slot).max(v);
    }
    max_by_replica.values().copied().sum()
}

/// Strategy: an event log of `(replica, observed_count)` pairs. Replica
/// space and value space are bounded so totals stay well inside `u64`.
fn event_log() -> impl Strategy<Value = Vec<(u8, u64)>> {
    prop::collection::vec((0u8..8, 0u64..1000), 0..128)
}

proptest! {
    /// I3-core — the fold equals the additive reference total. The
    /// event-fold cannot under- or over-count.
    #[test]
    fn fold_equals_additive_reference_total(events in event_log()) {
        prop_assert_eq!(GCounter::fold(&events).total(), reference_total(&events));
    }

    /// I3-commutativity — the fold is order-independent. Any permutation
    /// of the event log yields the same counter (hence the same total).
    #[test]
    fn fold_is_order_independent(events in event_log(), seed in any::<u64>()) {
        let base = GCounter::fold(&events);

        // A deterministic permutation derived from `seed` (Fisher–Yates
        // with a tiny LCG — no rng dependency needed for shrinking).
        let mut shuffled = events.clone();
        let mut state = seed | 1;
        let len = shuffled.len();
        for i in (1..len).rev() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let j = usize::try_from((state >> 33) % (i as u64 + 1)).unwrap();
            shuffled.swap(i, j);
        }

        prop_assert_eq!(GCounter::fold(&shuffled), base.clone());
        prop_assert_eq!(GCounter::fold(&shuffled).total(), base.total());
    }

    /// I3-idempotence — re-applying an arbitrary already-seen subset of
    /// events does not change the counter. Losing or re-delivering an
    /// event (the lossy-broadcast / SSE-reconnect case) is harmless.
    #[test]
    fn fold_is_idempotent_under_redelivery(
        events in event_log(),
        dup_mask in prop::collection::vec(any::<bool>(), 0..128),
    ) {
        let base = GCounter::fold(&events);

        let mut with_dups = events.clone();
        for (i, ev) in events.iter().enumerate() {
            if *dup_mask.get(i).unwrap_or(&false) {
                with_dups.push(*ev);
            }
        }
        prop_assert_eq!(GCounter::fold(&with_dups), base);
    }

    /// I3-monotonicity — the aggregate never drops below any single
    /// observation, and appending events never decreases the total
    /// (grow-only).
    #[test]
    fn fold_is_grow_only(events in event_log(), tail in event_log()) {
        let before = GCounter::fold(&events).total();

        let mut grown = events.clone();
        grown.extend_from_slice(&tail);
        let after = GCounter::fold(&grown).total();

        prop_assert!(after >= before, "appending events must never lose count");
        if let Some(&(_, v)) = events.iter().max_by_key(|(_, v)| *v) {
            prop_assert!(before >= v, "aggregate must cover its largest observation");
        }
    }
}

// ---------------------------------------------------------------------------
// The bug I3 forbids — demonstrated, not just asserted.
// ---------------------------------------------------------------------------

/// Simulate the **naïve read-modify-write** shape ADR-110 §I3 forbids:
/// a single shared scalar, where each "increment" is `read; +1; write`,
/// and two workers interleave their reads before either writes.
///
/// `rounds` workers each perform one increment; with the worst
/// interleaving (everyone reads the same base, then everyone writes
/// `base + 1`), all but one increment is lost — the last writer wins.
fn naive_rmw_lost_update(initial: u64, workers: usize) -> u64 {
    // Every worker reads `initial`, computes `initial + 1`, and the last
    // write wins. `workers` increments collapse to a single +1.
    let mut shared = initial;
    let read_snapshot = shared; // all workers read the same value first
    for _ in 0..workers {
        shared = read_snapshot + 1; // each overwrites with the same value
    }
    shared
}

/// Concrete companion to the proptests: the precise breakage #3 from
/// ADR-110's Context — *"`surface_freeze` counter non-additive —
/// concurrent read-modify-write on a JSON counter, with last-writer-wins
/// erasing intermediate increments."* The naïve scalar loses updates;
/// the event-fold does not.
#[test]
fn naive_rmw_loses_increments_event_fold_does_not() {
    let workers = 8usize;

    // Naïve RMW under worst interleaving: 8 increments collapse to 1.
    let naive = naive_rmw_lost_update(0, workers);
    assert_eq!(naive, 1, "last-writer-wins erases 7 of 8 increments");
    assert!(
        naive < workers as u64,
        "the bug: naïve counter under-counts"
    );

    // Event-fold of the same 8 increments, modelled as 8 distinct
    // single-increment events from 8 distinct replicas. No matter the
    // delivery order, the additive total is exactly 8.
    let n_workers = u8::try_from(workers).unwrap();
    let events: Vec<(u8, u64)> = (0..n_workers).map(|r| (r, 1)).collect();
    assert_eq!(GCounter::fold(&events).total(), workers as u64);

    // …and re-delivering every event (lossy broadcast replay) keeps it 8.
    let mut replayed = events.clone();
    replayed.extend_from_slice(&events);
    assert_eq!(GCounter::fold(&replayed).total(), workers as u64);
}

/// A grow-only *set* is the other canonical I3 shape (the form the §8p
/// API-surface fold now uses on `main`: the surface size is
/// `SURFACE_EVENTS.len()`, never a hand-written integer — see
/// `tests/api_surface_freeze.rs`). Distinct events counted once,
/// duplicates idempotent — the executable contract the erratum's
/// recommended fix had to satisfy, now satisfied.
#[test]
fn grow_only_set_count_is_additive_and_idempotent() {
    let events = ["GET /v1/a", "POST /v1/b", "GET /v1/a", "DELETE /v1/c"];
    let surface: BTreeSet<&str> = events.iter().copied().collect();
    assert_eq!(surface.len(), 3, "the duplicate 'GET /v1/a' counts once");

    // Re-folding (appending the same events) does not change the count —
    // unlike the pre-recovery hand-edited `assert_eq!(surface.len(), 29)`,
    // which two parallel branches each bumped and then collided at merge.
    let mut again: BTreeSet<&str> = surface.clone();
    again.extend(events.iter().copied());
    assert_eq!(again.len(), surface.len());
}
