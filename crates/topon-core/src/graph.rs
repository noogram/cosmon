// SPDX-License-Identifier: Apache-2.0

//! Symbol graph and `PageRank` ranking.
//!
//! A [`SymbolGraph`] holds extracted symbols and their cross-references.
//! [`pagerank`] computes importance scores using the standard `PageRank` algorithm,
//! producing a ranked list of symbols suitable for structural map generation.
//!
//! The `PageRank` algorithm treats each symbol as a web page and each reference as
//! a hyperlink. Symbols referenced by many other important symbols receive higher
//! rank. This naturally surfaces the "structural backbone" of a codebase — the
//! core types and functions that everything depends on.

use serde::{Deserialize, Serialize};

use crate::error::CfsError;
use crate::symbol::{Symbol, SymbolRef};

/// A graph of code symbols and their cross-references.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SymbolGraph {
    /// All symbols in the graph.
    pub symbols: Vec<Symbol>,
    /// Directed references between symbols (edges).
    pub refs: Vec<SymbolRef>,
}

impl SymbolGraph {
    /// Create an empty symbol graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a symbol to the graph, returning its index.
    pub fn add_symbol(&mut self, symbol: Symbol) -> usize {
        let idx = self.symbols.len();
        self.symbols.push(symbol);
        idx
    }

    /// Add a reference (edge) between two symbols.
    pub fn add_ref(&mut self, sym_ref: SymbolRef) {
        self.refs.push(sym_ref);
    }

    /// Number of symbols in the graph.
    #[must_use]
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the graph has no symbols.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

/// A symbol with its computed `PageRank` score.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankedSymbol {
    /// The symbol.
    pub symbol: Symbol,
    /// `PageRank` score (higher = more central).
    pub rank: f64,
}

/// Compute `PageRank` scores for all symbols in a graph.
///
/// Uses the standard iterative `PageRank` algorithm with damping factor `d`
/// (typically 0.85). Iterates until convergence (L1 norm < `epsilon`) or
/// `max_iter` iterations.
///
/// # Errors
///
/// Returns [`CfsError::RankNotConverged`] if the algorithm does not converge
/// within `max_iter` iterations. In practice this is rare for reasonable graphs.
#[allow(clippy::cast_precision_loss)]
pub fn pagerank(
    graph: &SymbolGraph,
    damping: f64,
    epsilon: f64,
    max_iter: usize,
) -> Result<Vec<RankedSymbol>, CfsError> {
    let n = graph.symbols.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    let init = 1.0 / n as f64;
    let mut rank = vec![init; n];
    let mut new_rank = vec![0.0_f64; n];

    // Build outgoing edge counts for normalization.
    let mut out_degree = vec![0_usize; n];
    for r in &graph.refs {
        if r.from < n {
            out_degree[r.from] += 1;
        }
    }

    for _iteration in 0..max_iter {
        // Base rank from damping.
        let base = (1.0 - damping) / n as f64;
        new_rank.fill(base);

        // Distribute rank along edges.
        for r in &graph.refs {
            if r.from < n && r.to < n && out_degree[r.from] > 0 {
                new_rank[r.to] += damping * rank[r.from] / out_degree[r.from] as f64;
            }
        }

        // Handle dangling nodes (no outgoing edges): redistribute their rank.
        let dangling_sum: f64 = (0..n)
            .filter(|&i| out_degree[i] == 0)
            .map(|i| rank[i])
            .sum();
        let dangling_contrib = damping * dangling_sum / n as f64;
        for val in &mut new_rank {
            *val += dangling_contrib;
        }

        // Check convergence.
        let diff: f64 = rank
            .iter()
            .zip(new_rank.iter())
            .map(|(old, new)| (old - new).abs())
            .sum();

        std::mem::swap(&mut rank, &mut new_rank);

        if diff < epsilon {
            return Ok(build_ranked(graph, &rank));
        }
    }

    Err(CfsError::RankNotConverged(max_iter))
}

/// Build ranked symbols sorted by rank descending.
fn build_ranked(graph: &SymbolGraph, rank: &[f64]) -> Vec<RankedSymbol> {
    let mut ranked: Vec<RankedSymbol> = graph
        .symbols
        .iter()
        .zip(rank.iter())
        .map(|(sym, &r)| RankedSymbol {
            symbol: sym.clone(),
            rank: r,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.rank
            .partial_cmp(&a.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

/// Compute `PageRank` with default parameters (`d`=0.85, `epsilon`=1e-6, `max_iter`=100).
///
/// # Errors
///
/// Returns [`CfsError::RankNotConverged`] if convergence fails.
pub fn pagerank_default(graph: &SymbolGraph) -> Result<Vec<RankedSymbol>, CfsError> {
    pagerank(graph, 0.85, 1e-6, 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::{RefKind, Span, SymbolKind};
    use std::path::PathBuf;

    fn make_symbol(name: &str) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: PathBuf::from("test.rs"),
            span: Span {
                start: 0,
                end: 10,
                start_line: 0,
                end_line: 1,
            },
            is_public: true,
            parent: None,
        }
    }

    #[test]
    fn test_empty_graph_pagerank() {
        let graph = SymbolGraph::new();
        let ranked = pagerank_default(&graph).unwrap();
        assert!(ranked.is_empty());
    }

    #[test]
    fn test_single_node_pagerank() {
        let mut graph = SymbolGraph::new();
        graph.add_symbol(make_symbol("main"));
        let ranked = pagerank_default(&graph).unwrap();
        assert_eq!(ranked.len(), 1);
        assert!((ranked[0].rank - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_linear_chain_pagerank() {
        // a -> b -> c: c should have highest rank (receives most flow).
        let mut graph = SymbolGraph::new();
        graph.add_symbol(make_symbol("a"));
        graph.add_symbol(make_symbol("b"));
        graph.add_symbol(make_symbol("c"));
        graph.add_ref(SymbolRef {
            from: 0,
            to: 1,
            kind: RefKind::Calls,
        });
        graph.add_ref(SymbolRef {
            from: 1,
            to: 2,
            kind: RefKind::Calls,
        });

        let ranked = pagerank_default(&graph).unwrap();
        assert_eq!(ranked.len(), 3);
        // c should be first (highest rank — it's the sink).
        assert_eq!(ranked[0].symbol.name, "c");
    }

    #[test]
    fn test_star_topology_pagerank() {
        // a, b, c all reference d: d should have highest rank.
        let mut graph = SymbolGraph::new();
        graph.add_symbol(make_symbol("a"));
        graph.add_symbol(make_symbol("b"));
        graph.add_symbol(make_symbol("c"));
        graph.add_symbol(make_symbol("d"));
        for from in 0..3 {
            graph.add_ref(SymbolRef {
                from,
                to: 3,
                kind: RefKind::References,
            });
        }

        let ranked = pagerank_default(&graph).unwrap();
        assert_eq!(ranked[0].symbol.name, "d");
    }

    #[test]
    fn test_pagerank_sums_to_one() {
        let mut graph = SymbolGraph::new();
        for name in &["a", "b", "c", "d"] {
            graph.add_symbol(make_symbol(name));
        }
        graph.add_ref(SymbolRef {
            from: 0,
            to: 1,
            kind: RefKind::Calls,
        });
        graph.add_ref(SymbolRef {
            from: 1,
            to: 2,
            kind: RefKind::Calls,
        });
        graph.add_ref(SymbolRef {
            from: 2,
            to: 0,
            kind: RefKind::Calls,
        });
        graph.add_ref(SymbolRef {
            from: 3,
            to: 1,
            kind: RefKind::References,
        });

        let ranked = pagerank_default(&graph).unwrap();
        let total: f64 = ranked.iter().map(|r| r.rank).sum();
        assert!(
            (total - 1.0).abs() < 1e-4,
            "PageRank should sum to 1.0, got {total}"
        );
    }

    #[test]
    fn test_graph_add_and_len() {
        let mut graph = SymbolGraph::new();
        assert!(graph.is_empty());
        let idx = graph.add_symbol(make_symbol("foo"));
        assert_eq!(idx, 0);
        assert_eq!(graph.len(), 1);
        assert!(!graph.is_empty());
    }

    #[test]
    fn test_symbol_graph_serde_roundtrip() {
        let mut graph = SymbolGraph::new();
        graph.add_symbol(make_symbol("a"));
        graph.add_symbol(make_symbol("b"));
        graph.add_ref(SymbolRef {
            from: 0,
            to: 1,
            kind: RefKind::Calls,
        });

        let json = serde_json::to_string(&graph).unwrap();
        let back: SymbolGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.symbols.len(), 2);
        assert_eq!(back.refs.len(), 1);
    }
}
