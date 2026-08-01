// SPDX-License-Identifier: Apache-2.0

//! Errors of the session-probe port.
//!
//! The port is deliberately hard to make fail. Observing a live log is a
//! best-effort measurement, and a probe that returns `Err` because one line of
//! a 8 440-line rollout was half-written is a probe that cannot watch a live
//! session at all (probe P7 of ADR-168). So a malformed *complete* line is
//! counted, not raised; a partial trailing line is left for the next read; and
//! only a genuine I/O fault or a malformed identifier is an error.

use std::path::PathBuf;

/// Anything the port can refuse to do.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProbeError {
    /// The filesystem refused a read. Carries the path so the operator can
    /// see *which* log is unreadable rather than that "a" log is.
    #[error("session probe I/O error at {path}: {source}")]
    Io {
        /// The path being read when the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A provider name or native session id did not satisfy its newtype
    /// invariant. Keys of the protocol are validated at construction because
    /// FAIL-CLOSED-AUTHORITY forbids an unparseable identity from becoming a
    /// default one downstream.
    #[error("invalid session identifier: {0}")]
    InvalidIdentifier(String),

    /// A `<provider>:<native-session-id>` selector could not be parsed.
    #[error("invalid session selector {input:?}: {reason}")]
    InvalidSelector {
        /// The text the caller supplied.
        input: String,
        /// Why it is not a selector.
        reason: &'static str,
    },
}
