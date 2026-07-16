// SPDX-License-Identifier: Apache-2.0

//! Topon: structural topology maps for code and knowledge vaults.
//!
//! Topon provides two modalities of structural context for agent systems:
//!
//! 1. **Code structural maps** — tree-sitter parses source into symbols
//!    (functions, structs, traits, impls), builds a reference graph, then
//!    ranks symbols by centrality using `PageRank`. The result is a compact,
//!    importance-ordered map that tells an agent *what matters most* in a
//!    codebase. Inspired by the Aider "repo map" pattern and validated by
//!    the RIG/SPADE paper (+12.2% accuracy, −57.8% cost with structural maps).
//!
//! 2. **Vault wikilink graphs** — parses `[[wikilinks]]` from Obsidian-style
//!    markdown files, forming a directed knowledge graph. `PageRank` on this graph
//!    surfaces the "hub" notes that connect the most concepts.
//!
//! # Quick start
//!
//! ```
//! use topon_core::project::map_project;
//! use std::path::Path;
//!
//! // Map an entire Rust project in one call.
//! let map = map_project(Path::new("."), None).unwrap();
//! println!("{}", map.to_markdown(0));
//! ```
//!
//! # Architecture
//!
//! - [`symbol`] — Code symbol types (function, struct, trait, etc.)
//! - [`graph`] — Symbol graph construction and `PageRank` ranking
//! - [`extract`] — tree-sitter based symbol extraction for Rust
//! - [`wikilink`] — Wikilink parser for Obsidian vault files
//! - [`render`] — Markdown + JSON structural map output
//! - [`walk`] — `.gitignore`-aware directory walking
//! - [`project`] — High-level façade: walk → parse → graph → rank → render
//! - [`error`] — Error types

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod error;
pub mod extract;
pub mod graph;
pub mod project;
pub mod render;
pub mod symbol;
pub mod walk;
pub mod wikilink;
