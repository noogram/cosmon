// SPDX-License-Identifier: Apache-2.0

//! Code symbol types — functions, structs, traits, impls extracted from source.
//!
//! Symbols are the nodes in a structural map. Each symbol has a kind, a name,
//! and a byte-range span in its source file. Symbol references (edges) capture
//! how symbols relate: a function calls another, a struct field references a type,
//! a trait impl satisfies a trait definition.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What kind of code symbol this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A function or method definition.
    Function,
    /// A struct definition.
    Struct,
    /// A trait definition.
    Trait,
    /// An impl block (inherent or trait).
    Impl,
    /// An enum definition.
    Enum,
    /// A module declaration.
    Module,
    /// A type alias.
    TypeAlias,
    /// A constant or static.
    Constant,
    /// A macro definition.
    Macro,
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Function => f.write_str("fn"),
            Self::Struct => f.write_str("struct"),
            Self::Trait => f.write_str("trait"),
            Self::Impl => f.write_str("impl"),
            Self::Enum => f.write_str("enum"),
            Self::Module => f.write_str("mod"),
            Self::TypeAlias => f.write_str("type"),
            Self::Constant => f.write_str("const"),
            Self::Macro => f.write_str("macro"),
        }
    }
}

/// Byte-range span within a source file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    /// Starting byte offset.
    pub start: usize,
    /// Ending byte offset (exclusive).
    pub end: usize,
    /// Starting line (0-indexed).
    pub start_line: usize,
    /// Ending line (0-indexed).
    pub end_line: usize,
}

/// A code symbol extracted from source.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Symbol {
    /// The symbol's name (e.g. `"new"`, `"AgentId"`, `"ContextManager"`).
    pub name: String,
    /// What kind of symbol this is.
    pub kind: SymbolKind,
    /// The file this symbol lives in.
    pub file: PathBuf,
    /// Byte span in the source file.
    pub span: Span,
    /// Visibility: `true` if `pub` (or `pub(crate)`, etc.).
    pub is_public: bool,
    /// Optional parent symbol name (e.g. impl block name for methods).
    pub parent: Option<String>,
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref parent) = self.parent {
            write!(f, "{}::{}::{}", self.file.display(), parent, self.name)
        } else {
            write!(f, "{}::{}", self.file.display(), self.name)
        }
    }
}

/// How one symbol references another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// A function/method call.
    Calls,
    /// A type reference (field type, parameter type, return type).
    References,
    /// A trait implementation.
    Implements,
    /// A module import (`use` statement).
    Imports,
}

/// A directed reference from one symbol to another.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolRef {
    /// Index of the source symbol in the graph's symbol list.
    pub from: usize,
    /// Index of the target symbol in the graph's symbol list.
    pub to: usize,
    /// The kind of reference.
    pub kind: RefKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_kind_display() {
        assert_eq!(SymbolKind::Function.to_string(), "fn");
        assert_eq!(SymbolKind::Struct.to_string(), "struct");
        assert_eq!(SymbolKind::Trait.to_string(), "trait");
        assert_eq!(SymbolKind::Impl.to_string(), "impl");
        assert_eq!(SymbolKind::Module.to_string(), "mod");
    }

    #[test]
    fn test_symbol_display_with_parent() {
        let sym = Symbol {
            name: "new".into(),
            kind: SymbolKind::Function,
            file: "src/id.rs".into(),
            span: Span {
                start: 0,
                end: 100,
                start_line: 0,
                end_line: 5,
            },
            is_public: true,
            parent: Some("AgentId".into()),
        };
        assert_eq!(sym.to_string(), "src/id.rs::AgentId::new");
    }

    #[test]
    fn test_symbol_display_without_parent() {
        let sym = Symbol {
            name: "AgentId".into(),
            kind: SymbolKind::Struct,
            file: "src/id.rs".into(),
            span: Span {
                start: 0,
                end: 50,
                start_line: 0,
                end_line: 3,
            },
            is_public: true,
            parent: None,
        };
        assert_eq!(sym.to_string(), "src/id.rs::AgentId");
    }

    #[test]
    fn test_symbol_serde_roundtrip() {
        let sym = Symbol {
            name: "evolve".into(),
            kind: SymbolKind::Function,
            file: "src/molecule.rs".into(),
            span: Span {
                start: 100,
                end: 200,
                start_line: 10,
                end_line: 20,
            },
            is_public: true,
            parent: Some("Molecule".into()),
        };
        let json = serde_json::to_string(&sym).unwrap();
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(sym, back);
    }

    #[test]
    fn test_symbol_ref_serde_roundtrip() {
        let r = SymbolRef {
            from: 0,
            to: 1,
            kind: RefKind::Calls,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: SymbolRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
