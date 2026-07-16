// SPDX-License-Identifier: AGPL-3.0-only

//! Property-based invariants for `cosmon-graph` per ADR-022.
//!
//! Covers the new DAG primitives introduced alongside the native DAG scheduler:
//! [`Plan`], [`prune_completed`], [`insert_subgraph`], and [`critical_path`].
//! Per CLAUDE.md "Testing Policy" (Stable tier), these invariant-heavy types
//! deserve property-based coverage in addition to the deterministic unit tests.
//!
//! Strategy: every generated edge set is expressed over integer nodes
//! `0..N` with edges pointing from low to high, which makes the base graph
//! acyclic by construction. Cycle-inducing inputs are generated separately
//! (arbitrary `(u, v)` pairs) so `insert_subgraph` can be exercised on both
//! paths.

#![allow(clippy::cast_possible_truncation)]

use std::collections::{HashMap, HashSet};

use cosmon_graph::{critical_path, insert_subgraph, prune_completed, toposort, Plan};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Maximum number of distinct nodes in any generated DAG. Kept small so
/// the reference algorithms (which are O(V·E) or O(V+E)) stay cheap and
/// shrinking remains fast.
const MAX_NODES: u32 = 8;

/// Maximum number of raw edges produced before dedup. The post-dedup
/// edge count is typically smaller.
const MAX_EDGES: usize = 16;

/// Produce a random acyclic edge list over nodes `0..MAX_NODES` by
/// constraining every edge to go from a lower-numbered node to a
/// higher-numbered one. Duplicates are removed and the result sorted
/// for determinism.
fn arb_dag() -> impl Strategy<Value = Vec<(u32, u32)>> {
    proptest::collection::vec((0u32..MAX_NODES, 0u32..MAX_NODES), 0..=MAX_EDGES).prop_map(|pairs| {
        let mut edges: Vec<(u32, u32)> = pairs
            .into_iter()
            .filter_map(|(a, b)| match a.cmp(&b) {
                std::cmp::Ordering::Less => Some((a, b)),
                std::cmp::Ordering::Greater => Some((b, a)),
                std::cmp::Ordering::Equal => None,
            })
            .collect();
        edges.sort_unstable();
        edges.dedup();
        edges
    })
}

/// Produce an *arbitrary* edge list — edges may point either direction,
/// so the resulting graph can contain cycles. Used to stress
/// [`insert_subgraph`]'s cycle-rejection branch.
fn arb_any_edges() -> impl Strategy<Value = Vec<(u32, u32)>> {
    proptest::collection::vec((0u32..MAX_NODES, 0u32..MAX_NODES), 0..=MAX_EDGES)
}

// ---------------------------------------------------------------------------
// Reference algorithms (independent of production code)
// ---------------------------------------------------------------------------

/// Fixed-point longest-path length (in nodes) on an acyclic graph.
///
/// Intentionally written without calling [`toposort`] so it is an
/// independent oracle for [`critical_path`]. Uses a Bellman-Ford-style
/// relaxation that converges in at most `|V|` passes over the edges on
/// a DAG.
fn reference_longest_path_len(edges: &[(u32, u32)]) -> usize {
    if edges.is_empty() {
        return 0;
    }
    let nodes: HashSet<u32> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
    let mut dp: HashMap<u32, usize> = nodes.iter().map(|&n| (n, 1usize)).collect();
    for _ in 0..nodes.len() {
        for (src, dst) in edges {
            let candidate = dp.get(src).copied().unwrap_or(1) + 1;
            let cur = dp.get(dst).copied().unwrap_or(1);
            if candidate > cur {
                dp.insert(*dst, candidate);
            }
        }
    }
    dp.values().copied().max().unwrap_or(0)
}

/// Return the sorted, deduplicated node universe of an edge list.
fn node_universe(edges: &[(u32, u32)]) -> Vec<u32> {
    let mut nodes: Vec<u32> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]

    /// P1 — A [`Plan`] built from any acyclic edge list drains under the
    /// obvious `mark_running` → `mark_done` loop over the ready frontier.
    /// If the plan ever has non-empty `running`/`done` sets but zero ready
    /// nodes and is not drained, the reducer is broken.
    #[test]
    fn prop_plan_drains(edges in arb_dag()) {
        let mut plan = Plan::new(edges.clone(), HashSet::new())
            .expect("low→high edges are always acyclic");

        let node_count = node_universe(&edges).len();
        // Generous bound: the plan cannot need more rounds than it has nodes.
        let max_rounds = node_count + 4;
        let mut rounds = 0usize;

        while !plan.is_drained() {
            prop_assert!(
                rounds < max_rounds,
                "plan did not drain in {max_rounds} rounds for edges {edges:?}"
            );
            let ready: Vec<u32> = plan.ready().iter().copied().collect();
            prop_assert!(
                !ready.is_empty(),
                "plan stuck: not drained but no ready nodes"
            );
            for id in &ready {
                plan.mark_running(id);
            }
            for id in &ready {
                plan.mark_done(id);
            }
            rounds += 1;
        }
        prop_assert!(plan.is_drained());
    }

    /// P2 — Every node surfaced by [`Plan::mark_done`] as newly ready has
    /// **all** of its direct dependencies already satisfied (i.e. in the
    /// caller's tracked `done` set, since no nodes are skipped here).
    /// This is the precedence invariant of the reducer.
    #[test]
    fn prop_mark_done_precedence(edges in arb_dag()) {
        let mut plan = Plan::new(edges.clone(), HashSet::new())
            .expect("acyclic by construction");
        let mut done: HashSet<u32> = HashSet::new();

        let node_count = node_universe(&edges).len();
        let max_rounds = node_count + 4;
        let mut rounds = 0usize;

        while !plan.is_drained() {
            prop_assert!(rounds < max_rounds);
            let ready: Vec<u32> = plan.ready().iter().copied().collect();
            for id in &ready {
                plan.mark_running(id);
            }
            for id in &ready {
                let newly = plan.mark_done(id);
                done.insert(*id);
                for n in &newly {
                    // Every dependency of `n` must already be in `done`.
                    for (d, s) in &edges {
                        if s == n {
                            prop_assert!(
                                done.contains(d),
                                "dep {d} of newly ready {n} not yet in done"
                            );
                        }
                    }
                }
            }
            rounds += 1;
        }
    }

    /// P3 — [`insert_subgraph`] either accepts the splice (in which case
    /// the result is acyclic and is exactly the sorted, deduplicated union
    /// of the two inputs) or rejects it (in which case the manual union
    /// actually contains a cycle). This covers the I6 invariant:
    /// mid-run mutations never silently introduce
    /// cycles, and acceptance implies the merged graph is a superset of
    /// both inputs — so any `done` set valid under the original edges
    /// (dependencies all in `done`) remains valid under the merged edges.
    #[test]
    fn prop_insert_subgraph_consistency(
        edges in arb_dag(),
        new_edges in arb_any_edges(),
    ) {
        // Build the manual union for comparison.
        let mut union: Vec<(u32, u32)> = edges.clone();
        for e in &new_edges {
            if !union.contains(e) {
                union.push(*e);
            }
        }

        match insert_subgraph(&edges, &new_edges) {
            Ok(merged) => {
                // Acyclicity of the merged result.
                prop_assert!(
                    toposort(&merged).is_ok(),
                    "insert_subgraph accepted but result has cycle"
                );
                // Merged is exactly the sorted deduped union.
                let mut expected = union.clone();
                expected.sort_unstable();
                expected.dedup();
                prop_assert_eq!(merged.clone(), expected);
                // Original edges are all preserved (done-set preservation).
                for e in &edges {
                    prop_assert!(merged.contains(e));
                }
            }
            Err(_) => {
                // Rejection is only legal when the union actually has a cycle.
                prop_assert!(
                    toposort(&union).is_err(),
                    "insert_subgraph rejected an acyclic union"
                );
            }
        }
    }

    /// P4 — [`prune_completed`] is idempotent. Pruning twice with the same
    /// `completed` set returns the same edge list as pruning once, and a
    /// second prune removes no further nodes (because every surviving
    /// edge has a source outside `completed`).
    #[test]
    fn prop_prune_completed_idempotent(
        edges in arb_dag(),
        completed_mask in proptest::collection::vec(any::<bool>(), 0..=(MAX_NODES as usize)),
    ) {
        let universe = node_universe(&edges);
        let completed: HashSet<u32> = universe
            .iter()
            .enumerate()
            .filter(|(i, _)| completed_mask.get(*i).copied().unwrap_or(false))
            .map(|(_, n)| *n)
            .collect();

        let (pruned1, _removed1) = prune_completed(&edges, &completed);
        let (pruned2, removed2) = prune_completed(&pruned1, &completed);

        prop_assert_eq!(pruned1, pruned2);
        // Second prune can surface no new removed nodes: any node whose only
        // anchoring edges had sources in `completed` was already surfaced on
        // the first call and is absent from `pruned1`'s node set.
        prop_assert!(removed2.is_empty());
    }

    /// P5 — [`critical_path`] returns a path whose length (in nodes) equals
    /// the value computed by an independent Bellman-Ford-style reference DP.
    /// Also sanity-checks that every consecutive pair in the returned path
    /// corresponds to an edge in the original input.
    #[test]
    fn prop_critical_path_matches_reference(edges in arb_dag()) {
        let path = critical_path(&edges).expect("acyclic by construction");
        let expected_len = reference_longest_path_len(&edges);
        prop_assert_eq!(path.len(), expected_len);

        // Every consecutive pair must be a real edge.
        for window in path.windows(2) {
            let pair = (window[0], window[1]);
            prop_assert!(
                edges.contains(&pair),
                "reconstructed path has non-edge {pair:?}"
            );
        }
    }
}
