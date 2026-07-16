// SPDX-License-Identifier: AGPL-3.0-only

//! `Plan<Id>`: pure reducer for DAG execution (ADR-022 Native DAG Scheduler).
//!
//! # Why a pure reducer?
//!
//! The Resident Runtime owns the event loop: it polls workers, listens to
//! transport events, and decides when to launch work. The [`Plan`] is a
//! *pure reducer* — no I/O, no `tokio`, no background tasks. The runtime
//! calls methods on [`Plan`] to advance state and receives newly-ready
//! nodes as return values. This keeps the scheduler testable in isolation,
//! deterministic, and composable with any external clock.
//!
//! # Loop invariant
//!
//! At all times, the sets (`ready`, `running`, `done`, `skipped`) are
//! pairwise disjoint: no id can appear in two sets simultaneously.
//! A node is eligible for `ready` iff all of its dependencies are in
//! `done ∪ skipped`, and it is not itself in any of the four sets.
//! `skipped` is frozen at construction; nodes never enter `skipped`
//! after [`Plan::new`] returns.
//!
//! # Determinism
//!
//! The ready frontier is a [`BTreeSet`], so iteration order is the
//! `Ord` ordering of `Id`. [`Plan::mark_done`] returns newly-ready nodes
//! in the same sorted order. Two runs of the same sequence of calls on
//! the same inputs produce identical observable state.

use crate::{toposort, CycleError};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::Hash;

/// A pure reducer that advances a DAG execution plan one transition at a time.
///
/// Generic over the node id type `Id`. The only bounds are the minimum needed
/// to hash nodes (`Eq + Hash`), sort them deterministically (`Ord`), and copy
/// them across internal collections (`Clone`). Notably, there is no
/// `Display`, `Send`, or `Sync` bound — `Plan` is a pure data structure and
/// the runtime decides how to carry it across threads.
///
/// # Lifecycle
///
/// Each node travels through states: initially absent → `ready` → `running`
/// → `done`. Nodes in the `skipped` set bypass the lifecycle entirely and
/// act as satisfied dependencies for downstream nodes.
///
/// ```
/// use cosmon_graph::Plan;
/// use std::collections::HashSet;
///
/// //     a
/// //    / \
/// //   b   c
/// //    \ /
/// //     d
/// let mut plan = Plan::new(
///     vec![("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")],
///     HashSet::new(),
/// )
/// .unwrap();
///
/// // Only "a" is ready at start.
/// assert_eq!(plan.ready().iter().copied().collect::<Vec<_>>(), vec!["a"]);
///
/// // Complete "a" → both "b" and "c" become ready.
/// plan.mark_running(&"a");
/// assert_eq!(plan.mark_done(&"a"), vec!["b", "c"]);
/// ```
#[derive(Debug, Clone)]
pub struct Plan<Id> {
    /// Dependency edges `(from, to)` meaning "`from` must complete before `to`".
    ///
    /// Stored as a `Vec` rather than a richer adjacency structure because
    /// (a) plans are typically small and linear-scanned once per transition,
    /// and (b) the flat representation is trivial to serialize if a future
    /// variant wants to snapshot the reducer.
    edges: Vec<(Id, Id)>,
    /// Nodes whose dependencies are all satisfied and which are waiting to
    /// be picked up. Sorted for deterministic iteration.
    ready: BTreeSet<Id>,
    /// Nodes currently in flight — handed out by the scheduler but not yet done.
    running: HashSet<Id>,
    /// Nodes that have finished successfully.
    done: HashSet<Id>,
    /// Nodes the caller asked to treat as already-satisfied (e.g., skipped by
    /// `--from`, `--only`, or manual override). They behave like "done" for
    /// the purpose of unlocking dependents but never enter `ready`/`running`.
    skipped: HashSet<Id>,
}

impl<Id> Plan<Id>
where
    Id: Eq + Hash + Ord + Clone,
{
    /// Construct a new [`Plan`] from dependency edges and a skip set.
    ///
    /// # Initial state
    ///
    /// Every node whose dependencies are all in `skip` (including roots with
    /// no dependencies) enters `ready` immediately, unless it is itself in
    /// `skip`. No node is `running` or `done` at construction.
    ///
    /// # Errors
    ///
    /// Returns [`CycleError`] if the graph contains a cycle. A cyclic plan
    /// cannot be scheduled regardless of the skip set, so we validate
    /// acyclicity via [`toposort`] before touching the skip logic.
    pub fn new(edges: Vec<(Id, Id)>, skip: HashSet<Id>) -> Result<Self, CycleError<Id>> {
        Self::new_with_roots(edges, skip, HashSet::new())
    }

    /// Like [`Self::new`] but also registers standalone root nodes that have
    /// no edges. Without this, a single-node plan (0 edges) would have 0
    /// nodes and `ready()` would return empty — the scheduler would exit
    /// immediately even though work exists.
    ///
    /// # Errors
    ///
    /// Returns [`CycleError`] if the edge list contains a cycle.
    pub fn new_with_roots(
        edges: Vec<(Id, Id)>,
        skip: HashSet<Id>,
        roots: HashSet<Id>,
    ) -> Result<Self, CycleError<Id>> {
        toposort(&edges)?;

        let mut nodes: HashSet<Id> = HashSet::new();
        // Register standalone roots first so they appear in the node universe
        // even if they have no edges.
        for r in roots {
            nodes.insert(r);
        }
        for (a, b) in &edges {
            nodes.insert(a.clone());
            nodes.insert(b.clone());
        }

        // Index dependencies for O(|edges|) initial-ready computation.
        let mut deps_of: HashMap<&Id, Vec<&Id>> = HashMap::new();
        for (dep, step) in &edges {
            deps_of.entry(step).or_default().push(dep);
        }

        let mut ready: BTreeSet<Id> = BTreeSet::new();
        for node in &nodes {
            if skip.contains(node) {
                continue;
            }
            // Roots (no deps_of entry) satisfy the predicate trivially.
            let satisfied = deps_of
                .get(node)
                .is_none_or(|deps| deps.iter().all(|d| skip.contains(*d)));
            if satisfied {
                ready.insert(node.clone());
            }
        }

        Ok(Self {
            edges,
            ready,
            running: HashSet::new(),
            done: HashSet::new(),
            skipped: skip,
        })
    }

    /// Return the currently-ready frontier in deterministic sorted order.
    ///
    /// Exposed as a reference to the underlying [`BTreeSet`] so callers can
    /// iterate, check membership, or count without cloning.
    #[must_use]
    pub fn ready(&self) -> &BTreeSet<Id> {
        &self.ready
    }

    /// Mark a node as running — move it from `ready` to `running`.
    ///
    /// No-op if `id` is not currently in `ready` (e.g., already running,
    /// already done, skipped, or unknown). This keeps the method safe to
    /// call defensively from the runtime after a race between pickup and
    /// an earlier transition.
    pub fn mark_running(&mut self, id: &Id) {
        if self.ready.remove(id) {
            self.running.insert(id.clone());
        }
    }

    /// Return a previously-dispatched node to the ready frontier.
    ///
    /// Moves `id` from `running` back to `ready`. No-op if `id` is not
    /// currently in `running` (already done, skipped, still ready, or
    /// unknown). The runtime calls this when a dispatch failed and the
    /// molecule was rolled back to `Pending` in the store (or the liveness
    /// recheck reset an orphan), so the policy must re-surface it for retry
    /// instead of leaking it in `running` forever. The node was ready before
    /// it was marked running, so its dependencies are still satisfied — the
    /// move is unconditionally safe.
    pub fn mark_ready(&mut self, id: &Id) {
        if self.running.remove(id) {
            self.ready.insert(id.clone());
        }
    }

    /// Mark a node as done and return the list of newly-ready nodes.
    ///
    /// Idempotent: calling twice with the same id returns an empty `Vec`
    /// on subsequent calls and does not perturb state. The returned list
    /// is sorted by `Ord` for determinism.
    ///
    /// A dependent becomes newly ready when *all* of its dependencies are
    /// now in `done ∪ skipped` and it is not already tracked in another
    /// set. Skipped dependents are never surfaced — they were factored in
    /// at construction time.
    pub fn mark_done(&mut self, id: &Id) -> Vec<Id> {
        // Idempotency and unknown-id safety: if the id is already terminal
        // (done or skipped), do nothing and return an empty list.
        if self.done.contains(id) || self.skipped.contains(id) {
            return Vec::new();
        }

        // A node can be completed either from `running` (normal path) or
        // directly from `ready` (degenerate path where the runtime elides
        // the running state for instantaneous work).
        self.running.remove(id);
        self.ready.remove(id);
        self.done.insert(id.clone());

        // Gather unique direct dependents of `id`. `BTreeSet` both dedups
        // parallel edges (e.g., `(a, b)` listed twice) and gives us sorted
        // iteration for the return value.
        let candidates: BTreeSet<Id> = self
            .edges
            .iter()
            .filter(|(dep, _)| dep == id)
            .map(|(_, step)| step.clone())
            .collect();

        let mut newly_ready: Vec<Id> = Vec::new();
        for step in candidates {
            // Only consider dependents that haven't been touched yet.
            if self.done.contains(&step)
                || self.running.contains(&step)
                || self.skipped.contains(&step)
                || self.ready.contains(&step)
            {
                continue;
            }
            // A dependent becomes ready iff *every* one of its dependencies
            // is now in `done ∪ skipped`.
            let all_deps_satisfied = self
                .edges
                .iter()
                .filter(|(_, s)| s == &step)
                .all(|(d, _)| self.done.contains(d) || self.skipped.contains(d));
            if all_deps_satisfied {
                newly_ready.push(step);
            }
        }
        // `candidates` was already sorted (BTreeSet), and we pushed in order,
        // so `newly_ready` is already sorted. The explicit sort is a cheap
        // guard against future refactors changing the iteration order.
        newly_ready.sort();
        for n in &newly_ready {
            self.ready.insert(n.clone());
        }
        newly_ready
    }

    /// Return `true` when no nodes are `ready` and none are `running`.
    ///
    /// A drained plan may still have `done` and `skipped` nodes; it just
    /// has no pending or in-flight work. The runtime uses this as the
    /// termination condition for its event loop.
    #[must_use]
    pub fn is_drained(&self) -> bool {
        self.ready.is_empty() && self.running.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_vec(plan: &Plan<&'static str>) -> Vec<&'static str> {
        plan.ready().iter().copied().collect()
    }

    #[test]
    fn test_plan_empty_graph_is_drained() {
        let plan: Plan<&str> = Plan::new(vec![], HashSet::new()).unwrap();
        assert!(plan.ready().is_empty());
        assert!(plan.is_drained());
    }

    #[test]
    fn test_plan_single_effective_node() {
        // Two-node chain is the smallest plan expressible through edges.
        // Exercises the complete lifecycle of one "effective" node.
        let mut plan = Plan::new(vec![("a", "b")], HashSet::new()).unwrap();
        assert_eq!(ready_vec(&plan), vec!["a"]);
        assert!(!plan.is_drained());

        plan.mark_running(&"a");
        assert!(plan.ready().is_empty());
        assert!(!plan.is_drained());

        let newly = plan.mark_done(&"a");
        assert_eq!(newly, vec!["b"]);
        assert_eq!(ready_vec(&plan), vec!["b"]);

        plan.mark_running(&"b");
        let newly = plan.mark_done(&"b");
        assert!(newly.is_empty());
        assert!(plan.is_drained());
    }

    #[test]
    fn test_plan_linear_chain() {
        let mut plan = Plan::new(vec![("a", "b"), ("b", "c"), ("c", "d")], HashSet::new()).unwrap();
        assert_eq!(ready_vec(&plan), vec!["a"]);

        plan.mark_running(&"a");
        assert_eq!(plan.mark_done(&"a"), vec!["b"]);

        plan.mark_running(&"b");
        assert_eq!(plan.mark_done(&"b"), vec!["c"]);

        plan.mark_running(&"c");
        assert_eq!(plan.mark_done(&"c"), vec!["d"]);

        plan.mark_running(&"d");
        assert!(plan.mark_done(&"d").is_empty());
        assert!(plan.is_drained());
    }

    #[test]
    fn test_plan_diamond_dag() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d
        let mut plan = Plan::new(
            vec![("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")],
            HashSet::new(),
        )
        .unwrap();
        assert_eq!(ready_vec(&plan), vec!["a"]);

        plan.mark_running(&"a");
        // Completing `a` unlocks both `b` and `c`, sorted.
        assert_eq!(plan.mark_done(&"a"), vec!["b", "c"]);
        assert_eq!(ready_vec(&plan), vec!["b", "c"]);

        plan.mark_running(&"b");
        // `d` still waits for `c`.
        assert!(plan.mark_done(&"b").is_empty());

        plan.mark_running(&"c");
        // Now both parents of `d` are done.
        assert_eq!(plan.mark_done(&"c"), vec!["d"]);

        plan.mark_running(&"d");
        assert!(plan.mark_done(&"d").is_empty());
        assert!(plan.is_drained());
    }

    #[test]
    fn test_plan_rejects_two_node_cycle() {
        let err = Plan::new(vec![("a", "b"), ("b", "a")], HashSet::new()).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    #[test]
    fn test_plan_rejects_three_node_cycle() {
        let err = Plan::new(vec![("a", "b"), ("b", "c"), ("c", "a")], HashSet::new()).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    #[test]
    fn test_plan_mark_done_is_idempotent() {
        let mut plan = Plan::new(vec![("a", "b")], HashSet::new()).unwrap();
        plan.mark_running(&"a");
        let newly1 = plan.mark_done(&"a");
        assert_eq!(newly1, vec!["b"]);

        // Second call must be a no-op.
        let newly2 = plan.mark_done(&"a");
        assert!(newly2.is_empty());

        // State unchanged after the redundant call.
        assert_eq!(ready_vec(&plan), vec!["b"]);
        assert!(!plan.is_drained());
    }

    #[test]
    fn test_plan_mark_done_idempotent_does_not_double_surface_dependents() {
        // Diamond: ensure re-completing `a` does not re-insert `b`/`c`
        // into the newly-ready list on the second call.
        let mut plan = Plan::new(
            vec![("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")],
            HashSet::new(),
        )
        .unwrap();
        plan.mark_running(&"a");
        assert_eq!(plan.mark_done(&"a"), vec!["b", "c"]);
        // Second call: nothing new.
        assert!(plan.mark_done(&"a").is_empty());
        assert_eq!(ready_vec(&plan), vec!["b", "c"]);
    }

    #[test]
    fn test_plan_skip_set_unlocks_roots() {
        // A → B → C, skip B: on construction both A (no deps) and C
        // (only dep B is in skip) become ready; B never enters the plan.
        let skip: HashSet<&str> = ["b"].into_iter().collect();
        let mut plan = Plan::new(vec![("a", "b"), ("b", "c")], skip).unwrap();
        assert_eq!(ready_vec(&plan), vec!["a", "c"]);

        plan.mark_running(&"a");
        // A's only dependent is B, which is skipped: nothing newly surfaces.
        let newly = plan.mark_done(&"a");
        assert!(newly.is_empty());
        assert!(!plan.ready().contains(&"b"));

        plan.mark_running(&"c");
        assert!(plan.mark_done(&"c").is_empty());
        assert!(plan.is_drained());
    }

    #[test]
    fn test_plan_skip_set_respects_partial_skip() {
        // A → C, B → C, skip A. Initially B is ready (no deps) and C still
        // waits on B (A's satisfaction comes from being skipped).
        let skip: HashSet<&str> = ["a"].into_iter().collect();
        let mut plan = Plan::new(vec![("a", "c"), ("b", "c")], skip).unwrap();
        assert_eq!(ready_vec(&plan), vec!["b"]);

        plan.mark_running(&"b");
        // Completing B should unlock C because A is treated as satisfied.
        assert_eq!(plan.mark_done(&"b"), vec!["c"]);
        plan.mark_running(&"c");
        plan.mark_done(&"c");
        assert!(plan.is_drained());
    }

    #[test]
    fn test_plan_mark_running_is_defensive() {
        // mark_running on a node that isn't ready is a no-op.
        let mut plan = Plan::new(vec![("a", "b")], HashSet::new()).unwrap();
        plan.mark_running(&"b"); // b isn't ready yet
        assert_eq!(ready_vec(&plan), vec!["a"]);
        assert!(!plan.is_drained());
    }
}
