// SPDX-License-Identifier: Apache-2.0

//! Error types for the CFS crate.

use std::path::PathBuf;

/// Errors from CFS operations.
#[derive(Debug, thiserror::Error)]
pub enum CfsError {
    /// Failed to read a source file.
    #[error("failed to read {path}: {source}")]
    ReadFile {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// tree-sitter failed to parse a source file.
    #[error("parse failed for {0}")]
    ParseFailed(PathBuf),

    /// `PageRank` did not converge within the iteration limit.
    #[error("PageRank did not converge after {0} iterations")]
    RankNotConverged(usize),
}
