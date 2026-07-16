// SPDX-License-Identifier: AGPL-3.0-only

//! Pure graph mutation primitives for dynamic DAG re-planning.
//!
//! These functions support the ADR-016 §5 dynamic-DAG / decay-aware
//! re-planning case (see ADR-022). They let a scheduler prune a DAG
//! after nodes complete and splice in new subgraphs at runtime while
//! preserving acyclicity.
//!
//! Both functions are pure (no `&mut`), generic over the node type, and
//! deterministic: any tie-breaks use `Ord` on `N`.

use crate::{toposort, CycleError};
use std::collections::HashSet;
use std::hash::Hash;

// ---------------------------------------------------------------------------
// prune_completed
// ---------------------------------------------------------------------------

/// Remove every edge whose source is already in `completed`.
///
/// Returns the pruned edge list paired with the list of nodes that were
/// present in the original edge list but no longer appear anywhere in the
/// pruned edge list. The second component lets the caller retire nodes
/// whose incoming dependencies have all been satisfied and which have no
/// successors — they would otherwise "fall off" the edge-list view.
///
/// # Invariant
///
/// For every node `n` that still appears in `pruned`:
/// `n ∈ ready_frontier(edges, completed)` implies
/// `n ∈ ready_frontier(pruned, ∅)`.
///
/// In other words, pruning preserves the ready-frontier for nodes the
/// scheduler still tracks via edges. Nodes that were ready but whose only
/// anchoring edges pointed to completed predecessors are surfaced in
/// `removed_nodes` so the caller can dispatch or forget them explicitly.
///
/// # Determinism
///
/// Both return values are sorted by `Ord` on `N` so outputs are stable
/// across runs and insertion orders.
///
/// # Why pure
///
/// The function takes shared slices and returns owned `Vec`s so callers
/// (including the forthcoming Resident Runtime DAG policy) can re-plan
/// without mutating shared state. See ADR-022.
pub fn prune_completed<N, S>(edges: &[(N, N)], completed: &HashSet<N, S>) -> (Vec<(N, N)>, Vec<N>)
where
    N: Eq + Hash + Ord + Clone,
    S: ::std::hash::BuildHasher,
{
    let mut pruned: Vec<(N, N)> = edges
        .iter()
        .filter(|(src, _)| !completed.contains(src))
        .cloned()
        .collect();
    pruned.sort();

    let mut original: HashSet<&N> = HashSet::new();
    for (a, b) in edges {
        original.insert(a);
        original.insert(b);
    }
    let mut remaining: HashSet<&N> = HashSet::new();
    for (a, b) in &pruned {
        remaining.insert(a);
        remaining.insert(b);
    }

    let mut removed: Vec<N> = original
        .difference(&remaining)
        .map(|n| (*n).clone())
        .collect();
    removed.sort();

    (pruned, removed)
}

// ---------------------------------------------------------------------------
// insert_subgraph
// ---------------------------------------------------------------------------

/// Splice `new_edges` into `edges`, validating that the union remains acyclic.
///
/// The merged result is the set-union of the two edge lists (duplicates
/// removed). If adding the new edges would introduce a cycle, the
/// function returns [`CycleError`] without mutating anything — both
/// inputs are shared references, so non-mutation is also structurally
/// guaranteed by the signature.
///
/// # Invariant I6
///
/// If `edges` is acyclic and `new_edges` is acyclic and the union
/// `edges ∪ new_edges` is acyclic, the returned merged edge list is
/// acyclic. If the union would be cyclic, `Err(CycleError(node))` is
/// returned naming one node involved in the cycle, and the caller's
/// inputs are untouched.
///
/// # Idempotence
///
/// Re-inserting an edge that already exists is a no-op: duplicates are
/// collapsed in the merged output, so repeated calls with the same
/// `new_edges` converge to a fixed point.
///
/// # Determinism
///
/// The returned edge list is sorted by `Ord` on `(N, N)` for stable
/// ordering regardless of input order.
///
/// # Errors
///
/// Returns [`CycleError`] naming one node involved in the introduced
/// cycle if the union is not acyclic.
pub fn insert_subgraph<N>(
    edges: &[(N, N)],
    new_edges: &[(N, N)],
) -> Result<Vec<(N, N)>, CycleError<N>>
where
    N: Eq + Hash + Ord + Clone,
{
    let mut seen: HashSet<(N, N)> = HashSet::new();
    let mut merged: Vec<(N, N)> = Vec::with_capacity(edges.len() + new_edges.len());
    for edge in edges.iter().chain(new_edges.iter()) {
        if seen.insert(edge.clone()) {
            merged.push(edge.clone());
        }
    }

    // Validate acyclicity on the union. On error we propagate without
    // returning `merged`, so the caller's inputs are untouched.
    toposort(&merged)?;

    merged.sort();
    Ok(merged)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ready_frontier;

    // -- prune_completed --

    #[test]
    fn test_prune_completed_empty_graph() {
        let edges: &[(&str, &str)] = &[];
        let completed: HashSet<&str> = HashSet::new();
        let (pruned, removed) = prune_completed(edges, &completed);
        assert!(pruned.is_empty());
        assert!(removed.is_empty());
    }

    #[test]
    fn test_prune_completed_nothing_to_prune() {
        let edges = [("a", "b"), ("b", "c")];
        let completed: HashSet<&str> = HashSet::new();
        let (pruned, removed) = prune_completed(&edges, &completed);
        assert_eq!(pruned, vec![("a", "b"), ("b", "c")]);
        assert!(removed.is_empty());
    }

    #[test]
    fn test_prune_completed_single_edge_linear_chain() {
        // a → b → c, with `a` done. Edge (a,b) is dropped; (b,c) stays.
        let edges = [("a", "b"), ("b", "c")];
        let completed: HashSet<&str> = ["a"].into_iter().collect();
        let (pruned, removed) = prune_completed(&edges, &completed);
        assert_eq!(pruned, vec![("b", "c")]);
        assert_eq!(removed, vec!["a"]);

        // Invariant check on nodes that still appear in pruned.
        let original_ready = ready_frontier(&edges, &completed);
        let pruned_ready = ready_frontier(&pruned, &HashSet::<&str>::new());
        for n in &original_ready {
            if pruned.iter().any(|(x, y)| x == n || y == n) {
                assert!(
                    pruned_ready.contains(n),
                    "invariant violated for node {n:?}"
                );
            }
        }
    }

    #[test]
    fn test_prune_completed_diamond_dag() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d
        let edges = [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")];
        let completed: HashSet<&str> = ["a", "b"].into_iter().collect();
        let (pruned, removed) = prune_completed(&edges, &completed);

        // Only (c,d) survives: (a,*) dropped because a is completed; (b,d)
        // dropped because b is completed.
        assert_eq!(pruned, vec![("c", "d")]);
        assert_eq!(removed, vec!["a", "b"]);

        // Full invariant: ready_frontier(pruned, ∅) ⊇ ready_frontier(edges,
        // completed) for nodes still present in pruned.
        let original_ready = ready_frontier(&edges, &completed);
        let pruned_ready = ready_frontier(&pruned, &HashSet::<&str>::new());
        assert_eq!(original_ready, vec!["c"]);
        assert_eq!(pruned_ready, vec!["c"]);
    }

    #[test]
    fn test_prune_completed_everything_completed() {
        let edges = [("a", "b"), ("b", "c"), ("a", "c")];
        let completed: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let (pruned, removed) = prune_completed(&edges, &completed);
        assert!(pruned.is_empty());
        assert_eq!(removed, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_prune_completed_is_deterministic() {
        // Same inputs in different orders produce the same pruned edge list.
        let e1 = [("a", "b"), ("b", "c"), ("c", "d")];
        let e2 = [("c", "d"), ("a", "b"), ("b", "c")];
        let completed: HashSet<&str> = ["a"].into_iter().collect();
        let (p1, r1) = prune_completed(&e1, &completed);
        let (p2, r2) = prune_completed(&e2, &completed);
        assert_eq!(p1, p2);
        assert_eq!(r1, r2);
    }

    // -- insert_subgraph --

    #[test]
    fn test_insert_subgraph_successful_merge() {
        let edges = [("a", "b")];
        let new_edges = [("b", "c"), ("c", "d")];
        let merged = insert_subgraph(&edges, &new_edges).unwrap();
        assert_eq!(merged, vec![("a", "b"), ("b", "c"), ("c", "d")]);

        // Topological order still holds on the merged result.
        let order = toposort(&merged).unwrap();
        assert_eq!(order, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_insert_subgraph_disconnected_subgraph() {
        let edges = [("a", "b")];
        let new_edges = [("c", "d")];
        let merged = insert_subgraph(&edges, &new_edges).unwrap();
        assert_eq!(merged, vec![("a", "b"), ("c", "d")]);
    }

    #[test]
    fn test_insert_subgraph_cycle_rejected() {
        let edges = [("a", "b"), ("b", "c")];
        let new_edges = [("c", "a")];
        let err = insert_subgraph(&edges, &new_edges).unwrap_err();
        assert!(matches!(err, CycleError(_)));

        // Confirm inputs are untouched (structural — we still can
        // toposort the original slices).
        assert_eq!(toposort(&edges).unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_insert_subgraph_idempotent_reinsertion() {
        let edges = [("a", "b"), ("b", "c")];
        // Re-inserting the same edges must be a fixed point.
        let once = insert_subgraph(&edges, &edges).unwrap();
        assert_eq!(once, vec![("a", "b"), ("b", "c")]);

        let twice = insert_subgraph(&once, &edges).unwrap();
        assert_eq!(twice, once);
    }

    #[test]
    fn test_insert_subgraph_self_loop_rejected() {
        let edges = [("a", "b")];
        let new_edges = [("c", "c")];
        let err = insert_subgraph(&edges, &new_edges).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    #[test]
    fn test_insert_subgraph_deterministic_order() {
        // Different insertion orders produce the same merged edge list.
        let a = [("a", "b"), ("c", "d")];
        let b = [("c", "d"), ("a", "b")];
        let m1 = insert_subgraph(&a, &[]).unwrap();
        let m2 = insert_subgraph(&b, &[]).unwrap();
        assert_eq!(m1, m2);
    }

    #[test]
    fn test_insert_subgraph_with_owned_strings() {
        // Exercise the generic bound on an owning node type.
        let edges = [("a".to_owned(), "b".to_owned())];
        let new_edges = [("b".to_owned(), "c".to_owned())];
        let merged = insert_subgraph(&edges, &new_edges).unwrap();
        assert_eq!(
            merged,
            vec![
                ("a".to_owned(), "b".to_owned()),
                ("b".to_owned(), "c".to_owned()),
            ]
        );
    }
}
