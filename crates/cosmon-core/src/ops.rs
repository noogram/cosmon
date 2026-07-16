// SPDX-License-Identifier: AGPL-3.0-only

//! Event-sourced operational state model (ADR-GOV-006).
//!
//! Agent actions are **commands** that produce **events**. Current state is a
//! **projection** derived by folding the event log. No direct state mutation —
//! every change goes through command → validate → event → project.
//!
//! This provides:
//! - Full audit trail of every state change
//! - Replay/debugging: reconstruct any historical state by replaying a prefix
//! - Testability: projections are pure functions over event sequences
//!
//! # Architecture
//!
//! ```text
//! ┌─────────┐    validate    ┌─────────┐    persist    ┌────────────┐
//! │ Command │───────────────▶│  Event  │──────────────▶│ EventStore │
//! └─────────┘                └─────────┘               └────────────┘
//!                                 │                          │
//!                                 │ apply                    │ replay
//!                                 ▼                          ▼
//!                            ┌──────────┐              ┌──────────┐
//!                            │ OpsState │◀─────────────│ OpsState │
//!                            └──────────┘   fold       └──────────┘
//! ```
//!
//! # Examples
//!
//! ```
//! use cosmon_core::ops::{OpsCommand, OpsEvent, OpsState};
//! use cosmon_core::id::{AgentId, WorkerId};
//!
//! let mut state = OpsState::default();
//!
//! // Command produces an event after validation:
//! let cmd = OpsCommand::SpawnWorker {
//!     worker_id: WorkerId::new("quartz").unwrap(),
//!     agent_id: AgentId::new("polecat").unwrap(),
//! };
//! let event = state.decide(cmd).unwrap();
//!
//! // Event is applied to produce new state:
//! state.apply(&event);
//!
//! assert_eq!(state.workers().len(), 1);
//! assert!(state.worker(&WorkerId::new("quartz").unwrap()).is_some());
//! ```

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CosmonError;
use crate::id::{AgentId, MoleculeId, WorkerId};
use crate::molecule::MoleculeStatus;
use crate::worker::WorkerStatus;

// ---------------------------------------------------------------------------
// Commands — what agents request
// ---------------------------------------------------------------------------

/// A command from an agent requesting a state change.
///
/// Commands are validated against current state before producing events.
/// Invalid commands return errors — they never produce events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum OpsCommand {
    /// Request to spawn a new worker.
    SpawnWorker {
        /// The worker to create.
        worker_id: WorkerId,
        /// The agent definition it instantiates.
        agent_id: AgentId,
    },

    /// Request to terminate a worker.
    TerminateWorker {
        /// The worker to terminate.
        worker_id: WorkerId,
        /// Reason for termination.
        reason: String,
    },

    /// Request to assign a molecule to a worker.
    AssignMolecule {
        /// The molecule to assign.
        molecule_id: MoleculeId,
        /// The worker to assign it to.
        worker_id: WorkerId,
    },

    /// Request to transition a molecule to a new status.
    TransitionMolecule {
        /// The molecule to transition.
        molecule_id: MoleculeId,
        /// The target status.
        to: MoleculeStatus,
        /// Optional reason (for collapse).
        reason: Option<String>,
    },

    /// Request to complete a molecule step.
    CompleteStep {
        /// The molecule.
        molecule_id: MoleculeId,
        /// Zero-based step index that was completed.
        step: usize,
        /// Total steps in the molecule.
        total: usize,
    },

    /// Request to update a worker's status.
    UpdateWorkerStatus {
        /// The worker to update.
        worker_id: WorkerId,
        /// The new status.
        status: WorkerStatus,
    },
}

impl OpsCommand {
    /// The agent (worker) that issued this command, if identifiable.
    #[must_use]
    pub fn actor(&self) -> Option<&WorkerId> {
        match self {
            Self::TerminateWorker { worker_id, .. }
            | Self::UpdateWorkerStatus { worker_id, .. }
            | Self::AssignMolecule { worker_id, .. } => Some(worker_id),
            Self::SpawnWorker { .. }
            | Self::TransitionMolecule { .. }
            | Self::CompleteStep { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Events — facts that happened
// ---------------------------------------------------------------------------

/// An event recording a state change that occurred.
///
/// Events are immutable facts. Once persisted, they are never modified or
/// deleted. The current state is always derivable by replaying all events
/// in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpsEvent {
    /// A worker was spawned.
    WorkerSpawned {
        /// The worker's identity.
        worker_id: WorkerId,
        /// The agent definition it instantiates.
        agent_id: AgentId,
        /// When spawned.
        at: DateTime<Utc>,
    },

    /// A worker was terminated.
    WorkerTerminated {
        /// The worker's identity.
        worker_id: WorkerId,
        /// Why it terminated.
        reason: String,
        /// When terminated.
        at: DateTime<Utc>,
    },

    /// A worker's status changed.
    WorkerStatusChanged {
        /// The worker's identity.
        worker_id: WorkerId,
        /// Previous status.
        from: WorkerStatus,
        /// New status.
        to: WorkerStatus,
        /// When the change occurred.
        at: DateTime<Utc>,
    },

    /// A molecule was assigned to a worker.
    MoleculeAssigned {
        /// The molecule.
        molecule_id: MoleculeId,
        /// The worker.
        worker_id: WorkerId,
        /// When assigned.
        at: DateTime<Utc>,
    },

    /// A molecule transitioned to a new status.
    MoleculeTransitioned {
        /// The molecule.
        molecule_id: MoleculeId,
        /// Previous status.
        from: MoleculeStatus,
        /// New status.
        to: MoleculeStatus,
        /// Optional reason (for collapse).
        reason: Option<String>,
        /// When transitioned.
        at: DateTime<Utc>,
    },

    /// A molecule step was completed.
    StepCompleted {
        /// The molecule.
        molecule_id: MoleculeId,
        /// Zero-based step index.
        step: usize,
        /// Total steps.
        total: usize,
        /// When completed.
        at: DateTime<Utc>,
    },
}

impl OpsEvent {
    /// When this event occurred.
    #[must_use]
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Self::WorkerSpawned { at, .. }
            | Self::WorkerTerminated { at, .. }
            | Self::WorkerStatusChanged { at, .. }
            | Self::MoleculeAssigned { at, .. }
            | Self::MoleculeTransitioned { at, .. }
            | Self::StepCompleted { at, .. } => *at,
        }
    }
}

// ---------------------------------------------------------------------------
// Event envelope — timestamped + sequenced wrapper
// ---------------------------------------------------------------------------

/// A sequenced, timestamped event in the ops log.
///
/// The `sequence` is a monotonically increasing counter assigned by the
/// event store. It provides a total order over events independent of
/// wall-clock time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpsEnvelope {
    /// Monotonically increasing sequence number.
    pub sequence: u64,
    /// The event payload.
    pub event: OpsEvent,
}

// ---------------------------------------------------------------------------
// Projection — derived state views
// ---------------------------------------------------------------------------

/// A projected view of a single worker, derived from events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerView {
    /// The worker's identity.
    pub id: WorkerId,
    /// The agent definition it instantiates.
    pub agent_id: AgentId,
    /// Current status.
    pub status: WorkerStatus,
    /// Currently assigned molecule, if any.
    pub current_molecule: Option<MoleculeId>,
    /// When spawned.
    pub spawned_at: DateTime<Utc>,
    /// Last event timestamp for this worker.
    pub updated_at: DateTime<Utc>,
}

/// A projected view of a single molecule, derived from events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoleculeView {
    /// The molecule's identity.
    pub id: MoleculeId,
    /// Current lifecycle status.
    pub status: MoleculeStatus,
    /// Assigned worker, if any.
    pub assigned_worker: Option<WorkerId>,
    /// Current step index.
    pub current_step: usize,
    /// Total steps.
    pub total_steps: usize,
    /// Last event timestamp for this molecule.
    pub updated_at: DateTime<Utc>,
}

/// The complete operational state, projected from the event log.
///
/// This is a pure fold over the event stream. Given the same events in
/// the same order, the same `OpsState` is produced — deterministic and
/// reproducible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpsState {
    workers: HashMap<WorkerId, WorkerView>,
    molecules: HashMap<MoleculeId, MoleculeView>,
    /// Sequence of the last applied event.
    last_sequence: u64,
}

impl OpsState {
    /// Validate a command against current state and produce an event.
    ///
    /// This is the "decide" step in the command → event cycle. The command
    /// is checked for precondition violations (duplicate IDs, missing
    /// entities, invalid transitions). If valid, an event is returned. If
    /// invalid, an error is returned and no state changes.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] if the command violates a precondition.
    pub fn decide(&self, command: OpsCommand) -> Result<OpsEvent, CosmonError> {
        let now = Utc::now();
        match command {
            OpsCommand::SpawnWorker {
                worker_id,
                agent_id,
            } => {
                if self.workers.contains_key(&worker_id) {
                    return Err(CosmonError::Runtime {
                        reason: format!("worker already exists: {worker_id}"),
                    });
                }
                Ok(OpsEvent::WorkerSpawned {
                    worker_id,
                    agent_id,
                    at: now,
                })
            }

            OpsCommand::TerminateWorker { worker_id, reason } => {
                if !self.workers.contains_key(&worker_id) {
                    return Err(CosmonError::WorkerNotFound(worker_id));
                }
                Ok(OpsEvent::WorkerTerminated {
                    worker_id,
                    reason,
                    at: now,
                })
            }

            OpsCommand::UpdateWorkerStatus { worker_id, status } => {
                let worker = self
                    .workers
                    .get(&worker_id)
                    .ok_or_else(|| CosmonError::WorkerNotFound(worker_id.clone()))?;
                let from = worker.status.clone();
                Ok(OpsEvent::WorkerStatusChanged {
                    worker_id,
                    from,
                    to: status,
                    at: now,
                })
            }

            OpsCommand::AssignMolecule {
                molecule_id,
                worker_id,
            } => {
                if !self.workers.contains_key(&worker_id) {
                    return Err(CosmonError::WorkerNotFound(worker_id));
                }
                Ok(OpsEvent::MoleculeAssigned {
                    molecule_id,
                    worker_id,
                    at: now,
                })
            }

            OpsCommand::TransitionMolecule {
                molecule_id,
                to,
                reason,
            } => {
                let mol = self
                    .molecules
                    .get(&molecule_id)
                    .ok_or_else(|| CosmonError::MoleculeNotFound(molecule_id.clone()))?;
                let from = mol.status;
                if !from.can_transition_to(to) {
                    return Err(CosmonError::InvalidTransition {
                        molecule: molecule_id,
                        from,
                        to,
                    });
                }
                Ok(OpsEvent::MoleculeTransitioned {
                    molecule_id,
                    from,
                    to,
                    reason,
                    at: now,
                })
            }

            OpsCommand::CompleteStep {
                molecule_id,
                step,
                total,
            } => Ok(OpsEvent::StepCompleted {
                molecule_id,
                step,
                total,
                at: now,
            }),
        }
    }

    /// Apply an event to update the projection.
    ///
    /// This is a pure state transition — no I/O, no validation. The event
    /// is assumed to be valid (it was produced by `decide` or loaded from
    /// a trusted event store).
    pub fn apply(&mut self, event: &OpsEvent) {
        match event {
            OpsEvent::WorkerSpawned {
                worker_id,
                agent_id,
                at,
            } => {
                self.workers.insert(
                    worker_id.clone(),
                    WorkerView {
                        id: worker_id.clone(),
                        agent_id: agent_id.clone(),
                        status: WorkerStatus::Starting,
                        current_molecule: None,
                        spawned_at: *at,
                        updated_at: *at,
                    },
                );
            }

            OpsEvent::WorkerTerminated { worker_id, at, .. } => {
                if let Some(worker) = self.workers.get_mut(worker_id) {
                    worker.status = WorkerStatus::Stopped;
                    worker.updated_at = *at;
                }
            }

            OpsEvent::WorkerStatusChanged {
                worker_id, to, at, ..
            } => {
                if let Some(worker) = self.workers.get_mut(worker_id) {
                    worker.status = to.clone();
                    worker.updated_at = *at;
                }
            }

            OpsEvent::MoleculeAssigned {
                molecule_id,
                worker_id,
                at,
            } => {
                // Create or update the molecule view.
                let mol = self
                    .molecules
                    .entry(molecule_id.clone())
                    .or_insert_with(|| MoleculeView {
                        id: molecule_id.clone(),
                        status: MoleculeStatus::Running,
                        assigned_worker: None,
                        current_step: 0,
                        total_steps: 0,
                        updated_at: *at,
                    });
                mol.assigned_worker = Some(worker_id.clone());
                mol.updated_at = *at;

                // Update the worker's current molecule.
                if let Some(worker) = self.workers.get_mut(worker_id) {
                    worker.current_molecule = Some(molecule_id.clone());
                    worker.updated_at = *at;
                }
            }

            OpsEvent::MoleculeTransitioned {
                molecule_id,
                to,
                at,
                ..
            } => {
                if let Some(mol) = self.molecules.get_mut(molecule_id) {
                    mol.status = *to;
                    mol.updated_at = *at;
                }
            }

            OpsEvent::StepCompleted {
                molecule_id,
                step,
                total,
                at,
            } => {
                let mol = self
                    .molecules
                    .entry(molecule_id.clone())
                    .or_insert_with(|| MoleculeView {
                        id: molecule_id.clone(),
                        status: MoleculeStatus::Running,
                        assigned_worker: None,
                        current_step: 0,
                        total_steps: *total,
                        updated_at: *at,
                    });
                mol.current_step = *step;
                mol.total_steps = *total;
                mol.updated_at = *at;
            }
        }
    }

    /// Apply a sequenced envelope, updating both projection and sequence counter.
    pub fn apply_envelope(&mut self, envelope: &OpsEnvelope) {
        self.apply(&envelope.event);
        self.last_sequence = envelope.sequence;
    }

    /// Rebuild state from a sequence of events (replay).
    ///
    /// This is the fundamental operation of event sourcing: given a
    /// complete, ordered event log, produce the current state.
    #[must_use]
    pub fn replay(events: &[OpsEvent]) -> Self {
        let mut state = Self::default();
        for event in events {
            state.apply(event);
        }
        state
    }

    /// Rebuild state from sequenced envelopes.
    #[must_use]
    pub fn replay_envelopes(envelopes: &[OpsEnvelope]) -> Self {
        let mut state = Self::default();
        for envelope in envelopes {
            state.apply_envelope(envelope);
        }
        state
    }

    // --- Read accessors (projections are read-only outside this module) ---

    /// All known workers.
    #[must_use]
    pub fn workers(&self) -> &HashMap<WorkerId, WorkerView> {
        &self.workers
    }

    /// Look up a single worker.
    #[must_use]
    pub fn worker(&self, id: &WorkerId) -> Option<&WorkerView> {
        self.workers.get(id)
    }

    /// All known molecules.
    #[must_use]
    pub fn molecules(&self) -> &HashMap<MoleculeId, MoleculeView> {
        &self.molecules
    }

    /// Look up a single molecule.
    #[must_use]
    pub fn molecule(&self, id: &MoleculeId) -> Option<&MoleculeView> {
        self.molecules.get(id)
    }

    /// Active workers (status is Active).
    #[must_use]
    pub fn active_workers(&self) -> Vec<&WorkerView> {
        self.workers
            .values()
            .filter(|w| w.status == WorkerStatus::Active)
            .collect()
    }

    /// Sequence number of the last applied event.
    #[must_use]
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
}

// ---------------------------------------------------------------------------
// EventStore — hexagonal port for event persistence
// ---------------------------------------------------------------------------

/// Hexagonal port for persisting and retrieving operational events.
///
/// Implementations provide the actual storage backend (JSONL files,
/// database, etc.). The trait is object-safe for use as `dyn OpsEventStore`.
pub trait OpsEventStore {
    /// Append an event to the log, returning the assigned sequence number.
    ///
    /// The implementation MUST assign a monotonically increasing sequence
    /// number. Events are immutable once appended.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the append fails.
    fn append(&self, event: &OpsEvent) -> Result<OpsEnvelope, CosmonError>;

    /// Read all events from the log.
    ///
    /// # Errors
    /// Returns [`CosmonError`] on I/O failure.
    fn read_all(&self) -> Result<Vec<OpsEnvelope>, CosmonError>;

    /// Read events starting after the given sequence number.
    ///
    /// # Errors
    /// Returns [`CosmonError`] on I/O failure.
    fn read_since(&self, after_sequence: u64) -> Result<Vec<OpsEnvelope>, CosmonError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_id(name: &str) -> WorkerId {
        WorkerId::new(name).unwrap()
    }

    fn agent_id(name: &str) -> AgentId {
        AgentId::new(name).unwrap()
    }

    fn molecule_id(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    // -- Command → Event → State cycle --

    #[test]
    fn test_spawn_worker_produces_event_and_updates_state() {
        let mut state = OpsState::default();
        let cmd = OpsCommand::SpawnWorker {
            worker_id: worker_id("quartz"),
            agent_id: agent_id("polecat"),
        };

        let event = state.decide(cmd).unwrap();
        assert!(matches!(event, OpsEvent::WorkerSpawned { .. }));

        state.apply(&event);
        assert_eq!(state.workers().len(), 1);
        let w = state.worker(&worker_id("quartz")).unwrap();
        assert_eq!(w.agent_id.as_str(), "polecat");
        assert_eq!(w.status, WorkerStatus::Starting);
    }

    #[test]
    fn test_spawn_duplicate_worker_rejected() {
        let mut state = OpsState::default();
        let cmd = OpsCommand::SpawnWorker {
            worker_id: worker_id("quartz"),
            agent_id: agent_id("polecat"),
        };
        let event = state.decide(cmd.clone()).unwrap();
        state.apply(&event);

        // Second spawn of same worker should fail.
        let result = state.decide(cmd);
        assert!(result.is_err());
    }

    #[test]
    fn test_terminate_worker_updates_status() {
        let mut state = OpsState::default();

        // Spawn.
        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        // Terminate.
        let event = state
            .decide(OpsCommand::TerminateWorker {
                worker_id: worker_id("quartz"),
                reason: "session ended".to_owned(),
            })
            .unwrap();
        state.apply(&event);

        let w = state.worker(&worker_id("quartz")).unwrap();
        assert_eq!(w.status, WorkerStatus::Stopped);
    }

    #[test]
    fn test_terminate_nonexistent_worker_rejected() {
        let state = OpsState::default();
        let result = state.decide(OpsCommand::TerminateWorker {
            worker_id: worker_id("ghost"),
            reason: "gone".to_owned(),
        });
        assert!(matches!(result, Err(CosmonError::WorkerNotFound(_))));
    }

    #[test]
    fn test_update_worker_status() {
        let mut state = OpsState::default();

        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        let event = state
            .decide(OpsCommand::UpdateWorkerStatus {
                worker_id: worker_id("quartz"),
                status: WorkerStatus::Active,
            })
            .unwrap();
        state.apply(&event);

        let w = state.worker(&worker_id("quartz")).unwrap();
        assert_eq!(w.status, WorkerStatus::Active);
    }

    #[test]
    fn test_assign_molecule_to_worker() {
        let mut state = OpsState::default();

        // Spawn worker.
        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        // Assign molecule.
        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::AssignMolecule {
                molecule_id: mol_id.clone(),
                worker_id: worker_id("quartz"),
            })
            .unwrap();
        state.apply(&event);

        // Worker has the molecule.
        let w = state.worker(&worker_id("quartz")).unwrap();
        assert_eq!(
            w.current_molecule.as_ref().unwrap().as_str(),
            "cs-20260401-abcd"
        );

        // Molecule exists with worker assigned.
        let m = state.molecule(&mol_id).unwrap();
        assert_eq!(m.assigned_worker.as_ref().unwrap().as_str(), "quartz");
        assert_eq!(m.status, MoleculeStatus::Running);
    }

    #[test]
    fn test_assign_molecule_to_nonexistent_worker_rejected() {
        let state = OpsState::default();
        let result = state.decide(OpsCommand::AssignMolecule {
            molecule_id: molecule_id("cs-20260401-abcd"),
            worker_id: worker_id("ghost"),
        });
        assert!(matches!(result, Err(CosmonError::WorkerNotFound(_))));
    }

    #[test]
    fn test_transition_molecule() {
        let mut state = OpsState::default();

        // Spawn and assign.
        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::AssignMolecule {
                molecule_id: mol_id.clone(),
                worker_id: worker_id("quartz"),
            })
            .unwrap();
        state.apply(&event);

        // Transition to completed.
        let event = state
            .decide(OpsCommand::TransitionMolecule {
                molecule_id: mol_id.clone(),
                to: MoleculeStatus::Completed,
                reason: None,
            })
            .unwrap();
        state.apply(&event);

        let m = state.molecule(&mol_id).unwrap();
        assert_eq!(m.status, MoleculeStatus::Completed);
    }

    #[test]
    fn test_complete_step() {
        let mut state = OpsState::default();

        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::CompleteStep {
                molecule_id: mol_id.clone(),
                step: 2,
                total: 8,
            })
            .unwrap();
        state.apply(&event);

        let m = state.molecule(&mol_id).unwrap();
        assert_eq!(m.current_step, 2);
        assert_eq!(m.total_steps, 8);
    }

    // -- Replay --

    #[test]
    fn test_replay_produces_same_state() {
        let mut state = OpsState::default();
        let mut events = Vec::new();

        // Build up a sequence of events.
        let cmds = vec![
            OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            },
            OpsCommand::SpawnWorker {
                worker_id: worker_id("onyx"),
                agent_id: agent_id("witness"),
            },
            OpsCommand::UpdateWorkerStatus {
                worker_id: worker_id("quartz"),
                status: WorkerStatus::Active,
            },
        ];

        for cmd in cmds {
            let event = state.decide(cmd).unwrap();
            state.apply(&event);
            events.push(event);
        }

        // Replay from scratch should produce identical state.
        let replayed = OpsState::replay(&events);
        assert_eq!(replayed.workers().len(), state.workers().len());
        for (id, w) in state.workers() {
            let rw = replayed.worker(id).unwrap();
            assert_eq!(w.status, rw.status);
            assert_eq!(w.agent_id, rw.agent_id);
        }
    }

    #[test]
    fn test_active_workers_filter() {
        let mut state = OpsState::default();

        // Spawn two workers.
        for name in &["quartz", "onyx"] {
            let event = state
                .decide(OpsCommand::SpawnWorker {
                    worker_id: worker_id(name),
                    agent_id: agent_id("polecat"),
                })
                .unwrap();
            state.apply(&event);
        }

        // Activate one.
        let event = state
            .decide(OpsCommand::UpdateWorkerStatus {
                worker_id: worker_id("quartz"),
                status: WorkerStatus::Active,
            })
            .unwrap();
        state.apply(&event);

        let active = state.active_workers();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id.as_str(), "quartz");
    }

    // -- Serde roundtrips --

    #[test]
    fn test_ops_command_serde_roundtrip() {
        let cmds = vec![
            OpsCommand::SpawnWorker {
                worker_id: worker_id("q"),
                agent_id: agent_id("p"),
            },
            OpsCommand::TerminateWorker {
                worker_id: worker_id("q"),
                reason: "done".to_owned(),
            },
            OpsCommand::AssignMolecule {
                molecule_id: molecule_id("cs-20260401-abcd"),
                worker_id: worker_id("q"),
            },
            OpsCommand::TransitionMolecule {
                molecule_id: molecule_id("cs-20260401-abcd"),
                to: MoleculeStatus::Frozen,
                reason: Some("waiting".to_owned()),
            },
            OpsCommand::CompleteStep {
                molecule_id: molecule_id("cs-20260401-abcd"),
                step: 0,
                total: 3,
            },
            OpsCommand::UpdateWorkerStatus {
                worker_id: worker_id("q"),
                status: WorkerStatus::Active,
            },
        ];

        for cmd in cmds {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: OpsCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cmd, "roundtrip failed for: {json}");
        }
    }

    #[test]
    fn test_ops_event_serde_roundtrip() {
        let now = Utc::now();
        let events = vec![
            OpsEvent::WorkerSpawned {
                worker_id: worker_id("q"),
                agent_id: agent_id("p"),
                at: now,
            },
            OpsEvent::WorkerTerminated {
                worker_id: worker_id("q"),
                reason: "exit".to_owned(),
                at: now,
            },
            OpsEvent::WorkerStatusChanged {
                worker_id: worker_id("q"),
                from: WorkerStatus::Starting,
                to: WorkerStatus::Active,
                at: now,
            },
            OpsEvent::MoleculeAssigned {
                molecule_id: molecule_id("cs-20260401-abcd"),
                worker_id: worker_id("q"),
                at: now,
            },
            OpsEvent::MoleculeTransitioned {
                molecule_id: molecule_id("cs-20260401-abcd"),
                from: MoleculeStatus::Running,
                to: MoleculeStatus::Completed,
                reason: None,
                at: now,
            },
            OpsEvent::StepCompleted {
                molecule_id: molecule_id("cs-20260401-abcd"),
                step: 1,
                total: 3,
                at: now,
            },
        ];

        for evt in events {
            let json = serde_json::to_string(&evt).unwrap();
            let back: OpsEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, evt, "roundtrip failed for: {json}");
        }
    }

    #[test]
    fn test_ops_envelope_serde_roundtrip() {
        let envelope = OpsEnvelope {
            sequence: 42,
            event: OpsEvent::WorkerSpawned {
                worker_id: worker_id("q"),
                agent_id: agent_id("p"),
                at: Utc::now(),
            },
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let back: OpsEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, envelope);
    }

    #[test]
    fn test_ops_event_store_is_object_safe() {
        fn accepts_dyn(_store: &dyn OpsEventStore) {}
        let _ = accepts_dyn;
    }

    #[test]
    fn test_envelope_replay() {
        let envelopes = vec![
            OpsEnvelope {
                sequence: 1,
                event: OpsEvent::WorkerSpawned {
                    worker_id: worker_id("q"),
                    agent_id: agent_id("p"),
                    at: Utc::now(),
                },
            },
            OpsEnvelope {
                sequence: 2,
                event: OpsEvent::WorkerStatusChanged {
                    worker_id: worker_id("q"),
                    from: WorkerStatus::Starting,
                    to: WorkerStatus::Active,
                    at: Utc::now(),
                },
            },
        ];

        let state = OpsState::replay_envelopes(&envelopes);
        assert_eq!(state.last_sequence(), 2);
        assert_eq!(state.workers().len(), 1);
        assert_eq!(
            state.worker(&worker_id("q")).unwrap().status,
            WorkerStatus::Active,
        );
    }

    #[test]
    fn test_default_state_is_empty() {
        let state = OpsState::default();
        assert!(state.workers().is_empty());
        assert!(state.molecules().is_empty());
        assert_eq!(state.last_sequence(), 0);
        assert!(state.active_workers().is_empty());
    }

    #[test]
    fn test_command_actor() {
        let cmd = OpsCommand::TerminateWorker {
            worker_id: worker_id("q"),
            reason: "done".to_owned(),
        };
        assert_eq!(cmd.actor().unwrap().as_str(), "q");

        let cmd = OpsCommand::SpawnWorker {
            worker_id: worker_id("q"),
            agent_id: agent_id("p"),
        };
        assert!(cmd.actor().is_none());
    }

    #[test]
    fn test_ops_event_timestamp() {
        let now = Utc::now();
        let event = OpsEvent::WorkerSpawned {
            worker_id: worker_id("q"),
            agent_id: agent_id("p"),
            at: now,
        };
        assert_eq!(event.timestamp(), now);
    }

    // -- Transition validation --

    #[test]
    fn test_transition_molecule_rejects_invalid_from_completed() {
        let mut state = OpsState::default();

        // Spawn worker, assign molecule, transition to completed.
        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::AssignMolecule {
                molecule_id: mol_id.clone(),
                worker_id: worker_id("quartz"),
            })
            .unwrap();
        state.apply(&event);

        let event = state
            .decide(OpsCommand::TransitionMolecule {
                molecule_id: mol_id.clone(),
                to: MoleculeStatus::Completed,
                reason: None,
            })
            .unwrap();
        state.apply(&event);

        // Completed → Running should be rejected.
        let result = state.decide(OpsCommand::TransitionMolecule {
            molecule_id: mol_id,
            to: MoleculeStatus::Running,
            reason: None,
        });
        assert!(
            matches!(result, Err(CosmonError::InvalidTransition { .. })),
            "completed → running should be invalid"
        );
    }

    #[test]
    fn test_transition_molecule_rejects_frozen_to_completed() {
        let mut state = OpsState::default();

        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::AssignMolecule {
                molecule_id: mol_id.clone(),
                worker_id: worker_id("quartz"),
            })
            .unwrap();
        state.apply(&event);

        // Running → Frozen (valid).
        let event = state
            .decide(OpsCommand::TransitionMolecule {
                molecule_id: mol_id.clone(),
                to: MoleculeStatus::Frozen,
                reason: None,
            })
            .unwrap();
        state.apply(&event);

        // Frozen → Completed (invalid — must thaw first).
        let result = state.decide(OpsCommand::TransitionMolecule {
            molecule_id: mol_id,
            to: MoleculeStatus::Completed,
            reason: None,
        });
        assert!(
            matches!(result, Err(CosmonError::InvalidTransition { .. })),
            "frozen → completed should be invalid"
        );
    }

    #[test]
    fn test_transition_molecule_allows_frozen_to_running() {
        let mut state = OpsState::default();

        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::AssignMolecule {
                molecule_id: mol_id.clone(),
                worker_id: worker_id("quartz"),
            })
            .unwrap();
        state.apply(&event);

        // Running → Frozen.
        let event = state
            .decide(OpsCommand::TransitionMolecule {
                molecule_id: mol_id.clone(),
                to: MoleculeStatus::Frozen,
                reason: None,
            })
            .unwrap();
        state.apply(&event);

        // Frozen → Running (thaw).
        let event = state
            .decide(OpsCommand::TransitionMolecule {
                molecule_id: mol_id.clone(),
                to: MoleculeStatus::Running,
                reason: None,
            })
            .unwrap();
        state.apply(&event);

        let m = state.molecule(&mol_id).unwrap();
        assert_eq!(m.status, MoleculeStatus::Running);
    }

    #[test]
    fn test_transition_molecule_rejects_self_transition() {
        let mut state = OpsState::default();

        let event = state
            .decide(OpsCommand::SpawnWorker {
                worker_id: worker_id("quartz"),
                agent_id: agent_id("polecat"),
            })
            .unwrap();
        state.apply(&event);

        let mol_id = molecule_id("cs-20260401-abcd");
        let event = state
            .decide(OpsCommand::AssignMolecule {
                molecule_id: mol_id.clone(),
                worker_id: worker_id("quartz"),
            })
            .unwrap();
        state.apply(&event);

        // Running → Running (no-op, but invalid).
        let result = state.decide(OpsCommand::TransitionMolecule {
            molecule_id: mol_id,
            to: MoleculeStatus::Running,
            reason: None,
        });
        assert!(
            matches!(result, Err(CosmonError::InvalidTransition { .. })),
            "running → running should be invalid"
        );
    }
}
