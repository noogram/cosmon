// SPDX-License-Identifier: AGPL-3.0-only

//! Durable worker-host composition, independent of current operator configuration.

use cosmon_core::id::WorkerId;
use cosmon_core::terminal::TerminalIdentity;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};
use cosmon_core::transport_selection::TransportKind;

/// Qualified host implementations. A recorded pin never falls back across hosts.
pub enum WorkerHost {
    /// Existing tmux implementation, including legacy workers.
    Tmux(crate::TmuxBackend),
    /// Herdr protocol 22 with supervisor-owned stop authority.
    #[cfg(unix)]
    Herdr(crate::herdr::HerdrBackend),
}
impl WorkerHost {
    /// Compose a new dispatch from its explicit host and socket.
    ///
    /// # Errors
    /// Refuses unavailable platform support.
    #[allow(clippy::unnecessary_wraps)] // Non-Unix builds must return Unsupported.
    pub fn new(kind: TransportKind, socket: &str) -> Result<Self, TransportError> {
        match kind {
            TransportKind::Tmux => Ok(Self::Tmux(crate::TmuxBackend::new(socket))),
            #[cfg(unix)]
            TransportKind::Herdr => Ok(Self::Herdr(crate::herdr::HerdrBackend::new(socket))),
            #[cfg(not(unix))]
            TransportKind::Herdr => Err(TransportError::Unsupported(
                "Herdr requires Unix sockets".into(),
            )),
        }
    }
    /// Recompose an existing dispatch solely from its saved pin and terminal witness.
    ///
    /// # Errors
    /// Refuses missing or inconsistent identity; never reconstructs a name.
    pub fn for_process(
        process: Option<&cosmon_core::process::MoleculeProcess>,
        legacy_socket: &str,
    ) -> Result<Self, TransportError> {
        let Some(process) = process.filter(|p| p.transport.is_some()) else {
            return Self::new(TransportKind::Tmux, legacy_socket);
        };
        let kind = process.transport.map(|selection| selection.backend);
        match (kind, process.terminal.as_ref()) {
            (Some(TransportKind::Tmux), Some(TerminalIdentity::Tmux { .. })) => Ok(Self::Tmux(
                crate::TmuxBackend::for_process(Some(process), "")?,
            )),
            #[cfg(unix)]
            (
                Some(TransportKind::Herdr),
                Some(identity @ TerminalIdentity::Herdr { socket_path, .. }),
            ) if process.worker_launch.is_some() => {
                Ok(Self::Herdr(crate::herdr::HerdrBackend::bound(
                    socket_path.clone(),
                    crate::herdr::SocketClient::new(socket_path),
                    process.worker_id.to_string(),
                    identity.clone(),
                )))
            }
            _ => Err(TransportError::Io(
                "pinned host has missing or inconsistent creation identity; no fallback permitted"
                    .into(),
            )),
        }
    }
    /// Socket label for reporting; it carries no stop authority.
    pub fn socket(&self) -> &str {
        match self {
            Self::Tmux(backend) => backend.socket(),
            #[cfg(unix)]
            Self::Herdr(_) => "herdr",
        }
    }
    fn backend(&self) -> &dyn TransportBackend {
        match self {
            Self::Tmux(backend) => backend,
            #[cfg(unix)]
            Self::Herdr(backend) => backend,
        }
    }
}
impl TransportBackend for WorkerHost {
    fn supports_command_sessions(&self) -> bool {
        self.backend().supports_command_sessions()
    }
    fn spawn_command(
        &self,
        session: &str,
        directory: &str,
        command: &str,
        environment: &[(String, String)],
    ) -> Result<Option<TerminalIdentity>, TransportError> {
        self.backend()
            .spawn_command(session, directory, command, environment)
    }
    fn spawn_argv(
        &self,
        session: &str,
        directory: &str,
        argv: &[String],
        environment: &[(String, String)],
    ) -> Result<Option<TerminalIdentity>, TransportError> {
        self.backend()
            .spawn_argv(session, directory, argv, environment)
    }
    fn install_exit_hook(&self, session: &str, command: &str) -> Result<(), TransportError> {
        self.backend().install_exit_hook(session, command)
    }
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError> {
        self.backend().spawn(agent, config)
    }
    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
        self.backend().terminate(id)
    }
    fn terminate_session(&self, session: &str) -> Result<(), TransportError> {
        self.backend().terminate_session(session)
    }
    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        self.backend().is_alive(id)
    }
    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError> {
        self.backend().send_input(id, input)
    }
    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        self.backend().capture_output(id, lines)
    }
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        self.backend().list_sessions()
    }
    fn session_exists(&self, session: &str) -> Result<bool, TransportError> {
        self.backend().session_exists(session)
    }
    fn graceful_exit(
        &self,
        id: &WorkerId,
        timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        self.backend().graceful_exit(id, timeout)
    }
}
