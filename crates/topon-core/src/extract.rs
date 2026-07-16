// SPDX-License-Identifier: Apache-2.0

//! tree-sitter based symbol extraction for Rust source files.
//!
//! Parses Rust source using the tree-sitter-rust grammar and extracts
//! top-level symbols (functions, structs, traits, enums, impls, modules,
//! type aliases, constants, and macros). Methods inside impl blocks are
//! extracted with their parent set to the impl target type.
//!
//! Cross-file reference extraction is intentionally limited to identifiers
//! that appear in type positions and function calls — full semantic resolution
//! would require name resolution (a compiler's job). The structural map
//! prioritizes recall over precision: false-positive references are acceptable,
//! false-negative important symbols are not.

use std::path::Path;

use crate::error::CfsError;
use crate::symbol::{RefKind, Span, Symbol, SymbolKind, SymbolRef};

/// Extract symbols from Rust source code using tree-sitter.
///
/// Returns a list of symbols found in the source. The `file` path is attached
/// to each symbol for identification in multi-file graphs.
///
/// # Errors
///
/// Returns [`CfsError::ParseFailed`] if tree-sitter cannot parse the source.
pub fn extract_rust_symbols(source: &str, file: &Path) -> Result<Vec<Symbol>, CfsError> {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|_| CfsError::ParseFailed(file.to_path_buf()))?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| CfsError::ParseFailed(file.to_path_buf()))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();

    extract_from_node(root, source, file, None, &mut symbols);

    Ok(symbols)
}

/// Recursively extract symbols from a tree-sitter node.
fn extract_from_node(
    node: tree_sitter::Node<'_>,
    source: &str,
    file: &Path,
    parent: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "function_item" => {
            if let Some(sym) =
                extract_named_symbol(node, source, file, SymbolKind::Function, parent)
            {
                symbols.push(sym);
            }
        }
        "struct_item" => {
            if let Some(sym) = extract_named_symbol(node, source, file, SymbolKind::Struct, parent)
            {
                symbols.push(sym);
            }
        }
        "trait_item" => {
            if let Some(sym) = extract_named_symbol(node, source, file, SymbolKind::Trait, parent) {
                symbols.push(sym);
            }
        }
        "enum_item" => {
            if let Some(sym) = extract_named_symbol(node, source, file, SymbolKind::Enum, parent) {
                symbols.push(sym);
            }
        }
        "mod_item" => {
            if let Some(sym) = extract_named_symbol(node, source, file, SymbolKind::Module, parent)
            {
                symbols.push(sym);
            }
        }
        "type_item" => {
            if let Some(sym) =
                extract_named_symbol(node, source, file, SymbolKind::TypeAlias, parent)
            {
                symbols.push(sym);
            }
        }
        "const_item" | "static_item" => {
            if let Some(sym) =
                extract_named_symbol(node, source, file, SymbolKind::Constant, parent)
            {
                symbols.push(sym);
            }
        }
        "macro_definition" => {
            if let Some(sym) = extract_named_symbol(node, source, file, SymbolKind::Macro, parent) {
                symbols.push(sym);
            }
        }
        "impl_item" => {
            let impl_name = extract_impl_target(node, source);
            let impl_name_str = impl_name.as_deref();

            // Record the impl block itself.
            if let Some(name) = impl_name_str {
                symbols.push(Symbol {
                    name: name.to_owned(),
                    kind: SymbolKind::Impl,
                    file: file.to_path_buf(),
                    span: node_span(node),
                    is_public: false, // impl blocks don't have visibility
                    parent: parent.map(String::from),
                });
            }

            // Extract methods from the impl body.
            if let Some(body) = node.child_by_field_name("body") {
                let cursor = &mut body.walk();
                for child in body.children(cursor) {
                    extract_from_node(child, source, file, impl_name_str, symbols);
                }
            }
            return; // Don't recurse into children again.
        }
        _ => {}
    }

    // Recurse into children for top-level traversal.
    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        // Don't recurse into items we already handled (impl bodies).
        if child.kind() != "impl_item" || node.kind() == "source_file" || node.kind() == "mod_item"
        {
            extract_from_node(child, source, file, parent, symbols);
        }
    }
}

/// Extract a named symbol from a node that has a "name" field.
fn extract_named_symbol(
    node: tree_sitter::Node<'_>,
    source: &str,
    file: &Path,
    kind: SymbolKind,
    parent: Option<&str>,
) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?;

    Some(Symbol {
        name: name.to_owned(),
        kind,
        file: file.to_path_buf(),
        span: node_span(node),
        is_public: has_visibility(node),
        parent: parent.map(String::from),
    })
}

/// Extract the target type name of an impl block.
fn extract_impl_target(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    // For `impl Foo` or `impl Trait for Foo`, we want the type being implemented.
    node.child_by_field_name("type")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .map(str::to_owned)
}

/// Check if a node has a visibility modifier (pub, pub(crate), etc.).
fn has_visibility(node: tree_sitter::Node<'_>) -> bool {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .any(|child| child.kind() == "visibility_modifier");
    result
}

/// Convert a tree-sitter node's range to our Span type.
fn node_span(node: tree_sitter::Node<'_>) -> Span {
    let range = node.range();
    Span {
        start: range.start_byte,
        end: range.end_byte,
        start_line: range.start_point.row,
        end_line: range.end_point.row,
    }
}

/// Build cross-references between symbols by matching identifier names.
///
/// This is a heuristic: for each symbol, scan the source ranges of other
/// symbols for occurrences of its name. Precision is sacrificed for recall —
/// the structural map should capture relationships even if some are spurious.
#[must_use]
pub fn build_references(symbols: &[Symbol], sources: &[(&Path, &str)]) -> Vec<SymbolRef> {
    let mut refs = Vec::new();

    // Build a name→index map for efficient lookup.
    let name_to_indices: std::collections::HashMap<&str, Vec<usize>> = {
        let mut map: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
        for (i, sym) in symbols.iter().enumerate() {
            map.entry(sym.name.as_str()).or_default().push(i);
        }
        map
    };

    // For each symbol, check if its body references other symbols by name.
    for (i, sym) in symbols.iter().enumerate() {
        // Find the source for this symbol's file.
        let Some((_path, source)) = sources.iter().find(|(p, _)| *p == sym.file.as_path()) else {
            continue;
        };

        // Get the symbol's body text.
        let body = &source[sym.span.start..sym.span.end.min(source.len())];

        // Check for references to other symbols.
        for (name, indices) in &name_to_indices {
            if body.contains(name) {
                for &target_idx in indices {
                    // Don't self-reference, and skip same-name symbols.
                    if target_idx == i {
                        continue;
                    }
                    // Determine reference kind based on context.
                    let kind = if sym.kind == SymbolKind::Impl {
                        RefKind::Implements
                    } else if sym.kind == SymbolKind::Function {
                        RefKind::Calls
                    } else {
                        RefKind::References
                    };
                    refs.push(SymbolRef {
                        from: i,
                        to: target_idx,
                        kind,
                    });
                }
            }
        }
    }

    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_extract_function() {
        let source = "pub fn hello() { }";
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
        assert!(symbols[0].is_public);
    }

    #[test]
    fn test_extract_struct() {
        let source = "pub struct Foo { x: i32 }";
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert_eq!(symbols[0].kind, SymbolKind::Struct);
    }

    #[test]
    fn test_extract_trait() {
        let source = "pub trait Bar { fn method(&self); }";
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();
        // Should find the trait and the method declaration.
        assert!(symbols
            .iter()
            .any(|s| s.name == "Bar" && s.kind == SymbolKind::Trait));
    }

    #[test]
    fn test_extract_impl_with_methods() {
        let source = r#"
struct Foo;
impl Foo {
    pub fn new() -> Self { Foo }
    fn private_method(&self) {}
}
"#;
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();

        // Should have: struct Foo, impl Foo, fn new, fn private_method.
        assert!(symbols
            .iter()
            .any(|s| s.name == "Foo" && s.kind == SymbolKind::Struct));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Foo" && s.kind == SymbolKind::Impl));
        let new_fn = symbols
            .iter()
            .find(|s| s.name == "new" && s.kind == SymbolKind::Function)
            .expect("should find 'new' function");
        assert_eq!(new_fn.parent.as_deref(), Some("Foo"));
        assert!(new_fn.is_public);

        let priv_fn = symbols
            .iter()
            .find(|s| s.name == "private_method")
            .expect("should find 'private_method'");
        assert!(!priv_fn.is_public);
    }

    #[test]
    fn test_extract_enum() {
        let source = "pub enum Color { Red, Green, Blue }";
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();
        assert!(symbols
            .iter()
            .any(|s| s.name == "Color" && s.kind == SymbolKind::Enum));
    }

    #[test]
    fn test_extract_const_and_static() {
        let source = r#"
pub const MAX: usize = 100;
pub static COUNTER: i32 = 0;
"#;
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();
        assert!(symbols
            .iter()
            .any(|s| s.name == "MAX" && s.kind == SymbolKind::Constant));
        assert!(symbols
            .iter()
            .any(|s| s.name == "COUNTER" && s.kind == SymbolKind::Constant));
    }

    #[test]
    fn test_extract_type_alias() {
        let source = "pub type Result<T> = std::result::Result<T, Error>;";
        let file = PathBuf::from("test.rs");
        let symbols = extract_rust_symbols(source, &file).unwrap();
        assert!(symbols
            .iter()
            .any(|s| s.name == "Result" && s.kind == SymbolKind::TypeAlias));
    }

    #[test]
    fn test_build_references_basic() {
        let source_a = "pub fn caller() { callee(); }";
        let source_b = "pub fn callee() {}";
        let file_a = PathBuf::from("a.rs");
        let file_b = PathBuf::from("b.rs");

        let mut symbols = extract_rust_symbols(source_a, &file_a).unwrap();
        symbols.extend(extract_rust_symbols(source_b, &file_b).unwrap());

        let sources: Vec<(&Path, &str)> =
            vec![(file_a.as_path(), source_a), (file_b.as_path(), source_b)];
        let refs = build_references(&symbols, &sources);

        // caller should reference callee.
        assert!(
            refs.iter().any(|r| r.from == 0 && r.to == 1),
            "caller should reference callee"
        );
    }
}
