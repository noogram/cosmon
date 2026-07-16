// SPDX-License-Identifier: AGPL-3.0-only

//! Bridge layer for Claude Code agents.
//!
//! Provides typed Rust wrappers over Gas Town CLI tools (`bd`) so that
//! Cosmon transport and orchestration code can interact with the beads
//! issue tracker without shelling out directly.
//!
//! All operations are standalone functions (not traits) per ADR-COS-001.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod beads;
pub mod entropy;
pub mod llm;

pub use llm::AnthropicSubprocess;
