// SPDX-License-Identifier: Apache-2.0

//! Error types for claudion.
//!
//! All fallible operations return [`ClaudionError`], which distinguishes I/O
//! failures from parse failures (with line-level granularity for JSONL debugging).

use std::path::PathBuf;

/// Errors produced by claudion operations.
#[derive(Debug, thiserror::Error)]
pub enum ClaudionError {
    /// Filesystem I/O failure (open, read, walk).
    #[error("I/O error on {path}: {source}")]
    Io {
        /// The path that triggered the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A JSONL line could not be parsed.
    #[error("JSON parse error at line {line}: {source}")]
    JsonParse {
        /// 1-based line number within the JSONL file.
        line: usize,
        /// The underlying serde error.
        source: serde_json::Error,
    },

    /// No session logs found under the given base path.
    #[error("no session logs found under {path}")]
    NoSessions {
        /// The base path that was searched.
        path: PathBuf,
    },

    /// A session ID could not be extracted from the filesystem path.
    #[error("invalid session path: {reason}")]
    InvalidPath {
        /// Why the path is invalid.
        reason: String,
    },
}
