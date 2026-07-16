// SPDX-License-Identifier: Apache-2.0

//! High-level façade: walk → parse → graph → rank → render.
//!
//! These functions compose the lower-level primitives into complete workflows.
//! Each function is a single entry point that takes a filesystem path and
//! returns a useful result — no intermediate steps required.

use std::path::Path;

use crate::error::CfsError;
use crate::extract::{build_references, extract_rust_symbols};
use crate::graph::{pagerank_default, SymbolGraph};
use crate::render::{symbol_outline, StructuralMap};
use crate::symbol::Symbol;
use crate::walk::walk_rust_files;

/// Parse an entire Rust project and produce a structural map ranked by `PageRank`.
///
/// Walks the directory at `root` (respecting `.gitignore`), extracts symbols
/// from every `.rs` file, builds a reference graph, computes `PageRank`, and
/// renders a [`StructuralMap`].
///
/// If `max_symbols` is `Some(n)`, only the top `n` symbols per module are
/// included in the rendered output.
///
/// # Errors
///
/// Returns errors from file walking, parsing, or ranking.
pub fn map_project(root: &Path, max_symbols: Option<usize>) -> Result<StructuralMap, CfsError> {
    let files = walk_rust_files(root)?;

    let mut all_symbols = Vec::new();
    for (path, source) in &files {
        let symbols = extract_rust_symbols(source, path)?;
        all_symbols.extend(symbols);
    }

    let sources: Vec<(&Path, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_path(), s.as_str()))
        .collect();
    let refs = build_references(&all_symbols, &sources);

    let mut graph = SymbolGraph::new();
    for sym in &all_symbols {
        graph.add_symbol(sym.clone());
    }
    for r in refs {
        graph.add_ref(r);
    }

    let ranked = pagerank_default(&graph)?;
    let mut map = StructuralMap::from_ranked(&ranked, graph.refs.len());

    // Apply max_symbols filter if set.
    if let Some(max) = max_symbols {
        for module in &mut map.modules {
            module.symbols.truncate(max);
        }
    }

    Ok(map)
}

/// Parse a single file and produce a symbol outline (no `PageRank`).
///
/// Returns a human-readable outline of all symbols in the file, grouped
/// by impl blocks.
///
/// # Errors
///
/// Returns [`CfsError::ReadFile`] if the file cannot be read, or
/// [`CfsError::ParseFailed`] if tree-sitter cannot parse it.
pub fn outline_file(path: &Path) -> Result<String, CfsError> {
    let source = std::fs::read_to_string(path).map_err(|e| CfsError::ReadFile {
        path: path.to_path_buf(),
        source: e,
    })?;

    let symbols = extract_rust_symbols(&source, path)?;
    Ok(symbol_outline(&symbols))
}

/// Search symbols by name across a project.
///
/// Walks the directory at `root`, extracts all symbols, and returns those
/// whose name contains `query` (case-insensitive substring match).
///
/// # Errors
///
/// Returns errors from file walking or parsing.
pub fn search_symbols(root: &Path, query: &str) -> Result<Vec<Symbol>, CfsError> {
    let files = walk_rust_files(root)?;
    let query_lower = query.to_lowercase();

    let mut matches = Vec::new();
    for (path, source) in &files {
        let symbols = extract_rust_symbols(source, path)?;
        for sym in symbols {
            if sym.name.to_lowercase().contains(&query_lower) {
                matches.push(sym);
            }
        }
    }

    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("main.rs"),
            r#"
pub fn main() {
    let g = greet();
    println!("{}", g);
}
"#,
        )
        .unwrap();
        fs::write(
            src.join("lib.rs"),
            r#"
pub struct Config {
    pub name: String,
}

impl Config {
    pub fn new() -> Self {
        Config { name: String::new() }
    }
}

pub fn greet() -> String {
    "hello".into()
}
"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn test_map_project() {
        let dir = setup_project();
        let map = map_project(dir.path(), None).unwrap();
        assert!(map.total_symbols > 0);
        assert!(!map.modules.is_empty());

        let md = map.to_markdown(0);
        assert!(md.contains("Structural Map"));
        assert!(md.contains("greet"));
    }

    #[test]
    fn test_map_project_with_limit() {
        let dir = setup_project();
        let map = map_project(dir.path(), Some(1)).unwrap();
        for module in &map.modules {
            assert!(module.symbols.len() <= 1);
        }
    }

    #[test]
    fn test_outline_file() {
        let dir = setup_project();
        let outline = outline_file(&dir.path().join("src/lib.rs")).unwrap();
        assert!(outline.contains("struct Config"));
        assert!(outline.contains("fn greet"));
        assert!(outline.contains("impl Config"));
        assert!(outline.contains("fn new"));
    }

    #[test]
    fn test_search_symbols() {
        let dir = setup_project();
        let results = search_symbols(dir.path(), "config").unwrap();
        assert!(results.iter().any(|s| s.name == "Config"));
    }

    #[test]
    fn test_search_symbols_case_insensitive() {
        let dir = setup_project();
        let results = search_symbols(dir.path(), "CONFIG").unwrap();
        assert!(results.iter().any(|s| s.name == "Config"));
    }

    #[test]
    fn test_search_symbols_no_match() {
        let dir = setup_project();
        let results = search_symbols(dir.path(), "nonexistent").unwrap();
        assert!(results.is_empty());
    }
}
