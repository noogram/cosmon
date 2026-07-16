// SPDX-License-Identifier: AGPL-3.0-only

//! Standalone topological sort and ready-frontier computation.
//!
//! Thin wrappers over [`cosmon_graph`] bound to [`StepId`]. The generic
//! algorithm lives in the shared `cosmon-graph` crate; this module provides
//! the `StepId`-typed API that the rest of cosmon-core depends on.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::id::StepId;
//! use cosmon_core::toposort::{toposort, ready_frontier};
//! use std::collections::HashSet;
//!
//! let build = StepId::new("build").unwrap();
//! let test  = StepId::new("test").unwrap();
//! let edges = [(build.clone(), test.clone())];
//!
//! // Topological order: build before test.
//! let order = toposort(&edges).unwrap();
//! assert_eq!(order, vec![build.clone(), test.clone()]);
//!
//! // Nothing completed yet → only "build" is ready.
//! let completed = HashSet::new();
//! let ready = ready_frontier(&edges, &completed);
//! assert_eq!(ready, vec![build.clone()]);
//!
//! // After "build" completes → "test" is ready.
//! let completed: HashSet<StepId> = [build].into_iter().collect();
//! let ready = ready_frontier(&edges, &completed);
//! assert_eq!(ready, vec![test]);
//! ```

use std::collections::HashSet;

use crate::id::StepId;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Error returned when a topological sort cannot be computed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToposortError {
    /// The dependency graph contains a cycle.
    #[error("cycle detected involving step \"{0}\"")]
    Cycle(StepId),
}

// ---------------------------------------------------------------------------
// toposort
// ---------------------------------------------------------------------------

/// Topologically sort steps given their dependency edges.
///
/// Each edge `(a, b)` means "a must complete before b can start."
/// Returns all nodes reachable from the edges in a valid execution order.
/// Deterministic: ties broken by lexicographic `StepId` order.
///
/// # Errors
///
/// Returns [`ToposortError::Cycle`] if the graph contains a cycle.
pub fn toposort(edges: &[(StepId, StepId)]) -> Result<Vec<StepId>, ToposortError> {
    cosmon_graph::toposort(edges).map_err(|e| ToposortError::Cycle(e.0))
}

// ---------------------------------------------------------------------------
// ready_frontier
// ---------------------------------------------------------------------------

/// Compute the steps that are ready to execute given which steps have completed.
///
/// A step is "ready" when all its dependencies are in `completed` and the step
/// itself is not yet completed. For nodes with no incoming edges (roots), they
/// are ready as long as they are not completed.
///
/// Returns results sorted by lexicographic `StepId` order for determinism.
#[must_use]
pub fn ready_frontier<S: ::std::hash::BuildHasher>(
    edges: &[(StepId, StepId)],
    completed: &HashSet<StepId, S>,
) -> Vec<StepId> {
    cosmon_graph::ready_frontier(edges, completed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> StepId {
        StepId::new(s).unwrap()
    }

    // -- toposort --

    #[test]
    fn test_toposort_empty_graph() {
        let edges: &[(StepId, StepId)] = &[];
        let order = toposort(edges).unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn test_toposort_linear_chain() {
        let edges = [(sid("a"), sid("b")), (sid("b"), sid("c"))];
        let order = toposort(&edges).unwrap();
        assert_eq!(order, vec![sid("a"), sid("b"), sid("c")]);
    }

    #[test]
    fn test_toposort_diamond_dag() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d
        let edges = [
            (sid("a"), sid("b")),
            (sid("a"), sid("c")),
            (sid("b"), sid("d")),
            (sid("c"), sid("d")),
        ];
        let order = toposort(&edges).unwrap();
        assert_eq!(order[0], sid("a"));
        assert_eq!(order[3], sid("d"));
        // b and c can be in either order, but deterministic sort puts b before c.
        assert_eq!(order[1], sid("b"));
        assert_eq!(order[2], sid("c"));
    }

    #[test]
    fn test_toposort_cycle_detection() {
        let edges = [(sid("a"), sid("b")), (sid("b"), sid("a"))];
        let err = toposort(&edges).unwrap_err();
        assert!(matches!(err, ToposortError::Cycle(_)));
    }

    #[test]
    fn test_toposort_self_cycle() {
        let edges = [(sid("x"), sid("x"))];
        let err = toposort(&edges).unwrap_err();
        assert!(matches!(err, ToposortError::Cycle(_)));
    }

    #[test]
    fn test_toposort_three_node_cycle() {
        let edges = [
            (sid("a"), sid("b")),
            (sid("b"), sid("c")),
            (sid("c"), sid("a")),
        ];
        let err = toposort(&edges).unwrap_err();
        assert!(matches!(err, ToposortError::Cycle(_)));
    }

    // -- ready_frontier --

    #[test]
    fn test_ready_frontier_empty_graph() {
        let edges: &[(StepId, StepId)] = &[];
        let completed = HashSet::new();
        let ready = ready_frontier(edges, &completed);
        assert!(ready.is_empty());
    }

    #[test]
    fn test_ready_frontier_roots_ready() {
        let edges = [(sid("a"), sid("b"))];
        let completed = HashSet::new();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec![sid("a")]);
    }

    #[test]
    fn test_ready_frontier_after_root_completes() {
        let edges = [(sid("a"), sid("b"))];
        let completed: HashSet<StepId> = [sid("a")].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec![sid("b")]);
    }

    #[test]
    fn test_ready_frontier_diamond_dag() {
        let edges = [
            (sid("a"), sid("b")),
            (sid("a"), sid("c")),
            (sid("b"), sid("d")),
            (sid("c"), sid("d")),
        ];

        // Nothing done: only a is ready.
        let ready = ready_frontier(&edges, &HashSet::new());
        assert_eq!(ready, vec![sid("a")]);

        // a done: b and c are ready.
        let completed: HashSet<StepId> = [sid("a")].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec![sid("b"), sid("c")]);

        // a and b done: c still ready, d not yet (needs c).
        let completed: HashSet<StepId> = [sid("a"), sid("b")].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec![sid("c")]);

        // a, b, c done: d is ready.
        let completed: HashSet<StepId> = [sid("a"), sid("b"), sid("c")].into_iter().collect();
        let ready = ready_frontier(&edges, &completed);
        assert_eq!(ready, vec![sid("d")]);

        // All done: nothing ready.
        let completed: HashSet<StepId> = [sid("a"), sid("b"), sid("c"), sid("d")]
            .into_iter()
            .collect();
        let ready = ready_frontier(&edges, &completed);
        assert!(ready.is_empty());
    }
}
