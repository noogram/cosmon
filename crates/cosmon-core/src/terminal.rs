// SPDX-License-Identifier: AGPL-3.0-only

//! Optional read-only terminal views, independent of lifecycle and UI clients.

use serde::{Deserialize, Serialize};

/// The optional view behavior a client may request; it grants no lifecycle authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCapability {
    /// A read-only client excluded from worker sizing and environment updates.
    ReadOnlyIgnoreSize,
}

/// A launch-bound terminal incarnation. Absence is never reconstructed from labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum TerminalIdentity {
    /// Local tmux server, session and sole pane witnessed at creation.
    Tmux {
        /// Exact socket path; names and ambient temporary directories are insufficient.
        socket_path: String,
        /// Server PID and platform launch fingerprint prevent server reuse.
        server_pid: u32,
        /// Opaque platform launch fingerprint of the server.
        server_start: u64,
        /// Numeric session target, stable for this server's lifetime.
        session_id: u32,
        /// Tmux's session creation timestamp, an additional consistency witness.
        session_created: u64,
        /// Numeric pane target, stable until pane destruction.
        pane_id: u32,
        /// Pane root process, which must still match the launch witness.
        pane_pid: u32,
        /// Opaque platform launch fingerprint of the pane root process.
        pane_start: u64,
    },
}

/// Why a read-only terminal capability cannot be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum TerminalUnavailable {
    /// No bound process exists on the molecule.
    #[error("no bound worker process")]
    NoProcess,
    /// A legacy or unsupported spawn did not record a launch-bound terminal witness.
    #[error("no launch-bound terminal identity; legacy and unsupported workers cannot attach")]
    IdentityUnknown,
    /// The terminal backend cannot be reached or does not support this view.
    #[error("terminal backend or read-only attachment capability unavailable")]
    BackendUnavailable,
    /// The current terminal no longer matches the recorded launch.
    #[error("terminal incarnation changed or worker exited")]
    IdentityChanged,
    /// A multi-window or multi-pane session cannot be offered as one exact worker view.
    #[error("terminal requires exactly one live window and pane")]
    AmbiguousTerminal,
}

/// An injectable terminal reader/attacher. It never sends input or changes lifecycle.
pub trait TerminalBackend {
    /// Revalidate a launch witness against the current external terminal.
    ///
    /// # Errors
    /// Refuses unknown, changed, ambiguous or unavailable external evidence.
    fn validate_terminal(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable>;

    /// Attach the invoking terminal read-only without influencing worker size.
    ///
    /// # Errors
    /// Refuses a changed identity or unsupported attachment. Adapters revalidate
    /// at execution as well as before constructing the launch command.
    fn attach_read_only(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable>;
}

/// Explicit input-capable attachment, separate from the output-only reader port.
/// No UI provider or lifecycle authority belongs to this capability.
pub trait TerminalInteractionBackend {
    /// Attach literal input/output to the exact launch; dimensions may resize it.
    /// Session environment updates must remain disabled.
    ///
    /// # Errors
    /// Refuses unavailable, ambiguous or changed launch witnesses.
    fn attach_interactive(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable>;
}
