// SPDX-License-Identifier: AGPL-3.0-only

//! Dispatch logic — assigning a molecule to a worker.
//!
//! Dispatch is the bridge between work that needs to be done (molecules) and
//! agents available to do it (workers). This module contains domain types and
//! validation logic with no filesystem or process I/O. It does read the wall
//! clock ([`chrono::Utc::now`]) when stamping a dispatch — that ambient-time
//! seam is covered by waiver W1 (INV-DOMAIN-PURE-NO-IO, ADR-082) until the
//! planned `Clock` injection lands.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::id::{MoleculeId, WorkerId};

/// Who or what initiated the dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchSource {
    /// A human operator dispatched manually.
    Human(String),
    /// The orchestrator (e.g. Mayor) dispatched automatically.
    Orchestrator,
    /// Auto-detection assigned idle workers to pending molecules.
    AutoDetect,
}

impl fmt::Display for DispatchSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Human(name) => write!(f, "human:{name}"),
            Self::Orchestrator => f.write_str("orchestrator"),
            Self::AutoDetect => f.write_str("auto-detect"),
        }
    }
}

/// A record of a molecule being assigned to a worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    /// The molecule being dispatched.
    pub molecule: MoleculeId,
    /// The worker receiving the assignment.
    pub worker: WorkerId,
    /// When the dispatch occurred.
    pub dispatched_at: DateTime<Utc>,
    /// Who or what triggered the dispatch.
    pub dispatched_by: DispatchSource,
}

/// Errors specific to the dispatch process.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// The worker is not in a state that can accept work.
    #[error("worker {0} is not active")]
    WorkerNotActive(WorkerId),

    /// The worker already has an assigned molecule.
    #[error("worker {0} already has molecule {1} assigned")]
    WorkerBusy(WorkerId, MoleculeId),

    /// The molecule is not in a dispatchable state.
    #[error("molecule {0} is not active")]
    MoleculeNotActive(MoleculeId),
}

/// Input for creating a dispatch record.
pub struct DispatchRequest {
    /// The molecule to assign.
    pub molecule: MoleculeId,
    /// The target worker.
    pub worker: WorkerId,
    /// Who initiated the dispatch.
    pub source: DispatchSource,
}

/// Create a dispatch record binding a molecule to a worker.
///
/// It validates the request and produces a [`Dispatch`] record, stamping
/// `dispatched_at` from the wall clock ([`Utc::now`]) — so it is *not* a pure
/// function (the read of ambient time is waiver W1, ADR-082; a future `Clock`
/// parameter would restore purity). Callers are responsible for persisting the
/// result and updating fleet/molecule state.
#[must_use]
pub fn dispatch(req: DispatchRequest) -> Dispatch {
    let now = Utc::now();
    Dispatch {
        molecule: req.molecule,
        worker: req.worker,
        dispatched_at: now,
        dispatched_by: req.source,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_creates_record() {
        let mol_id = MoleculeId::new("cs-20260401-abcd").unwrap();
        let worker_id = WorkerId::new("quartz").unwrap();

        let result = dispatch(DispatchRequest {
            molecule: mol_id.clone(),
            worker: worker_id.clone(),
            source: DispatchSource::Orchestrator,
        });

        assert_eq!(result.molecule, mol_id);
        assert_eq!(result.worker, worker_id);
        assert_eq!(result.dispatched_by, DispatchSource::Orchestrator);
    }

    #[test]
    fn test_dispatch_source_display() {
        assert_eq!(
            DispatchSource::Human("mayor".to_owned()).to_string(),
            "human:mayor"
        );
        assert_eq!(DispatchSource::Orchestrator.to_string(), "orchestrator");
        assert_eq!(DispatchSource::AutoDetect.to_string(), "auto-detect");
    }

    #[test]
    fn test_dispatch_source_human() {
        let mol_id = MoleculeId::new("cs-20260401-efgh").unwrap();
        let worker_id = WorkerId::new("ep-jasper").unwrap();

        let result = dispatch(DispatchRequest {
            molecule: mol_id,
            worker: worker_id,
            source: DispatchSource::Human("admin".to_owned()),
        });

        assert_eq!(
            result.dispatched_by,
            DispatchSource::Human("admin".to_owned())
        );
    }

    #[test]
    fn test_dispatch_json_roundtrip() {
        let d = Dispatch {
            molecule: MoleculeId::new("cs-20260401-abcd").unwrap(),
            worker: WorkerId::new("quartz").unwrap(),
            dispatched_at: Utc::now(),
            dispatched_by: DispatchSource::Orchestrator,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: Dispatch = serde_json::from_str(&json).unwrap();
        assert_eq!(d.molecule, back.molecule);
        assert_eq!(d.worker, back.worker);
        assert_eq!(d.dispatched_by, back.dispatched_by);
    }

    #[test]
    fn test_dispatch_error_display() {
        let err = DispatchError::WorkerBusy(
            WorkerId::new("quartz").unwrap(),
            MoleculeId::new("cs-20260401-abcd").unwrap(),
        );
        assert!(err.to_string().contains("quartz"));
        assert!(err.to_string().contains("cs-20260401-abcd"));
    }
}
