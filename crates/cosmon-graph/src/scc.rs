// SPDX-License-Identifier: AGPL-3.0-only

//! Strongly connected components via Tarjan's algorithm.
//!
//! A strongly connected component (SCC) is a maximal set of vertices where
//! every vertex is reachable from every other vertex via directed edges.
//! Cosmon uses SCCs on the **session-wait graph** to detect livelock — a
//! non-trivial SCC (size ≥ 2, or a self-loop) is a cycle of sessions each
//! blocked on another session in the cycle.
//!
//! Cost: `O(V + E)` per call; iterative implementation so large graphs do
//! not overflow the call stack.
//!
//! Output determinism: vertices inside each SCC are returned sorted by
//! `Ord`; the list of SCCs is sorted by the smallest vertex in each SCC.

use std::collections::HashMap;
use std::hash::Hash;

/// Frame for the iterative DFS used by Tarjan's algorithm.
///
/// Declared at module scope so the function body stays flat and clippy's
/// `items_after_statements` lint is satisfied.
#[derive(Debug)]
struct Frame {
    /// Vertex whose outgoing edges we are walking.
    v: usize,
    /// Index of the next outgoing edge to visit in `adj[v]`.
    next: usize,
}

/// Compute the strongly connected components of a directed graph.
///
/// The graph is described by `vertices` and `edges` where each edge
/// `(u, v)` denotes a directed edge from `u` to `v`. Vertices that appear
/// only in edges are added automatically; `vertices` guarantees isolated
/// nodes are included.
///
/// Returns a list of SCCs. Each SCC is a `Vec<N>` sorted by `Ord`. The
/// outer list is sorted by the first (smallest) vertex in each SCC.
///
/// # Panics
///
/// Does not panic on well-formed input. The internal `expect("visited")`
/// guards an invariant of the iterative DFS — a vertex is only folded
/// back into its parent after it has been discovered and assigned an
/// index. If that ever fires it indicates a logic bug in this module,
/// not a misuse of the public API.
///
/// # Examples
///
/// ```
/// use cosmon_graph::scc::tarjan_scc;
///
/// // Two-cycle A → B → A plus isolated C.
/// let vertices = vec!["a", "b", "c"];
/// let edges = vec![("a", "b"), ("b", "a")];
/// let sccs = tarjan_scc(&vertices, &edges);
/// assert_eq!(sccs, vec![vec!["a", "b"], vec!["c"]]);
/// ```
pub fn tarjan_scc<N>(vertices: &[N], edges: &[(N, N)]) -> Vec<Vec<N>>
where
    N: Eq + Hash + Ord + Clone,
{
    let (nodes, adj) = build_adjacency(vertices, edges);
    let sccs_by_index = find_sccs(&adj);
    materialise(&nodes, sccs_by_index)
}

/// Intern every vertex and build the deterministic adjacency list.
fn build_adjacency<N>(vertices: &[N], edges: &[(N, N)]) -> (Vec<N>, Vec<Vec<usize>>)
where
    N: Eq + Hash + Ord + Clone,
{
    let mut idx_of: HashMap<N, usize> = HashMap::new();
    let mut nodes: Vec<N> = Vec::new();
    let mut intern = |n: &N| -> usize {
        if let Some(&i) = idx_of.get(n) {
            return i;
        }
        let i = nodes.len();
        nodes.push(n.clone());
        idx_of.insert(n.clone(), i);
        i
    };
    for v in vertices {
        intern(v);
    }
    for (u, v) in edges {
        intern(u);
        intern(v);
    }

    let n = nodes.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (u, v) in edges {
        adj[idx_of[u]].push(idx_of[v]);
    }
    // Deterministic neighbour order (stable output regardless of input edge order).
    for list in &mut adj {
        list.sort_unstable();
        list.dedup();
    }
    (nodes, adj)
}

/// Run Tarjan's algorithm over an index-keyed adjacency list.
///
/// Returns SCCs as lists of node indices. Materialisation into `N`
/// happens in [`materialise`].
fn find_sccs(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index: usize = 0;
    let mut indices: Vec<Option<usize>> = vec![None; n];
    let mut lowlinks: Vec<usize> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if indices[start].is_some() {
            continue;
        }
        indices[start] = Some(index);
        lowlinks[start] = index;
        index += 1;
        stack.push(start);
        on_stack[start] = true;
        let mut work: Vec<Frame> = vec![Frame { v: start, next: 0 }];

        while let Some(frame) = work.last_mut() {
            let v = frame.v;
            if frame.next < adj[v].len() {
                let w = adj[v][frame.next];
                frame.next += 1;
                match indices[w] {
                    None => {
                        indices[w] = Some(index);
                        lowlinks[w] = index;
                        index += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        work.push(Frame { v: w, next: 0 });
                    }
                    Some(wi) if on_stack[w] => {
                        if wi < lowlinks[v] {
                            lowlinks[v] = wi;
                        }
                    }
                    _ => {}
                }
            } else {
                // v is done — emit its SCC if it is a root, then fold
                // lowlink back into its parent.
                let v_low = lowlinks[v];
                let v_idx = indices[v].expect("visited");
                if v_low == v_idx {
                    let mut component: Vec<usize> = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(component);
                }
                work.pop();
                if let Some(parent) = work.last_mut() {
                    if lowlinks[v] < lowlinks[parent.v] {
                        lowlinks[parent.v] = lowlinks[v];
                    }
                }
            }
        }
    }
    sccs
}

/// Sort within each SCC and across SCCs — deterministic output.
fn materialise<N>(nodes: &[N], sccs: Vec<Vec<usize>>) -> Vec<Vec<N>>
where
    N: Ord + Clone,
{
    let mut out: Vec<Vec<N>> = sccs
        .into_iter()
        .map(|ixs| {
            let mut vs: Vec<N> = ixs.into_iter().map(|i| nodes[i].clone()).collect();
            vs.sort();
            vs
        })
        .collect();
    out.sort_by(|a, b| a[0].cmp(&b[0]));
    out
}

/// Return only the non-trivial SCCs: size ≥ 2, or a single vertex with a self-loop.
///
/// This is the "livelock filter" — a session alone without a self-loop is
/// not a livelock, but a single session that (somehow) waits on itself is.
pub fn non_trivial_sccs<N>(vertices: &[N], edges: &[(N, N)]) -> Vec<Vec<N>>
where
    N: Eq + Hash + Ord + Clone,
{
    let mut self_loops: std::collections::HashSet<&N> = std::collections::HashSet::new();
    for (u, v) in edges {
        if u == v {
            self_loops.insert(u);
        }
    }
    tarjan_scc(vertices, edges)
        .into_iter()
        .filter(|comp| comp.len() >= 2 || (comp.len() == 1 && self_loops.contains(&comp[0])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    #[test]
    fn empty_graph() {
        let vs: Vec<&str> = vec![];
        let es: Vec<(&str, &str)> = vec![];
        assert!(tarjan_scc(&vs, &es).is_empty());
    }

    #[test]
    fn singleton_no_edge() {
        let vs = vec!["a"];
        let es: Vec<(&str, &str)> = vec![];
        assert_eq!(tarjan_scc(&vs, &es), vec![vec!["a"]]);
    }

    #[test]
    fn singleton_self_loop_is_non_trivial() {
        let vs = vec!["a"];
        let es = vec![("a", "a")];
        assert_eq!(tarjan_scc(&vs, &es), vec![vec!["a"]]);
        assert_eq!(non_trivial_sccs(&vs, &es), vec![vec!["a"]]);
    }

    #[test]
    fn simple_cycle_two_nodes() {
        let vs = vec!["a", "b"];
        let es = vec![("a", "b"), ("b", "a")];
        assert_eq!(tarjan_scc(&vs, &es), vec![vec!["a", "b"]]);
        assert_eq!(non_trivial_sccs(&vs, &es), vec![vec!["a", "b"]]);
    }

    #[test]
    fn dag_no_cycles_all_trivial() {
        // a → b → c (DAG)
        let vs = vec!["a", "b", "c"];
        let es = vec![("a", "b"), ("b", "c")];
        let all = tarjan_scc(&vs, &es);
        assert_eq!(all, vec![vec!["a"], vec!["b"], vec!["c"]]);
        assert!(non_trivial_sccs(&vs, &es).is_empty());
    }

    #[test]
    fn mixed_cycle_and_dag() {
        // a → b → a (cycle), c → a (enters cycle), c → d (tail).
        let vs = vec!["a", "b", "c", "d"];
        let es = vec![("a", "b"), ("b", "a"), ("c", "a"), ("c", "d")];
        let sccs = tarjan_scc(&vs, &es);
        // {a,b} is a cycle; {c} and {d} are singletons.
        assert!(sccs.contains(&vec!["a", "b"]));
        assert!(sccs.contains(&vec!["c"]));
        assert!(sccs.contains(&vec!["d"]));
        let live = non_trivial_sccs(&vs, &es);
        assert_eq!(live, vec![vec!["a", "b"]]);
    }

    #[test]
    fn three_node_cycle() {
        let vs = vec!["a", "b", "c"];
        let es = vec![("a", "b"), ("b", "c"), ("c", "a")];
        let sccs = tarjan_scc(&vs, &es);
        assert_eq!(sccs, vec![vec!["a", "b", "c"]]);
    }

    #[test]
    fn two_disjoint_cycles() {
        let vs = vec!["a", "b", "c", "d"];
        let es = vec![("a", "b"), ("b", "a"), ("c", "d"), ("d", "c")];
        let live = non_trivial_sccs(&vs, &es);
        assert_eq!(live, vec![vec!["a", "b"], vec!["c", "d"]]);
    }

    #[test]
    fn isolated_vertex_is_singleton_scc() {
        let vs = vec!["alone"];
        let es: Vec<(&str, &str)> = vec![];
        assert_eq!(tarjan_scc(&vs, &es), vec![vec!["alone"]]);
    }

    // --- property tests ---

    proptest! {
        #[test]
        fn scc_partition_covers_every_vertex(
            nodes in 1usize..12,
            edges in prop::collection::vec((0usize..12, 0usize..12), 0..30),
        ) {
            let vs: Vec<usize> = (0..nodes).collect();
            let edges: Vec<(usize, usize)> = edges
                .into_iter()
                .filter(|(a, b)| *a < nodes && *b < nodes)
                .collect();

            let sccs = tarjan_scc(&vs, &edges);

            // 1. Every vertex is in exactly one SCC.
            let mut seen: HashSet<usize> = HashSet::new();
            for comp in &sccs {
                for &v in comp {
                    prop_assert!(seen.insert(v), "vertex {v} appeared in two SCCs");
                }
            }
            prop_assert_eq!(seen.len(), nodes);

            // 2. Within each SCC, every pair is mutually reachable.
            //    (We verify by directly checking reachability via BFS.)
            let adj: Vec<Vec<usize>> = {
                let mut a = vec![Vec::new(); nodes];
                for (u, v) in &edges { a[*u].push(*v); }
                a
            };
            for comp in &sccs {
                for &s in comp {
                    for &t in comp {
                        if s == t { continue; }
                        prop_assert!(reachable(&adj, s, t), "{s} cannot reach {t} in same SCC");
                    }
                }
            }
        }

        #[test]
        fn scc_is_maximal(
            nodes in 2usize..8,
            edges in prop::collection::vec((0usize..8, 0usize..8), 0..20),
        ) {
            let vs: Vec<usize> = (0..nodes).collect();
            let edges: Vec<(usize, usize)> = edges
                .into_iter()
                .filter(|(a, b)| *a < nodes && *b < nodes)
                .collect();

            let sccs = tarjan_scc(&vs, &edges);

            // Build adjacency for reachability.
            let adj: Vec<Vec<usize>> = {
                let mut a = vec![Vec::new(); nodes];
                for (u, v) in &edges { a[*u].push(*v); }
                a
            };

            // Map each vertex to its SCC index.
            let mut comp_of = vec![usize::MAX; nodes];
            for (idx, comp) in sccs.iter().enumerate() {
                for &v in comp { comp_of[v] = idx; }
            }

            // For any two vertices in different SCCs, they are NOT both reachable from each other.
            for s in 0..nodes {
                for t in 0..nodes {
                    if s == t { continue; }
                    if comp_of[s] != comp_of[t] {
                        let s_to_t = reachable(&adj, s, t);
                        let t_to_s = reachable(&adj, t, s);
                        prop_assert!(!(s_to_t && t_to_s),
                            "{s} and {t} are in different SCCs but mutually reachable");
                    }
                }
            }
        }

        #[test]
        fn trivial_scc_has_no_livelock(
            nodes in 1usize..8,
        ) {
            // A linear DAG 0 → 1 → ... → nodes-1 has only trivial SCCs.
            let vs: Vec<usize> = (0..nodes).collect();
            let edges: Vec<(usize, usize)> = (0..nodes.saturating_sub(1)).map(|i| (i, i+1)).collect();
            let live = non_trivial_sccs(&vs, &edges);
            prop_assert!(live.is_empty());
        }
    }

    fn reachable(adj: &[Vec<usize>], src: usize, dst: usize) -> bool {
        let mut visited = vec![false; adj.len()];
        let mut stack = vec![src];
        visited[src] = true;
        while let Some(v) = stack.pop() {
            if v == dst {
                return true;
            }
            for &w in &adj[v] {
                if !visited[w] {
                    visited[w] = true;
                    stack.push(w);
                }
            }
        }
        false
    }
}
