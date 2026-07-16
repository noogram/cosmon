// SPDX-License-Identifier: Apache-2.0

//! Structural map rendering — markdown and JSON output.
//!
//! Produces per-module markdown maps (human-readable outlines) and JSON sidecars
//! (machine-readable structured data). The markdown format follows the Aider
//! "repo map" pattern: a hierarchical outline of symbols ranked by importance.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graph::RankedSymbol;
use crate::symbol::SymbolKind;

/// A structural map for a single file/module.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleMap {
    /// The source file path.
    pub path: PathBuf,
    /// Ranked symbols in this module.
    pub symbols: Vec<RankedSymbol>,
}

/// A complete structural map for a codebase or vault.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StructuralMap {
    /// Per-module maps, keyed by file path.
    pub modules: Vec<ModuleMap>,
    /// Total number of symbols across all modules.
    pub total_symbols: usize,
    /// Total number of cross-references.
    pub total_refs: usize,
}

impl StructuralMap {
    /// Build a structural map from ranked symbols.
    #[must_use]
    pub fn from_ranked(ranked: &[RankedSymbol], total_refs: usize) -> Self {
        let mut by_file: BTreeMap<&Path, Vec<RankedSymbol>> = BTreeMap::new();
        for rs in ranked {
            by_file
                .entry(rs.symbol.file.as_path())
                .or_default()
                .push(rs.clone());
        }

        let modules: Vec<ModuleMap> = by_file
            .into_iter()
            .map(|(path, symbols)| ModuleMap {
                path: path.to_path_buf(),
                symbols,
            })
            .collect();

        let total_symbols = ranked.len();

        Self {
            modules,
            total_symbols,
            total_refs,
        }
    }

    /// Render the structural map as markdown.
    ///
    /// Produces an Aider-style repo map: each file is a section header, followed
    /// by indented symbol outlines sorted by rank. Only the top `max_symbols`
    /// symbols per module are included (0 = unlimited).
    #[must_use]
    pub fn to_markdown(&self, max_symbols_per_module: usize) -> String {
        let mut out = String::new();
        out.push_str("# Structural Map\n\n");
        let _ = writeln!(
            out,
            "> {} symbols, {} references across {} modules\n",
            self.total_symbols,
            self.total_refs,
            self.modules.len(),
        );

        for module in &self.modules {
            let _ = writeln!(out, "## {}\n", module.path.display());

            let limit = if max_symbols_per_module == 0 {
                module.symbols.len()
            } else {
                max_symbols_per_module.min(module.symbols.len())
            };

            for rs in module.symbols.iter().take(limit) {
                let vis = if rs.symbol.is_public { "pub " } else { "" };
                let parent_prefix = rs
                    .symbol
                    .parent
                    .as_ref()
                    .map_or(String::new(), |p| format!("{p}::"));

                let _ = writeln!(
                    out,
                    "- {vis}{} {parent_prefix}{} (rank: {:.4}, L{}–L{})",
                    rs.symbol.kind,
                    rs.symbol.name,
                    rs.rank,
                    rs.symbol.span.start_line + 1,
                    rs.symbol.span.end_line + 1,
                );
            }
            out.push('\n');
        }

        out
    }

    /// Render the structural map as a JSON string.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails (should not happen for valid data).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render the structural map as a JSON value.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// Render a simple per-file symbol outline (no `PageRank`, just extracted symbols).
///
/// Useful for quick structural inspection without the full graph pipeline.
#[must_use]
pub fn symbol_outline(symbols: &[crate::symbol::Symbol]) -> String {
    let mut by_file: BTreeMap<&Path, Vec<&crate::symbol::Symbol>> = BTreeMap::new();
    for sym in symbols {
        by_file.entry(sym.file.as_path()).or_default().push(sym);
    }

    let mut out = String::new();
    for (path, file_symbols) in &by_file {
        let _ = writeln!(out, "{}:", path.display());

        // Group by parent for impl blocks.
        let mut top_level: Vec<&&crate::symbol::Symbol> = Vec::new();
        let mut by_parent: BTreeMap<&str, Vec<&&crate::symbol::Symbol>> = BTreeMap::new();

        for sym in file_symbols {
            if let Some(ref parent) = sym.parent {
                // Skip impl block entries themselves — show methods under them.
                if sym.kind != SymbolKind::Impl {
                    by_parent.entry(parent.as_str()).or_default().push(sym);
                }
            } else if sym.kind != SymbolKind::Impl {
                top_level.push(sym);
            }
        }

        for sym in &top_level {
            let vis = if sym.is_public { "pub " } else { "" };
            let _ = writeln!(out, "  {vis}{} {}", sym.kind, sym.name);
        }

        for (parent, methods) in &by_parent {
            let _ = writeln!(out, "  impl {parent}");
            for sym in methods {
                let vis = if sym.is_public { "pub " } else { "" };
                let _ = writeln!(out, "    {vis}{} {}", sym.kind, sym.name);
            }
        }

        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::RankedSymbol;
    use crate::symbol::{Span, Symbol, SymbolKind};

    fn make_ranked(name: &str, kind: SymbolKind, file: &str, rank: f64) -> RankedSymbol {
        RankedSymbol {
            symbol: Symbol {
                name: name.into(),
                kind,
                file: PathBuf::from(file),
                span: Span {
                    start: 0,
                    end: 100,
                    start_line: 0,
                    end_line: 10,
                },
                is_public: true,
                parent: None,
            },
            rank,
        }
    }

    #[test]
    fn test_structural_map_from_ranked() {
        let ranked = vec![
            make_ranked("Foo", SymbolKind::Struct, "src/a.rs", 0.5),
            make_ranked("bar", SymbolKind::Function, "src/a.rs", 0.3),
            make_ranked("Baz", SymbolKind::Trait, "src/b.rs", 0.2),
        ];
        let map = StructuralMap::from_ranked(&ranked, 5);
        assert_eq!(map.modules.len(), 2);
        assert_eq!(map.total_symbols, 3);
        assert_eq!(map.total_refs, 5);
    }

    #[test]
    fn test_structural_map_markdown() {
        let ranked = vec![
            make_ranked("Foo", SymbolKind::Struct, "src/lib.rs", 0.6),
            make_ranked("new", SymbolKind::Function, "src/lib.rs", 0.4),
        ];
        let map = StructuralMap::from_ranked(&ranked, 1);
        let md = map.to_markdown(0);
        assert!(md.contains("# Structural Map"));
        assert!(md.contains("src/lib.rs"));
        assert!(md.contains("pub struct Foo"));
        assert!(md.contains("pub fn new"));
    }

    #[test]
    fn test_structural_map_json_roundtrip() {
        let ranked = vec![make_ranked(
            "main",
            SymbolKind::Function,
            "src/main.rs",
            1.0,
        )];
        let map = StructuralMap::from_ranked(&ranked, 0);
        let json = map.to_json().unwrap();
        let back: StructuralMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.total_symbols, 1);
        assert_eq!(back.modules.len(), 1);
    }

    #[test]
    fn test_symbol_outline() {
        let symbols = vec![
            Symbol {
                name: "Foo".into(),
                kind: SymbolKind::Struct,
                file: "src/lib.rs".into(),
                span: Span {
                    start: 0,
                    end: 50,
                    start_line: 0,
                    end_line: 3,
                },
                is_public: true,
                parent: None,
            },
            Symbol {
                name: "new".into(),
                kind: SymbolKind::Function,
                file: "src/lib.rs".into(),
                span: Span {
                    start: 60,
                    end: 100,
                    start_line: 5,
                    end_line: 8,
                },
                is_public: true,
                parent: Some("Foo".into()),
            },
        ];
        let outline = symbol_outline(&symbols);
        assert!(outline.contains("src/lib.rs:"));
        assert!(outline.contains("pub struct Foo"));
        assert!(outline.contains("impl Foo"));
        assert!(outline.contains("pub fn new"));
    }

    #[test]
    fn test_markdown_respects_max_symbols() {
        let ranked = vec![
            make_ranked("a", SymbolKind::Function, "src/lib.rs", 0.5),
            make_ranked("b", SymbolKind::Function, "src/lib.rs", 0.3),
            make_ranked("c", SymbolKind::Function, "src/lib.rs", 0.2),
        ];
        let map = StructuralMap::from_ranked(&ranked, 0);
        let md = map.to_markdown(2);
        // Should only include 2 symbols.
        let fn_count = md.matches("pub fn").count();
        assert_eq!(fn_count, 2);
    }
}
