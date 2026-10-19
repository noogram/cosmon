// SPDX-License-Identifier: AGPL-3.0-only

//! Per-worker routing for fleet probes. A pinned handle never reaches the fallback.

use cosmon_core::id::WorkerId;
use cosmon_core::process::MoleculeProcess;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};

/// Compose fleet probes with the durable bindings of existing workers.
/// The fallback exists solely for legacy records and unbound fleet workers.
pub struct ProcessTransport<'a> {
    fallback: &'a dyn TransportBackend,
    processes: Vec<&'a MoleculeProcess>,
}

impl<'a> ProcessTransport<'a> {
    /// Bind a read snapshot for one application operation, without looking at current config.
    pub fn new(
        fallback: &'a dyn TransportBackend,
        processes: impl IntoIterator<Item = &'a MoleculeProcess>,
    ) -> Self {
        Self {
            fallback,
            processes: processes
                .into_iter()
                .filter(|p| p.transport.is_some())
                .collect(),
        }
    }

    fn routed<T>(
        &self,
        worker: &WorkerId,
        operation: impl FnOnce(&dyn TransportBackend) -> Result<T, TransportError>,
    ) -> Result<T, TransportError> {
        if let Some(process) = self.processes.iter().find(|p| &p.worker_id == worker) {
            return operation(&crate::TmuxBackend::for_process(Some(process), "")?);
        }
        operation(self.fallback)
    }
}

impl TransportBackend for ProcessTransport<'_> {
    fn spawn(&self, _: &AgentDefinition, _: &RuntimeConfig) -> Result<SpawnHandle, TransportError> {
        Err(TransportError::SpawnFailed(
            "existing-process router cannot spawn".into(),
        ))
    }
    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
        self.routed(id, |b| b.terminate_session(id.as_str()))
    }
    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        self.routed(id, |b| b.is_alive(id))
    }
    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError> {
        self.routed(id, |b| b.send_input(id, input))
    }
    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        self.routed(id, |b| b.capture_output(id, lines))
    }
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        let mut sessions = self.fallback.list_sessions()?;
        sessions.retain(|s| !self.processes.iter().any(|p| p.worker_id == s.worker_id));
        for process in &self.processes {
            sessions.extend(crate::TmuxBackend::for_process(Some(process), "")?.list_sessions()?);
        }
        Ok(sessions)
    }
    fn graceful_exit(
        &self,
        id: &WorkerId,
        timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        self.routed(id, |b| b.graceful_exit(id, timeout))
    }
}
