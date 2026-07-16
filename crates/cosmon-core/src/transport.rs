// SPDX-License-Identifier: AGPL-3.0-only

//! Transport backend trait — hexagonal port for agent lifecycle management.
//!
//! This module defines the `TransportBackend` trait (a hexagonal port) that
//! abstracts how worker agents are spawned, monitored, and terminated.
//! Concrete adapters (e.g. `TmuxBackend`) live outside this crate.

use serde::{Deserialize, Serialize};

use crate::agent::AgentRole;
use crate::id::{AgentId, WorkerId};

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Definition of an agent to spawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique agent identifier.
    pub id: AgentId,
    /// The agent's role within the rig.
    pub role: AgentRole,
    /// The executable command to run.
    pub command: String,
    /// Command-line arguments for the executable.
    pub args: Vec<String>,
}

/// Configuration for the transport runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Socket or namespace name (e.g. tmux socket). Default: `"cosmon"`.
    pub socket_name: String,
    /// Prefix for session names. Sessions are named `{prefix}{worker-name}`.
    pub session_prefix: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            socket_name: "cosmon".to_owned(),
            session_prefix: String::new(),
        }
    }
}

/// Handle to a spawned session returned by [`TransportBackend::spawn`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnHandle {
    /// The worker's unique identifier.
    pub id: WorkerId,
    /// The transport session name (e.g. tmux session).
    pub session_name: String,
}

/// Information about an active session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The worker that owns this session.
    pub worker_id: WorkerId,
    /// The transport session name.
    pub session_name: String,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors from transport operations.
///
/// `#[non_exhaustive]` — the hexagonal port admits new structural failure
/// modes (backend lost connection, capability negotiation refused, …)
/// without requiring a federation-wide major bump. External `match` sites
/// must carry a catch-all arm.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The transport backend failed to spawn a new worker session.
    #[error("spawn failed: {0}")]
    SpawnFailed(String),

    /// The requested worker does not exist in the transport backend.
    #[error("worker not found: {0}")]
    NotFound(WorkerId),

    /// An I/O error occurred in the transport layer.
    #[error("I/O error: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// Trait (hexagonal port)
// ---------------------------------------------------------------------------

/// Hexagonal port for agent lifecycle management.
///
/// Implementations handle the mechanics of spawning processes, sending input,
/// and capturing output. The domain layer programs against this trait without
/// knowing whether the backend is tmux, Docker, or a test mock.
pub trait TransportBackend {
    /// Spawn a new worker agent.
    ///
    /// # Errors
    /// Returns [`TransportError::SpawnFailed`] if the backend cannot create the session.
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError>;

    /// Terminate a running worker.
    ///
    /// # Errors
    /// Returns [`TransportError::NotFound`] if the worker does not exist.
    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError>;

    /// Check whether a worker is still alive.
    ///
    /// # Errors
    /// Returns [`TransportError::Io`] if the backend cannot determine status.
    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError>;

    /// Send text input to a worker's session.
    ///
    /// # Errors
    /// Returns [`TransportError::NotFound`] if the worker does not exist.
    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError>;

    /// Capture the last `lines` lines of output from a worker's session.
    ///
    /// # Errors
    /// Returns [`TransportError::NotFound`] if the worker does not exist.
    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError>;

    /// List all active sessions managed by this backend.
    ///
    /// # Errors
    /// Returns [`TransportError::Io`] if the backend cannot enumerate sessions.
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError>;

    /// Send a graceful exit signal and wait for the session to terminate.
    ///
    /// Returns `Ok(true)` if the session exited gracefully, `Ok(false)` if
    /// it had to be force-killed after timeout.
    ///
    /// # Errors
    /// Returns [`TransportError`] if both graceful exit and terminate fail.
    fn graceful_exit(
        &self,
        id: &WorkerId,
        timeout: std::time::Duration,
    ) -> Result<bool, TransportError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The trait must be object-safe so we can use `dyn TransportBackend`.
    #[test]
    fn test_trait_is_object_safe() {
        fn _accepts_dyn(_: &dyn TransportBackend) {}
    }
}
