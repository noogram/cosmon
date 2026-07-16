// SPDX-License-Identifier: AGPL-3.0-only

//! Concrete [`crate::tool::Tool`] implementations.
//!
//! The v0 trio is `read_file`, `edit_file`, `exec_command`. The
//! local-research extension adds four more tools sharing the same
//! sandbox + truncation discipline:
//!
//! - [`edit_file`] — Aider-style exact-match search-and-replace.
//! - [`exec_command`] — persistent shell with sentinel-prompt
//!   protocol.
//! - [`list_dir`] — gitignore-aware directory listing.
//! - [`grep`] — in-process regex search across the worktree.
//! - [`find_file`] — gitignore-style glob over file names.
//! - [`write_file`] — create-only file writer; refuses to overwrite
//!   (the no-wholesale-rewrite rule is honoured by construction).
//!
//! `read_file` itself still lives in [`crate::tool`] for historical
//! reasons — it predated this submodule.
//!
//! Registration in [`crate::tool::default_registry`] is the
//! integration point — see that function's doc for the canonical
//! list of tools the harness whitelists.

pub mod await_operator;
pub mod edit_file;
pub mod exec_command;
pub mod find_file;
pub mod grep;
pub mod list_dir;
pub mod write_file;
