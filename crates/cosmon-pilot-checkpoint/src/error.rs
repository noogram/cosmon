// SPDX-License-Identifier: AGPL-3.0-only

//! Errors this crate can return.
//!
//! There is deliberately **no** error variant that a caller could mistake for
//! a verdict. Comparing two checkpoints never fails: an input the comparison
//! cannot read is rendered as `INCONCLUSIVE` inside the report, not as an
//! `Err` the caller is free to `unwrap_or(AGREE)`. Everything below is an I/O
//! or a validation fault of the *store*, which is a different concern.

use std::path::PathBuf;

use thiserror::Error;

/// A fault while validating an identifier or reading/writing the checkpoint
/// store.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CheckpointError {
    /// An identifier was empty or carried a byte outside its alphabet.
    ///
    /// Identifiers become path segments in the store, so a `/`, a `..` or a
    /// NUL in one is a directory-traversal bug waiting to happen; they are
    /// rejected at construction instead.
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(String),

    /// A checkpoint with this id already exists on disk.
    ///
    /// The store is append-only by construction: a published checkpoint is
    /// what a relief pilot resumes from (CHECKPOINT-NOT-SCROLLBACK), so
    /// silently rewriting one would change history under a reader that has
    /// already cited it.
    #[error("checkpoint {id} is already published at {path}")]
    AlreadyPublished {
        /// The checkpoint id that was being published.
        id: String,
        /// Where the existing record lives.
        path: PathBuf,
    },

    /// A file in the store could not be read or written.
    #[error("checkpoint store I/O at {path}: {source}")]
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A file in the store was not a checkpoint record.
    #[error("checkpoint store: {path} is not a valid checkpoint record: {source}")]
    Malformed {
        /// The offending file.
        path: PathBuf,
        /// The decoding failure.
        #[source]
        source: serde_json::Error,
    },

    /// A finding id could not be derived from its content.
    ///
    /// Only reachable if canonical serialisation of the finding's own fields
    /// fails, which would mean a non-serialisable value reached a record whose
    /// every field is a `String`, a number or a `Vec` of those.
    #[error("could not derive a content-addressed id: {0}")]
    Digest(String),
}
