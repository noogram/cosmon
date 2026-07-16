// SPDX-License-Identifier: AGPL-3.0-only

#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Lightweight DAG primitives: topological sort and ready-frontier.
//!
//! Generic over node type. No external dependencies beyond `std` and `thiserror`.
//! Edges are `(dependency, dependent)` pairs: the first node must complete
//! before the second can begin.
//!
//! # Examples
//!
//! ```
//! use cosmon_graph::{toposort, ready_frontier, CycleError};
//! use std::collections::HashSet;
//!
//! let edges = [("build", "test"), ("test", "deploy")];
//!
//! // Topological order: build → test → deploy.
//! let order = toposort(&edges).unwrap();
//! assert_eq!(order, vec!["build", "test", "deploy"]);
//!
//! // Nothing completed yet → only "build" is ready.
//! let ready = ready_frontier(&edges, &HashSet::new());
//! assert_eq!(ready, vec!["build"]);
//!
//! // After "build" completes → "test" is ready.
//! let completed: HashSet<&str> = ["build"].into_iter().collect();
//! let ready = ready_frontier(&edges, &completed);
//! assert_eq!(ready, vec!["test"]);
//! ```

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

mod affinity;
mod critical_path;
mod mutation;
mod plan;
pub mod scc;

pub use affinity::{affinity_order, model_switch_count};
pub use critical_path::{critical_path, critical_path_weighted};
pub use mutation::{insert_subgraph, prune_completed};
pub use plan::Plan;
pub use scc::{non_trivial_sccs, tarjan_scc};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// A cycle was detected during topological sort.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cycle detected involving node \"{0:?}\"")]
pub struct CycleError<N>(
    /// The node involved in the cycle.
    pub N,
);

// ---------------------------------------------------------------------------
// toposort
// ---------------------------------------------------------------------------

/// Topologically sort nodes given dependency edges.
///
/// Each edge `(a, b)` means "a must complete before b can start."
/// Returns all nodes reachable from the edges in a valid execution order.
/// Deterministic: ties broken by `Ord` ordering of `N`.
///
/// # Errors
///
/// Returns [`CycleError`] if the graph contains a cycle.
pub fn toposort<N>(edges: &[(N, N)]) -> Result<Vec<N>, CycleError<N>>
where
    N: Eq + Hash + Ord + Clone,
{
    // Collect all nodes.
    let mut nodes: HashSet<&N> = HashSet::new();
    for (a, b) in edges {
        nodes.insert(a);
        nodes.insert(b);
    }

    // Build in-degree map and adjacency list.
    let mut in_degree: HashMap<&N, usize> = nodes.iter().map(|&n| (n, 0)).collect();
    let mut dependents: HashMap<&N, Vec<&N>> = HashMap::new();

    for (dep, step) in edges {
        dependents.entry(dep).or_default().push(step);
        *in_degree.entry(step).or_default() += 1;
    }

    // Kahn's algorithm with deterministic tie-breaking.
    let mut queue: Vec<&N> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();
    queue.sort();

    let mut result: Vec<N> = Vec::with_capacity(nodes.len());

    while let Some(node) = queue.first().copied() {
        queue.remove(0);
        result.push(node.clone());

        if let Some(deps) = dependents.get(node) {
            let mut newly_ready: Vec<&N> = Vec::new();
            for &dep in deps {
                let Some(deg) = in_degree.get_mut(dep) else {
                    continue;
                };
                *deg -= 1;
                if *deg == 0 {
                    newly_ready.push(dep);
                }
            }
            newly_ready.sort();
            queue.extend(newly_ready);
        }
    }

    if result.len() != nodes.len() {
        // Find a stuck node for the error message.
        if let Some((&stuck, _)) = in_degree.iter().find(|(_, &deg)| deg > 0) {
            return Err(CycleError(stuck.clone()));
        }
        // Logically unreachable: if result.len() < nodes.len(), some node has deg > 0.
        return Err(CycleError(
            result.pop().unwrap_or_else(|| edges[0].0.clone()),
        ));
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// ready_frontier
// ---------------------------------------------------------------------------

/// Compute nodes ready to execute given which nodes have completed.
///
/// A node is "ready" when all its dependencies are in `completed` and the node
/// itself is not yet completed. For nodes with no incoming edges (roots), they
/// are ready as long as they are not completed.
///
/// Returns results sorted by `Ord` ordering of `N` for determinism.
pub fn ready_frontier<N, S>(edges: &[(N, N)], completed: &HashSet<N, S>) -> Vec<N>
where
    N: Eq + Hash + Ord + Clone,
    S: ::std::hash::BuildHasher,
{
    // Collect all nodes.
    let mut nodes: HashSet<&N> = HashSet::new();
    for (a, b) in edges {
        nodes.insert(a);
        nodes.insert(b);
    }

    // Build dependency map: node → set of its dependencies.
    let mut deps_of: HashMap<&N, Vec<&N>> = HashMap::new();
    for (dep, step) in edges {
        deps_of.entry(step).or_default().push(dep);
    }

    let mut ready: Vec<N> = nodes
        .into_iter()
        .filter(|&node| {
            if completed.contains(node) {
                return false;
            }
            // All dependencies must be completed.
            deps_of
                .get(node)
                .is_none_or(|deps| deps.iter().all(|d| completed.contains(*d)))
        })
        .cloned()
        .collect();

    ready.sort();
    ready
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- toposort --

    #[test]
    fn test_toposort_empty_graph() {
        let edges: &[(String, String)] = &[];
        let order = toposort(edges).unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn test_toposort_linear_chain() {
        let edges = [("a", "b"), ("b", "c")];
        let order = toposort(&edges).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_toposort_diamond_dag() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d
        let edges = [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")];
        let order = toposort(&edges).unwrap();
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
        // b and c can be in either order, but deterministic sort puts b before c.
        assert_eq!(order[1], "b");
        assert_eq!(order[2], "c");
    }

    #[test]
    fn test_toposort_cycle_detection() {
        let edges = [("a", "b"), ("b", "a")];
        let err = toposort(&edges).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    #[test]
    fn test_toposort_self_cycle() {
        let edges = [("x", "x")];
        let err = toposort(&edges).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    #[test]
    fn test_toposort_three_node_cycle() {
        let edges = [("a", "b"), ("b", "c"), ("c", "a")];
        let err = toposort(&edges).unwrap_err();
        assert!(matches!(err, CycleError(_)));
    }

    #[test]
    fn test_toposort_with_string_ids() {
        let edges = [
            ("build".to_owned(), "test".to_owned()),
            ("test".to_owned(), "deploy".to_owned()),
        ];
        let order = toposort(&edges).unwrap();
        assert_eq!(order, vec!["build", "test", "deploy"]);
    }

    #[test]
    fn test_toposort_with_integer_ids() {
        let edges = [(1, 2), (2, 3), (1, 3)];
        let order = toposort(&edges).unwrap();
        assert_eq!(order, vec![1, 2, 3]);
    }

    // -- ready_frontier --

    #[test]
    fn test_ready_frontier_empty_graph() {
        let edges: &[(&str, &str)] = &[];
        let completed = HashSet::new();
        let ready = ready_frontier(edges, &completed);
        assert!(ready.is_empty());
    }

    #[test]
    fn test_ready_frontier_roots_ready() {
        let edges = [("a", "b")];
        let completed = HashSet::new();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec!["a"]);
    }

    #[test]
    fn test_ready_frontier_after_root_completes() {
        let edges = [("a", "b")];
        let completed: HashSet<&str> = ["a"].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec!["b"]);
    }

    #[test]
    fn test_ready_frontier_diamond_dag() {
        let edges = [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")];

        // Nothing done: only a is ready.
        let ready = ready_frontier(&edges, &HashSet::new());
        assert_eq!(ready, vec!["a"]);

        // a done: b and c are ready.
        let completed: HashSet<&str> = ["a"].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec!["b", "c"]);

        // a and b done: c still ready, d not yet (needs c).
        let completed: HashSet<&str> = ["a", "b"].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec!["c"]);

        // a, b, c done: d is ready.
        let completed: HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec!["d"]);

        // All done: nothing ready.
        let completed: HashSet<&str> = ["a", "b", "c", "d"].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert!(ready.is_empty());
    }
}
