// SPDX-License-Identifier: AGPL-3.0-only

//! In-memory mock transport backend for unit tests of higher layers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cosmon_core::id::WorkerId;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};

/// Recorded call to the mock backend (for assertions in tests).
#[derive(Debug, Clone)]
pub enum MockCall {
    Spawn { agent_id: String },
    Terminate { worker_id: String },
    IsAlive { worker_id: String },
    SendInput { worker_id: String, input: String },
    CaptureOutput { worker_id: String, lines: usize },
    ListSessions,
    GracefulExit { worker_id: String },
}

/// Mutable state shared across clones of a `MockBackend`.
#[derive(Debug, Default)]
struct MockState {
    sessions: HashMap<String, SessionInfo>,
    calls: Vec<MockCall>,
    /// Canned output returned by `capture_output`.
    canned_output: String,
    /// If set, `spawn` will return this error.
    spawn_error: Option<String>,
}

/// In-memory mock backend for testing higher layers without tmux.
///
/// # Usage
/// ```
/// use cosmon_transport::MockBackend;
/// let backend = MockBackend::new();
/// // Use `backend` as a `&dyn TransportBackend` in your tests.
/// assert!(backend.calls().is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    state: Arc<Mutex<MockState>>,
}

impl MockBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the canned output that `capture_output` will return.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn set_canned_output(&self, output: impl Into<String>) {
        self.state.lock().unwrap().canned_output = output.into();
    }

    /// Configure `spawn` to fail with the given error message.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn set_spawn_error(&self, msg: impl Into<String>) {
        self.state.lock().unwrap().spawn_error = Some(msg.into());
    }

    /// Return a snapshot of all recorded calls.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn calls(&self) -> Vec<MockCall> {
        self.state.lock().unwrap().calls.clone()
    }
}

impl TransportBackend for MockBackend {
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::Spawn {
            agent_id: agent.id.to_string(),
        });

        if let Some(ref msg) = state.spawn_error {
            return Err(TransportError::SpawnFailed(msg.clone()));
        }

        let worker_id = WorkerId::new(agent.id.as_str())
            .map_err(|e| TransportError::SpawnFailed(e.to_string()))?;

        let session_name = format!("{}{}", config.session_prefix, worker_id.name());

        let info = SessionInfo {
            worker_id: worker_id.clone(),
            session_name: session_name.clone(),
        };
        state.sessions.insert(worker_id.as_str().to_owned(), info);

        Ok(SpawnHandle {
            id: worker_id,
            session_name,
        })
    }

    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::Terminate {
            worker_id: id.to_string(),
        });

        state
            .sessions
            .remove(id.as_str())
            .ok_or_else(|| TransportError::NotFound(id.clone()))?;

        Ok(())
    }

    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::IsAlive {
            worker_id: id.to_string(),
        });

        Ok(state.sessions.contains_key(id.as_str()))
    }

    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::SendInput {
            worker_id: id.to_string(),
            input: input.to_owned(),
        });

        if !state.sessions.contains_key(id.as_str()) {
            return Err(TransportError::NotFound(id.clone()));
        }

        Ok(())
    }

    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::CaptureOutput {
            worker_id: id.to_string(),
            lines,
        });

        if !state.sessions.contains_key(id.as_str()) {
            return Err(TransportError::NotFound(id.clone()));
        }

        Ok(state.canned_output.clone())
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::ListSessions);

        Ok(state.sessions.values().cloned().collect())
    }

    fn graceful_exit(
        &self,
        id: &WorkerId,
        _timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        let mut state = self.state.lock().unwrap();

        state.calls.push(MockCall::GracefulExit {
            worker_id: id.to_string(),
        });

        state
            .sessions
            .remove(id.as_str())
            .ok_or_else(|| TransportError::NotFound(id.clone()))?;

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::agent::AgentRole;
    use cosmon_core::id::AgentId;

    fn test_agent() -> AgentDefinition {
        AgentDefinition {
            id: AgentId::new("test-worker").unwrap(),
            role: AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec!["hello".to_owned()],
        }
    }

    #[test]
    fn test_mock_spawn_and_lifecycle() {
        let backend = MockBackend::new();
        let config = RuntimeConfig::default();
        let agent = test_agent();

        let worker = backend.spawn(&agent, &config).expect("spawn");
        assert_eq!(worker.id.name(), "test-worker");

        assert!(backend.is_alive(&worker.id).unwrap());

        backend.terminate(&worker.id).expect("terminate");
        assert!(!backend.is_alive(&worker.id).unwrap());
    }

    #[test]
    fn test_mock_canned_output() {
        let backend = MockBackend::new();
        let config = RuntimeConfig::default();
        let agent = test_agent();

        backend.set_canned_output("hello world");
        let worker = backend.spawn(&agent, &config).unwrap();

        let output = backend.capture_output(&worker.id, 10).unwrap();
        assert_eq!(output, "hello world");
    }

    #[test]
    fn test_mock_spawn_error() {
        let backend = MockBackend::new();
        let config = RuntimeConfig::default();
        let agent = test_agent();

        backend.set_spawn_error("out of resources");
        let result = backend.spawn(&agent, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_mock_records_calls() {
        let backend = MockBackend::new();
        let config = RuntimeConfig::default();
        let agent = test_agent();

        let worker = backend.spawn(&agent, &config).unwrap();
        backend.is_alive(&worker.id).unwrap();
        backend.send_input(&worker.id, "test").unwrap();
        backend.list_sessions().unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 4);
        assert!(matches!(calls[0], MockCall::Spawn { .. }));
        assert!(matches!(calls[1], MockCall::IsAlive { .. }));
        assert!(matches!(calls[2], MockCall::SendInput { .. }));
        assert!(matches!(calls[3], MockCall::ListSessions));
    }

    #[test]
    fn test_mock_not_found_errors() {
        let backend = MockBackend::new();
        let missing = WorkerId::new("ghost").unwrap();

        assert!(backend.terminate(&missing).is_err());
        assert!(backend.send_input(&missing, "hello").is_err());
        assert!(backend.capture_output(&missing, 10).is_err());
    }

    #[test]
    fn test_mock_graceful_exit() {
        let backend = MockBackend::new();
        let config = RuntimeConfig::default();
        let agent = test_agent();

        let worker = backend.spawn(&agent, &config).unwrap();
        assert!(backend.is_alive(&worker.id).unwrap());

        let graceful = backend
            .graceful_exit(&worker.id, std::time::Duration::from_secs(1))
            .unwrap();
        assert!(graceful, "mock should report graceful exit");
        assert!(!backend.is_alive(&worker.id).unwrap());

        // Verify the call was recorded
        let calls = backend.calls();
        assert!(calls
            .iter()
            .any(|c| matches!(c, MockCall::GracefulExit { .. })));
    }

    #[test]
    fn test_mock_graceful_exit_not_found() {
        let backend = MockBackend::new();
        let missing = WorkerId::new("ghost").unwrap();

        assert!(backend
            .graceful_exit(&missing, std::time::Duration::from_secs(1))
            .is_err());
    }
}
