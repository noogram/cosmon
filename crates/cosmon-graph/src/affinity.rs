// SPDX-License-Identifier: AGPL-3.0-only

//! Model-affinity ordering of the ready frontier.
//!
//! # Why this module exists
//!
//! On a single-GPU local oracle (the LPTHE `ollama-g5` case, C3 of
//! `delib-20260705-7288`), the box holds **exactly one** 120 B model
//! resident in VRAM (48 GB ≈ one 120 B; a second model forces a ~40 GB
//! swap through disk). Ollama keeps the last-used model loaded for
//! `keep_alive` (5 min by default), so two consecutive dispatches that
//! ask for the *same* model pay the load cost once, whereas an
//! alternating frontier (`A, B, A, B, …`) reloads on **every** turn and
//! the local path dies on latency long before it dies on quality.
//!
//! [`ready_frontier`](crate::ready_frontier) returns molecules sorted by
//! `Ord` for determinism — a *scheduling-blind* order. This module adds
//! a second, orthogonal ordering pass: given each molecule's bound model
//! (its `Incarnation` model slot — ADR-142 — chosen once at spawn and
//! never per step), it clusters same-model molecules contiguously and
//! drains the **currently-resident** model first. The DAG semantics are
//! untouched: the output is always a permutation of the input frontier,
//! so every molecule the frontier declared ready is still dispatched —
//! only the *order within a ready batch* changes.
//!
//! This is the scheduler half of "model-affinity batching (`keep_alive`)".
//! The provider half (holding the model warm across the batch) lives in
//! `cosmon_provider::OllamaProvider::with_keep_alive`. Neither depends
//! on the other: affinity ordering shrinks the number of *distinct*
//! adjacent models; `keep_alive` widens the window in which a resident
//! model survives to the next same-model dispatch.
//!
//! `Incarnation` is defined in `docs/adr/142-incarnation-launch-time-decision.md`.

use std::collections::BTreeMap;
use std::hash::Hash;

/// Re-order a ready frontier so molecules bound to the same model are
/// contiguous, minimizing model swaps on a single-resident-model oracle.
///
/// `frontier` is the ready set (typically the output of
/// [`ready_frontier`](crate::ready_frontier), already deterministically
/// sorted). `model_of` maps a molecule to its bound model key, or `None`
/// when the molecule imposes no model preference (a remote adapter, or a
/// molecule with no `Incarnation` model pin). `resident`
/// is the model currently loaded in the oracle's VRAM, if known.
///
/// # Ordering contract
///
/// 1. The result is a **permutation** of `frontier` — same elements,
///    same multiplicity. No molecule is dropped or duplicated. (This is
///    the load-bearing invariant: affinity is a *hint*, never a filter.)
/// 2. Molecules sharing a model key are **contiguous**.
/// 3. The `resident` model's bucket, if present in the frontier, comes
///    **first** — it needs no reload.
/// 4. Remaining model buckets follow in ascending `Ord` of the key, for
///    determinism (a stable schedule is a debuggable schedule).
/// 5. Molecules with `None` model come **last**, in their original
///    relative order: they run on whatever model is resident, so they
///    never *cause* a swap and are cheapest to defer.
/// 6. Within every bucket, the input's relative order is preserved
///    (stable partition).
///
/// The number of model switches in the returned order is provably
/// minimal for a single-resident-model machine: starting cold
/// (`resident` absent or not in the frontier) it equals the number of
/// distinct models present — each must be loaded at least once; starting
/// warm (`resident` is one of the frontier's models) it is one fewer,
/// because the resident bucket drains first with no reload. No ordering
/// can do better. [`model_switch_count`] measures it.
///
/// # Examples
///
/// ```
/// use cosmon_graph::affinity_order;
///
/// // Frontier alternates two models; affinity clusters them.
/// let frontier = ["m1", "m2", "m3", "m4"];
/// let model_of = |n: &&str| match *n {
///     "m1" | "m3" => Some("gpt-oss:120b"),
///     _ => Some("qwen3.5:122b"),
/// };
/// let order = affinity_order(&frontier, model_of, None);
/// // gpt-oss bucket (Ord-first) then qwen bucket — one switch, not three.
/// assert_eq!(order, vec!["m1", "m3", "m2", "m4"]);
/// ```
pub fn affinity_order<N, M>(
    frontier: &[N],
    model_of: impl Fn(&N) -> Option<M>,
    resident: Option<&M>,
) -> Vec<N>
where
    N: Clone,
    M: Eq + Hash + Ord + Clone,
{
    // Stable partition into per-model buckets. BTreeMap gives ascending
    // Ord iteration for free (contract rule 4); insertion order within a
    // Vec bucket preserves the input's relative order (contract rule 6).
    let mut named: BTreeMap<M, Vec<N>> = BTreeMap::new();
    let mut unbound: Vec<N> = Vec::new();

    for node in frontier {
        match model_of(node) {
            Some(model) => named.entry(model).or_default().push(node.clone()),
            None => unbound.push(node.clone()),
        }
    }

    let mut ordered: Vec<N> = Vec::with_capacity(frontier.len());

    // Rule 3: the resident model's bucket drains first (no reload).
    if let Some(res) = resident {
        if let Some(bucket) = named.remove(res) {
            ordered.extend(bucket);
        }
    }

    // Rule 4: remaining named buckets in ascending Ord of the key.
    for (_model, bucket) in named {
        ordered.extend(bucket);
    }

    // Rule 5: model-agnostic molecules last.
    ordered.extend(unbound);

    ordered
}

/// Count how many times the resident model must change to dispatch
/// `order` in sequence, starting from `resident`.
///
/// A "switch" is an adjacent pair whose model keys differ; a `None`
/// molecule inherits whatever model is loaded and never counts as a
/// switch. The starting `resident` seeds the count: the first named
/// molecule switches iff its model differs from `resident` (or `resident`
/// is `None`/unknown). This is the executable spec for
/// [`affinity_order`]'s optimality — see the module tests.
///
/// # Examples
///
/// ```
/// use cosmon_graph::model_switch_count;
///
/// let order = ["a", "b", "c"];
/// let model_of = |n: &&str| match *n {
///     "a" | "b" => Some("x"),
///     _ => Some("y"),
/// };
/// // x, x, y → one switch (x→y). Warm-started on "x".
/// assert_eq!(model_switch_count(&order, model_of, Some(&"x")), 1);
/// ```
pub fn model_switch_count<N, M>(
    order: &[N],
    model_of: impl Fn(&N) -> Option<M>,
    resident: Option<&M>,
) -> usize
where
    M: Eq + Clone,
{
    let mut current: Option<M> = resident.cloned();
    let mut switches = 0usize;
    for node in order {
        if let Some(model) = model_of(node) {
            if current.as_ref() != Some(&model) {
                switches += 1;
                current = Some(model);
            }
        }
        // A `None` molecule leaves `current` untouched (rule 5 rationale).
    }
    switches
}

#[cfg(test)]
mod tests {
    use super::*;

    // Model keys as &str for terse tests.
    fn two_model_map(n: &&str) -> Option<&'static str> {
        match *n {
            "m1" | "m3" => Some("gpt-oss:120b"),
            "m2" | "m4" => Some("qwen3.5:122b"),
            _ => None,
        }
    }

    #[test]
    fn affinity_order_is_a_permutation() {
        let frontier = ["m1", "m2", "m3", "m4"];
        let mut got = affinity_order(&frontier, two_model_map, None);
        let mut expect = frontier.to_vec();
        got.sort_unstable();
        expect.sort_unstable();
        assert_eq!(got, expect, "output must be a permutation of the input");
    }

    #[test]
    fn affinity_order_clusters_same_model_contiguously() {
        let frontier = ["m1", "m2", "m3", "m4"];
        let order = affinity_order(&frontier, two_model_map, None);
        // gpt-oss sorts before qwen (Ord), so its bucket comes first.
        assert_eq!(order, vec!["m1", "m3", "m2", "m4"]);
    }

    #[test]
    fn affinity_order_drains_resident_model_first() {
        let frontier = ["m1", "m2", "m3", "m4"];
        // qwen is resident → its bucket must come first despite Ord.
        let order = affinity_order(&frontier, two_model_map, Some(&"qwen3.5:122b"));
        assert_eq!(order, vec!["m2", "m4", "m1", "m3"]);
    }

    #[test]
    fn affinity_order_puts_unbound_last() {
        let frontier = ["free", "m1", "m2"];
        let order = affinity_order(&frontier, two_model_map, None);
        assert_eq!(order.last(), Some(&"free"), "None-model molecule runs last");
    }

    #[test]
    fn affinity_order_preserves_relative_order_within_bucket() {
        // Three molecules on the same model, given out of natural order.
        let frontier = ["z", "a", "k"];
        let same = |_: &&str| Some("solo");
        let order = affinity_order(&frontier, same, None);
        assert_eq!(order, vec!["z", "a", "k"], "stable within a bucket");
    }

    #[test]
    fn affinity_order_empty_frontier() {
        let frontier: [&str; 0] = [];
        let order = affinity_order(&frontier, two_model_map, None);
        assert!(order.is_empty());
    }

    #[test]
    fn affinity_order_minimizes_switches_vs_naive() {
        let frontier = ["m1", "m2", "m3", "m4"];
        // Naive Ord order alternates models: m1(gpt) m2(qwen) m3(gpt) m4(qwen)
        // → four cold loads. affinity clusters to gpt,gpt,qwen,qwen → two.
        let naive_switches = model_switch_count(&frontier, two_model_map, None);
        let order = affinity_order(&frontier, two_model_map, None);
        let affinity_switches = model_switch_count(&order, two_model_map, None);
        assert_eq!(naive_switches, 4, "alternating frontier reloads every turn");
        assert_eq!(
            affinity_switches, 2,
            "clustered frontier: one load per model"
        );
        assert!(affinity_switches < naive_switches);
    }

    #[test]
    fn switch_count_cold_equals_distinct_models() {
        // With N distinct models and a cold start, the floor is N switches
        // (each model loads once). affinity_order must hit that floor.
        let frontier = ["a", "b", "c", "d", "e"];
        let model_of = |n: &&str| match *n {
            "a" | "d" => Some("x"),
            "b" | "e" => Some("y"),
            _ => Some("z"),
        };
        let order = affinity_order(&frontier, model_of, None);
        let distinct = 3;
        assert_eq!(
            model_switch_count(&order, model_of, None),
            distinct,
            "cold start: one load per distinct model, no more"
        );
    }

    #[test]
    fn switch_count_warm_start_saves_one_load() {
        let frontier = ["a", "b", "c", "d", "e"];
        let model_of = |n: &&str| match *n {
            "a" | "d" => Some("x"),
            "b" | "e" => Some("y"),
            _ => Some("z"),
        };
        // Warm on "x": affinity drains x first, so only y and z reload.
        let order = affinity_order(&frontier, model_of, Some(&"x"));
        assert_eq!(
            model_switch_count(&order, model_of, Some(&"x")),
            2,
            "warm resident model saves its own load"
        );
    }

    #[test]
    fn unbound_molecules_never_count_as_switches() {
        let order = ["a", "free", "b"];
        let model_of = |n: &&str| match *n {
            "a" | "b" => Some("x"),
            _ => None,
        };
        // a(x) free(inherits x) b(x) → zero switches warm on x.
        assert_eq!(model_switch_count(&order, model_of, Some(&"x")), 0);
    }
}
