// SPDX-License-Identifier: AGPL-3.0-only

//! Longest-path DP on a DAG — "critical path" analysis.
//!
//! Given dependency edges `(dep, dependent)`, computes the longest chain of
//! nodes through the DAG. Used by the native DAG scheduler introduced in
//! [ADR-022](../../../docs/adr/022-native-dag-scheduler.md) to prioritize
//! work on the critical path of a plan: the longer the chain a task sits on,
//! the higher its latency impact, so it should be dispatched first.
//!
//! The algorithm is a textbook forward dynamic program over the topological
//! order: for each node, the best score is one plus the best score among its
//! direct predecessors (with an optional per-node weight). Reconstruction
//! follows parent pointers from the node with maximum score.

use std::collections::HashMap;
use std::hash::Hash;

use crate::{toposort, CycleError};

/// Compute one longest-length path through the DAG.
///
/// Returns a sequence of nodes of maximum length (counting one unit per node),
/// where each consecutive pair in the output is connected by a dependency
/// edge from the input. Ties are broken deterministically by `Ord`: among
/// candidates of equal score the one smallest by `Ord` is preferred, both
/// when choosing the sink and when choosing a parent during reconstruction.
/// This matches the determinism convention used by [`toposort`].
///
/// A DAG with no edges returns an empty vector. Isolated nodes (nodes that
/// do not appear in any edge) are invisible to this routine — callers that
/// need to include them should pre-pad the edge list with trivial edges
/// before calling.
///
/// # Errors
///
/// Returns [`CycleError`] if the graph contains a cycle.
///
/// # Examples
///
/// ```
/// use cosmon_graph::critical_path;
///
/// // A diamond: both `a → b → d` and `a → c → d` have length 3.
/// // Ord tie-break prefers the path through `b`.
/// let edges = [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")];
/// let path = critical_path(&edges).unwrap();
/// assert_eq!(path, vec!["a", "b", "d"]);
/// ```
pub fn critical_path<N>(edges: &[(N, N)]) -> Result<Vec<N>, CycleError<N>>
where
    N: Eq + Hash + Ord + Clone,
{
    critical_path_weighted(edges, |_| 1)
}

/// Weighted variant of [`critical_path`].
///
/// Each node contributes `weight(node)` to the total score of any path that
/// passes through it. Returns the path that maximizes the summed weight;
/// ties are broken by `Ord` exactly as in [`critical_path`].
///
/// # Errors
///
/// Returns [`CycleError`] if the graph contains a cycle.
pub fn critical_path_weighted<N, F>(edges: &[(N, N)], weight: F) -> Result<Vec<N>, CycleError<N>>
where
    N: Eq + Hash + Ord + Clone,
    F: Fn(&N) -> u64,
{
    let order = toposort(edges)?;
    if order.is_empty() {
        return Ok(Vec::new());
    }

    // Build predecessor map: step → its direct dependencies.
    let mut preds_of: HashMap<&N, Vec<&N>> = HashMap::new();
    for (dep, step) in edges {
        preds_of.entry(step).or_default().push(dep);
    }

    // DP tables. Keys borrow from `order`, which owns all nodes and lives
    // until the end of this function.
    let mut dp: HashMap<&N, u64> = HashMap::with_capacity(order.len());
    let mut parent: HashMap<&N, Option<&N>> = HashMap::with_capacity(order.len());

    // Loop invariant: after processing the prefix `[t_0, …, t_i]` of `order`,
    // for every node `v` in that prefix `dp[v]` equals the weight of the
    // longest (heaviest) path that ends at `v` and lies entirely inside the
    // prefix. Because `toposort` emits every predecessor of `v` strictly
    // before `v`, the invariant holds for each new node we visit: all of
    // `v`'s predecessors have final `dp` values by the time we compute
    // `dp[v] = weight(v) + max(dp[u] for u in preds(v))`.
    for node in &order {
        let w = weight(node);
        let mut best_score: u64 = w;
        let mut best_parent: Option<&N> = None;

        if let Some(preds) = preds_of.get(node) {
            for &pred in preds {
                let candidate = dp.get(pred).copied().unwrap_or(0) + w;
                if candidate > best_score {
                    best_score = candidate;
                    best_parent = Some(pred);
                } else if candidate == best_score && best_parent.is_some_and(|cur| pred < cur) {
                    // Tie on score: prefer the parent that is smaller by
                    // `Ord` for deterministic reconstruction.
                    best_parent = Some(pred);
                }
            }
        }

        dp.insert(node, best_score);
        parent.insert(node, best_parent);
    }

    // Select the sink: the node with maximum `dp`; on tie, smallest by `Ord`.
    // `max_by` returns the last maximum, so we make the comparator treat
    // "smaller Ord" as "greater" (better) on score ties.
    let Some(sink) = order.iter().max_by(|a, b| {
        dp.get(a)
            .copied()
            .unwrap_or(0)
            .cmp(&dp.get(b).copied().unwrap_or(0))
            .then_with(|| b.cmp(a))
    }) else {
        return Ok(Vec::new());
    };

    // Reconstruct by following parent pointers back to a source.
    let mut rev_path: Vec<N> = Vec::new();
    let mut cursor: Option<&N> = Some(sink);
    while let Some(node) = cursor {
        rev_path.push(node.clone());
        cursor = parent.get(node).copied().flatten();
    }
    rev_path.reverse();
    Ok(rev_path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_critical_path_empty_graph() {
        let edges: &[(&str, &str)] = &[];
        let path = critical_path(edges).unwrap();
        assert!(path.is_empty());
    }

    #[test]
    fn test_critical_path_single_edge() {
        // Smallest non-trivial graph: one edge, two nodes.
        let edges = [("a", "b")];
        let path = critical_path(&edges).unwrap();
        assert_eq!(path, vec!["a", "b"]);
    }

    #[test]
    fn test_critical_path_linear_chain() {
        let edges = [("a", "b"), ("b", "c"), ("c", "d")];
        let path = critical_path(&edges).unwrap();
        assert_eq!(path, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_critical_path_diamond_tie_broken_by_ord() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d
        //
        // Both `a → b → d` and `a → c → d` have length 3. Tie-break picks
        // the path through `b` because "b" < "c".
        let edges = [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")];
        let path = critical_path(&edges).unwrap();
        assert_eq!(path, vec!["a", "b", "d"]);
    }

    #[test]
    fn test_critical_path_diamond_tie_independent_of_edge_order() {
        // Same diamond but edges enumerated in the opposite order.
        // Determinism must not depend on edge insertion order.
        let edges = [("c", "d"), ("b", "d"), ("a", "c"), ("a", "b")];
        let path = critical_path(&edges).unwrap();
        assert_eq!(path, vec!["a", "b", "d"]);
    }

    #[test]
    fn test_critical_path_unequal_branches_picks_longer_arm() {
        //   a ── b ── c ── d       (arm of length 4)
        //   │
        //   └── e                  (arm of length 2)
        let edges = [("a", "b"), ("b", "c"), ("c", "d"), ("a", "e")];
        let path = critical_path(&edges).unwrap();
        assert_eq!(path, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_critical_path_multiple_sources_and_sinks() {
        // Two disjoint chains of different lengths in a single edge list.
        //   a → b → c       (length 3)
        //   x → y           (length 2)
        let edges = [("a", "b"), ("b", "c"), ("x", "y")];
        let path = critical_path(&edges).unwrap();
        assert_eq!(path, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_critical_path_cycle_returns_error() {
        let edges = [("a", "b"), ("b", "a")];
        let err = critical_path(&edges).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    // -- weighted variant --

    #[test]
    fn test_critical_path_weighted_empty_graph() {
        let edges: &[(&str, &str)] = &[];
        let path = critical_path_weighted(edges, |_| 10).unwrap();
        assert!(path.is_empty());
    }

    #[test]
    fn test_critical_path_weighted_outvotes_length() {
        //   a ── b ── c       (unit length 3, total weight 3)
        //   a ── h            (unit length 2, but `h` is very heavy)
        // With unit weights, arm [a,b,c] wins. With a heavy `h`, arm [a,h] wins.
        let edges = [("a", "b"), ("b", "c"), ("a", "h")];

        let unit = critical_path(&edges).unwrap();
        assert_eq!(unit, vec!["a", "b", "c"]);

        let weighted = critical_path_weighted(&edges, |n| if *n == "h" { 100 } else { 1 }).unwrap();
        assert_eq!(weighted, vec!["a", "h"]);
    }

    #[test]
    fn test_critical_path_weighted_matches_unit_when_weight_is_one() {
        let edges = [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")];
        let unit = critical_path(&edges).unwrap();
        let weighted = critical_path_weighted(&edges, |_| 1).unwrap();
        assert_eq!(unit, weighted);
    }

    #[test]
    fn test_critical_path_weighted_cycle_returns_error() {
        let edges = [("a", "b"), ("b", "a")];
        let err = critical_path_weighted(&edges, |_| 1).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }
}
