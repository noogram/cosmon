// SPDX-License-Identifier: AGPL-3.0-only

//! Exhaustive error hierarchy for the Cosmon domain.
//!
//! Uses `thiserror` for `Display` derivation. The CLI boundary uses `anyhow`.

use crate::agent::DepthExceeded;
use crate::clearance::Clearance;
use crate::id::{AgentId, FormulaId, MoleculeId, SessionId, StepId, WorkerId};
use crate::molecule::MoleculeStatus;
use crate::worker::WorkerStatus;

/// Top-level error type for all Cosmon domain operations.
///
/// `#[non_exhaustive]` because this type is returned from nearly every
/// fallible operation in the crate and its variant set will grow. Without it,
/// the first new error variant added after publication would force a major
/// version bump (downstream `match` arms would no longer be exhaustive).
/// External callers must include a `_ => …` wildcard arm. (task-20260622-da94,
/// F-TOLNAY-2: "the first thing that breaks is `CosmonError`".)
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CosmonError {
    /// The referenced agent does not exist.
    #[error("agent not found: {0}")]
    AgentNotFound(AgentId),

    /// The referenced worker does not exist.
    #[error("worker not found: {0}")]
    WorkerNotFound(WorkerId),

    /// The referenced molecule does not exist.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),

    /// The referenced formula does not exist.
    #[error("formula not found: {0}")]
    FormulaNotFound(FormulaId),

    /// The referenced session does not exist.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    /// The referenced step does not exist within its molecule.
    #[error("step not found: {0} in molecule {1}")]
    StepNotFound(StepId, MoleculeId),

    /// A molecule state transition is not allowed.
    #[error("invalid transition for molecule {molecule}: {from} -> {to}")]
    InvalidTransition {
        /// The molecule that attempted the transition.
        molecule: MoleculeId,
        /// The current state.
        from: MoleculeStatus,
        /// The target state.
        to: MoleculeStatus,
    },

    /// A worker state transition is not allowed.
    #[error("invalid worker transition for {worker}: {from} -> {to}")]
    InvalidWorkerTransition {
        /// The worker that attempted the transition.
        worker: WorkerId,
        /// The current state.
        from: WorkerStatus,
        /// The target state.
        to: WorkerStatus,
    },

    /// An agent lacks sufficient clearance for the requested operation.
    #[error("clearance violation: agent {agent} has {actual}, requires {required}")]
    ClearanceViolation {
        /// The agent that attempted the operation.
        agent: AgentId,
        /// The minimum clearance needed.
        required: Clearance,
        /// The agent's actual clearance.
        actual: Clearance,
    },

    /// An agent spawn would exceed the maximum nesting depth.
    #[error(transparent)]
    DepthExceeded(#[from] DepthExceeded),

    /// A formula definition failed to parse.
    #[error("formula parse error: {reason}")]
    FormulaParse {
        /// Description of the parse failure.
        reason: String,
    },

    /// The state store backend encountered an error.
    #[error("state store error: {reason}")]
    StateStore {
        /// Description of the storage failure.
        reason: String,
    },

    /// The signal bus backend encountered an error.
    #[error("signal bus error: {reason}")]
    SignalBus {
        /// Description of the signal bus failure.
        reason: String,
    },

    /// A runtime error not covered by more specific variants.
    #[error("runtime error: {reason}")]
    Runtime {
        /// Description of the runtime failure.
        reason: String,
    },

    /// Failed to acquire a file lock (e.g. fleet.json.lock).
    #[error("lock failed on {path}: {reason}")]
    LockFailed {
        /// Path of the lock file.
        path: String,
        /// Description of the lock failure.
        reason: String,
    },

    /// An I/O error from the underlying filesystem or network.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A JSON serialization or deserialization error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_includes_context() {
        let agent = AgentId::new("witness").unwrap();
        let err = CosmonError::ClearanceViolation {
            agent: agent.clone(),
            required: Clearance::Execute,
            actual: Clearance::Read,
        };
        let msg = err.to_string();
        assert!(msg.contains("witness"), "should contain agent id: {msg}");
        assert!(msg.contains("execute"), "should contain required: {msg}");
        assert!(msg.contains("read"), "should contain actual: {msg}");

        let worker = WorkerId::new("ep-quartz").unwrap();
        let err = CosmonError::WorkerNotFound(worker);
        assert!(err.to_string().contains("ep-quartz"));

        let mol = MoleculeId::new("cs-20260401-hjdr").unwrap();
        let step = StepId::new("step-1").unwrap();
        let err = CosmonError::StepNotFound(step, mol);
        let msg = err.to_string();
        assert!(msg.contains("step-1"));
        assert!(msg.contains("cs-20260401-hjdr"));

        let err = CosmonError::InvalidTransition {
            molecule: MoleculeId::new("cs-20260401-abcd").unwrap(),
            from: MoleculeStatus::Completed,
            to: MoleculeStatus::Running,
        };
        let msg = err.to_string();
        assert!(msg.contains("completed"));
        assert!(msg.contains("running"));
    }

    #[test]
    fn test_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: CosmonError = io_err.into();
        assert!(matches!(err, CosmonError::Io(_)));
        assert!(err.to_string().contains("file missing"));
    }

    #[test]
    fn test_error_from_json() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err: CosmonError = json_err.into();
        assert!(matches!(err, CosmonError::Json(_)));
    }

    #[test]
    fn test_clearance_violation_message() {
        let err = CosmonError::ClearanceViolation {
            agent: AgentId::new("polecat-jasper").unwrap(),
            required: Clearance::Write,
            actual: Clearance::Read,
        };
        let msg = err.to_string();
        assert_eq!(
            msg,
            "clearance violation: agent polecat-jasper has read, requires write"
        );
    }

    #[test]
    fn test_formula_parse_error() {
        let err = CosmonError::FormulaParse {
            reason: "unexpected token at line 5".to_owned(),
        };
        assert!(err.to_string().contains("unexpected token at line 5"));
    }

    #[test]
    fn test_agent_not_found() {
        let err = CosmonError::AgentNotFound(AgentId::new("refinery").unwrap());
        assert_eq!(err.to_string(), "agent not found: refinery");
    }

    #[test]
    fn test_depth_exceeded_into_cosmon_error() {
        let depth_err = crate::agent::DepthExceeded { depth: 6 };
        let err: CosmonError = depth_err.into();
        assert!(matches!(err, CosmonError::DepthExceeded(_)));
        assert!(err.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        // io::Error and serde_json::Error are Send + Sync, so CosmonError should be too
        assert_send_sync::<CosmonError>();
    }
}
