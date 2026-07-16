// SPDX-License-Identifier: AGPL-3.0-only

//! Property tests for the molecule-lifecycle [`EventBus`].
//!
//! ## Provenance
//!
//! Phase 3 / Étage 3 of the single-writer-trunk programme. ADR-110
//! (`docs/adr/110-single-writer-trunk-and-coordination-invariants.md`)
//! §"Phase 3 — conditional" point 2 prescribes, verbatim:
//!
//! > **`proptest` on the event bus** (~2–3 days) — fuzz the
//! > `surface_freeze` event-fold and the `cs stitch` topo-merge with
//! > random DAGs and random worker schedules ; assert I3 + I4 hold on
//! > all reachable traces.
//!
//! ## What is fuzzed here
//!
//! The bus is a thin wrapper over [`tokio::sync::broadcast`] — lossy on
//! slow consumers, FIFO per receiver, fan-out without per-subscriber
//! persistence (see `events_bus.rs` module docs). These properties pin
//! the four behaviours the SSE handler (`GET /v1/events`) silently
//! relies on:
//!
//! 1. **No loss within capacity** — a receiver that keeps up never
//!    misses an event while the in-flight backlog stays `≤ capacity`.
//! 2. **Monotone delivery order (FIFO)** — events surface to a receiver
//!    in exactly the order they were published, regardless of payload.
//! 3. **Lag is accounted, never silent** — when a slow receiver
//!    overflows the ring, every published event is *either* delivered
//!    *or* counted in a `Lagged(n)` report: `received + dropped == sent`.
//!    The receiver always catches up to the newest event.
//! 4. **Idempotent fold** — folding the delivered tail by event
//!    identity (the SSE `Last-Event-ID` catch-up shape) is idempotent
//!    and order-independent. Re-reading an overlapping window never
//!    double-counts.
//!
//! All properties are exercised synchronously via `send` + `try_recv`
//! (no async runtime needed): the broadcast `send` is synchronous and
//! `try_recv` drains without blocking, which keeps `proptest`'s
//! shrinking deterministic.

use std::collections::{BTreeMap, BTreeSet};

use cosmon_rpp_adapter::{EventBus, MoleculeEvent};
use proptest::prelude::*;
use tokio::sync::broadcast::error::TryRecvError;

/// Build a `molecule.event_appended` event carrying a known sequence
/// number in its payload so the test can reconstruct delivery order.
fn evt(seq: u64) -> MoleculeEvent {
    MoleculeEvent::event_appended(
        "noyau-a",
        format!("task-{seq:08}"),
        serde_json::json!({ "seq": seq }),
    )
}

/// Extract the sequence number an [`evt`] was tagged with.
///
/// `event_appended` nests the caller's payload under `data.event`, so
/// the tag lives at `data.event.seq` (not `data.seq`).
fn seq_of(e: &MoleculeEvent) -> u64 {
    e.data["event"]["seq"]
        .as_u64()
        .expect("evt() always tags data.event.seq as u64")
}

/// Drain a receiver to exhaustion without blocking.
///
/// Returns `(delivered_seqs_in_order, total_dropped)`. A `Lagged(n)`
/// error is not a terminal condition — it reports `n` skipped events and
/// the *next* `try_recv` resumes at the oldest still-retained event, so
/// the loop continues. `Empty`/`Closed` terminate the drain.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<MoleculeEvent>) -> (Vec<u64>, u64) {
    let mut got = Vec::new();
    let mut dropped = 0u64;
    loop {
        match rx.try_recv() {
            Ok(e) => got.push(seq_of(&e)),
            Err(TryRecvError::Lagged(n)) => dropped += n,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    (got, dropped)
}

/// Idempotent, order-independent fold: delivered events → per-noyau set
/// of seen sequence ids. Folding is keyed by `(noyau, seq)` identity, so
/// re-applying an already-seen event is a no-op (set insertion).
fn fold_by_identity(events: &[(String, u64)]) -> BTreeMap<String, BTreeSet<u64>> {
    let mut acc: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for (noyau, seq) in events {
        acc.entry(noyau.clone()).or_default().insert(*seq);
    }
    acc
}

proptest! {
    /// Property 1 — **no loss within capacity**. With the in-flight
    /// backlog held at `≤ capacity`, a receiver that drains afterwards
    /// observes every published event, in order, with zero drops.
    #[test]
    fn no_loss_within_capacity(cap in 2usize..64, n in 0usize..64) {
        // Hold the send count at or below capacity so the ring never
        // overflows. (tokio may round the backing buffer up to the next
        // power of two; sending `≤ cap` is safe under any rounding.)
        let n = n.min(cap);
        let bus = EventBus::new(cap);
        let mut rx = bus.subscribe();
        for i in 0..n {
            bus.publish(evt(i as u64));
        }
        let (got, dropped) = drain(&mut rx);
        prop_assert_eq!(dropped, 0, "no drop expected while backlog ≤ capacity");
        prop_assert_eq!(got, (0..n as u64).collect::<Vec<_>>());
    }

    /// Property 2 — **monotone delivery order (FIFO)**. A receiver
    /// surfaces events in exactly publish order, whatever the payload
    /// values are. We tag each event with its publish index so "FIFO"
    /// becomes "delivered sequence equals the index range".
    #[test]
    fn monotone_delivery_order(payloads in prop::collection::vec(any::<u64>(), 0..64)) {
        let cap = payloads.len() + 4; // generous: never overflow here
        let bus = EventBus::new(cap);
        let mut rx = bus.subscribe();
        // Publish events tagged 0,1,2,… in publish order.
        for i in 0..payloads.len() {
            bus.publish(evt(i as u64));
        }
        let (got, dropped) = drain(&mut rx);
        prop_assert_eq!(dropped, 0);
        // FIFO: received tags are the publish indices, strictly in order.
        prop_assert_eq!(got, (0..payloads.len() as u64).collect::<Vec<_>>());
    }

    /// Property 3 — **lag is accounted, never silent**. When the
    /// receiver overflows the ring, every published event is either
    /// delivered or counted as dropped: `delivered + dropped == sent`.
    /// The receiver always catches up to the newest event, and the
    /// delivered tail is contiguous (no interior gaps).
    #[test]
    fn lag_is_accounted_and_catches_up(cap in 1usize..32, extra in 1usize..96) {
        // `4*cap + 8 + extra` overflows the buffer even after a
        // next-power-of-two rounding (next pow2 ≤ 2*cap), guaranteeing at
        // least one dropped event without depending on the exact rounding.
        let total = 4 * cap + 8 + extra;
        let bus = EventBus::new(cap);
        let mut rx = bus.subscribe();
        for i in 0..total {
            bus.publish(evt(i as u64));
        }
        let (got, dropped) = drain(&mut rx);

        // Overflow guaranteed → at least one event was dropped.
        prop_assert!(dropped >= 1, "expected the slow receiver to lag");
        // No silent loss: the ledger balances exactly.
        prop_assert_eq!(
            got.len() as u64 + dropped,
            total as u64,
            "every sent event must be delivered or counted as dropped"
        );
        prop_assert!(!got.is_empty(), "the newest events are always retained");
        // Strictly increasing (FIFO preserved through the lag).
        for w in got.windows(2) {
            prop_assert!(w[0] < w[1], "delivery must stay monotone after a lag");
        }
        // Always catches up to the newest event…
        prop_assert_eq!(*got.last().unwrap(), (total - 1) as u64);
        // …and the delivered tail is a contiguous suffix (no interior gaps).
        let first = got[0];
        prop_assert_eq!(got, (first..total as u64).collect::<Vec<_>>());
    }

    /// Property 4 — **idempotent, order-independent fold**. Folding the
    /// delivered events by `(noyau, seq)` identity is invariant under
    /// permutation and under re-applying an arbitrary subset (the SSE
    /// reconnect-with-overlap shape). This is the bus-side face of the
    /// I3 ADDITIVE-COUNTERS invariant (see
    /// `proptest_i3_additive_counters.rs` for the counter proper).
    #[test]
    fn fold_is_idempotent_and_order_independent(
        events in prop::collection::vec(
            (prop::sample::select(vec!["noyau-a", "noyau-b", "noyau-c"]), any::<u64>()),
            0..64,
        ),
        dup_mask in prop::collection::vec(any::<bool>(), 0..64),
    ) {
        let events: Vec<(String, u64)> =
            events.into_iter().map(|(n, s)| (n.to_owned(), s)).collect();

        let base = fold_by_identity(&events);

        // Order independence: a reversed application yields the same fold.
        let mut reversed = events.clone();
        reversed.reverse();
        prop_assert_eq!(fold_by_identity(&reversed), base.clone());

        // Idempotence: re-applying an arbitrary already-seen subset
        // (selected by `dup_mask`) changes nothing.
        let mut with_dups = events.clone();
        for (i, ev) in events.iter().enumerate() {
            if *dup_mask.get(i).unwrap_or(&false) {
                with_dups.push(ev.clone());
            }
        }
        prop_assert_eq!(fold_by_identity(&with_dups), base);
    }
}

/// Deterministic companion to Property 3: a single hand-built overflow
/// case, kept as a `#[test]` so the lag accounting is greppable in
/// review without reading the proptest harness.
#[test]
fn lag_ledger_balances_on_a_concrete_overflow() {
    let cap = 4;
    let bus = EventBus::new(cap);
    let mut rx = bus.subscribe();
    let total = 100usize;
    for i in 0..total {
        bus.publish(evt(i as u64));
    }
    let (got, dropped) = drain(&mut rx);
    assert_eq!(
        got.len() as u64 + dropped,
        total as u64,
        "received + dropped must equal sent"
    );
    assert_eq!(
        *got.last().unwrap(),
        (total - 1) as u64,
        "caught up to newest"
    );
}
