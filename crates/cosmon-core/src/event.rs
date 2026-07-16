// SPDX-License-Identifier: AGPL-3.0-only

//! Domain events emitted during Cosmon operations.
//!
//! Events are the primary observability mechanism: every significant state
//! change produces an `Event` value. The core crate defines the enum (pure
//! data); I/O backends (e.g. `cosmon-filestore`) handle persistence.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::event::Event;
//! use cosmon_core::id::{MoleculeId, WorkerId};
//!
//! let evt = Event::WorkerSpawned {
//!     worker_id: WorkerId::new("quartz").unwrap(),
//!     agent: "polecat".to_owned(),
//! };
//!
//! // Round-trip through JSON:
//! let json = serde_json::to_string(&evt).unwrap();
//! let back: Event = serde_json::from_str(&json).unwrap();
//! assert_eq!(back, evt);
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use cosmon_hash::Hash;

use crate::id::{AgentId, ClaimId, MoleculeId, WorkerId};
use crate::kind::MoleculeKind;
use crate::message::{Channel, MessagePriority};
use crate::molecule::MoleculeStatus;

/// A timestamped wrapper around a domain event.
///
/// Every persisted event carries its own timestamp so that the event log
/// is self-describing — no external clock is needed to reconstruct order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Hash of the previous envelope in the chain (`None` for the genesis
    /// entry of a molecule's log). Backward compatible: old JSONL lines
    /// without this field load with `None` and `cs verify` treats them
    /// as pre-v2 unsigned history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<Hash>,
    /// Hash of this envelope (canonical form of `{prev_hash, timestamp, event}`).
    /// Populated when the event is appended; absent on historical entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<Hash>,
    /// The event payload.
    #[serde(flatten)]
    pub event: Event,
}

impl Envelope {
    /// Wrap an event with the current UTC timestamp.
    ///
    /// The returned envelope has no hash wiring yet; `cosmon-filestore::append`
    /// fills in `prev_hash` and `hash` at persist time.
    #[must_use]
    pub fn now(event: Event) -> Self {
        Self {
            timestamp: Utc::now(),
            prev_hash: None,
            hash: None,
            event,
        }
    }

    /// The subset of the envelope that participates in hashing.
    ///
    /// Excludes `hash` itself (would be circular) but includes `prev_hash`
    /// so the chain linkage is committed to. Used by
    /// [`cosmon_hash::hash_event`] callers and `cs verify`.
    #[must_use]
    pub fn hash_payload(&self) -> serde_json::Value {
        // Round-trip through JSON so the flattened event fields land at the
        // top level, exactly as they will on disk.
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.remove("hash");
        }
        v
    }
}

/// The kind of mutation an agent intends to perform on shared state.
///
/// Used in [`Event::IntentDeclared`] to classify the operation so that
/// governance mechanisms can apply appropriate access-control policies
/// (ADR-GOV-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive] // classification set may grow; consistent with sibling ClaimType/Verdict
pub enum MutationType {
    /// Read-only observation of state (no mutation).
    Read,
    /// Create a new entity or record.
    Create,
    /// Update an existing entity or record.
    Update,
    /// Delete an entity or record.
    Delete,
    /// Execute an action with side effects (e.g. spawn, dispatch).
    Execute,
}

/// Classification of a verifiable claim extracted from molecule output.
///
/// Used by the IFBDD (Instrument-First, Before Drawing Dragons) verification
/// pipeline to route a claim to the correct verifier kind. Marked
/// `#[non_exhaustive]` because the menagerie of verifiers will grow — new
/// variants must never be a breaking change for downstream consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimType {
    /// A citation to a paper, document, or prior artifact (DOI, citekey, URL).
    Citation,
    /// A numeric value or statistic subject to recomputation or bounds check.
    Numeric,
    /// A code snippet, identifier, or API shape that must compile or exist.
    Code,
    /// A hyperlink or filesystem path whose reachability is checkable.
    Link,
    /// A free-form factual assertion (worldmodel claim).
    Factual,
}

/// Outcome of verifying a claim.
///
/// `#[non_exhaustive]` so new verdict nuances (e.g. `PartiallyConfirmed`)
/// can be added without breaking downstream match sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Verdict {
    /// Evidence supports the claim.
    Confirmed,
    /// Evidence contradicts the claim.
    Refuted,
    /// Evidence insufficient to decide either way.
    Inconclusive,
}

impl std::fmt::Display for MutationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Create => f.write_str("create"),
            Self::Update => f.write_str("update"),
            Self::Delete => f.write_str("delete"),
            Self::Execute => f.write_str("execute"),
        }
    }
}

/// Domain events emitted during Cosmon operations.
///
/// Each variant captures the minimum context needed to reconstruct what
/// happened. Field names are kept short for compact JSONL output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A worker process was created.
    WorkerSpawned {
        /// The worker's identity.
        worker_id: WorkerId,
        /// The agent definition that the worker instantiates.
        agent: String,
    },

    /// A worker process terminated (normally or abnormally).
    WorkerTerminated {
        /// The worker's identity.
        worker_id: WorkerId,
        /// Human-readable reason for termination.
        reason: String,
    },

    /// A molecule was dispatched to a worker for execution.
    MoleculeDispatched {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// The worker assigned to execute it.
        worker_id: WorkerId,
    },

    /// A molecule transitioned to a new lifecycle status.
    MoleculeTransitioned {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// The status before the transition.
        from: MoleculeStatus,
        /// The status after the transition.
        to: MoleculeStatus,
    },

    /// A molecule step was completed.
    StepCompleted {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Zero-based step index that was completed.
        step: usize,
        /// Total number of steps in the molecule.
        total: usize,
    },

    /// A task was dispatched to an agent via the communication fabric.
    ///
    /// Emitted when `send_task` routes through a specific channel based on
    /// message priority. For [`Channel::DoltBead`], the `bead_id` is populated;
    /// for [`Channel::JsonlFile`], this event IS the durable record.
    TaskDispatched {
        /// Short summary of the dispatched task.
        title: String,
        /// The target agent address (e.g. "cosmon/polecats/jasper").
        target: String,
        /// Priority that determined channel selection.
        priority: MessagePriority,
        /// Channel used for this dispatch.
        channel: Channel,
        /// Bead ID, present only when channel is [`Channel::DoltBead`].
        bead_id: Option<String>,
    },

    /// An agent declared intent to mutate shared state (ADR-GOV-004).
    ///
    /// Emitted *before* the mutation occurs so that governance mechanisms
    /// can observe — and potentially reject — the operation.
    IntentDeclared {
        /// The agent declaring the intent.
        agent_id: AgentId,
        /// The domain being targeted (e.g. "molecules", "workers", "beads").
        target_domain: String,
        /// The kind of mutation the agent intends to perform.
        mutation_type: MutationType,
        /// Human-readable description of the expected scope of the mutation.
        expected_scope: String,
    },

    /// A worker was frozen (suspended for preemption or maintenance).
    WorkerFrozen {
        /// The frozen worker's identity.
        worker_id: WorkerId,
        /// The worker that triggered the freeze (if preemption).
        preempted_by: Option<WorkerId>,
    },

    /// A frozen worker was thawed (resumed).
    WorkerThawed {
        /// The thawed worker's identity.
        worker_id: WorkerId,
    },

    /// A worker was preempted by a higher-clearance worker.
    WorkerPreempted {
        /// The worker that was preempted (frozen).
        incumbent: WorkerId,
        /// The worker that took over.
        challenger: WorkerId,
    },

    /// A worker was immediately killed.
    WorkerKilled {
        /// The killed worker's identity.
        worker_id: WorkerId,
    },

    /// A dead worker was respawned by patrol.
    WorkerRespawned {
        /// The respawned worker's identity.
        worker_id: WorkerId,
        /// How many times this worker has been respawned (including this time).
        restart_count: u32,
    },

    /// A molecule advanced to its next step.
    MoleculeEvolved {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Zero-based index of the step that was just completed.
        step: usize,
        /// Total number of steps in the molecule.
        total: usize,
    },

    /// A molecule was marked as completed.
    MoleculeCompleted {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Human-readable reason for completion.
        reason: String,
    },

    /// A molecule collapsed (terminal failure).
    MoleculeCollapsed {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// Human-readable reason for collapse.
        reason: String,
    },

    /// A molecule was frozen (execution suspended).
    MoleculeFrozen {
        /// The molecule's identity.
        molecule_id: MoleculeId,
    },

    /// A frozen molecule was thawed (execution resumed).
    MoleculeThawed {
        /// The molecule's identity.
        molecule_id: MoleculeId,
    },

    /// A molecule decayed into child molecules (1 → N).
    MoleculeDecayed {
        /// The source molecule that decayed.
        molecule_id: MoleculeId,
        /// The product molecule IDs.
        products: Vec<MoleculeId>,
        /// Human-readable reason for the decay.
        reason: String,
    },

    /// Multiple molecules merged into one (N → 1).
    MoleculeMerged {
        /// The source molecule IDs that were consumed.
        sources: Vec<MoleculeId>,
        /// The resulting product molecule ID.
        product: MoleculeId,
        /// Human-readable reason for the merge.
        reason: String,
    },

    /// A molecule's kind was transformed.
    MoleculeTransformed {
        /// The molecule's identity.
        molecule_id: MoleculeId,
        /// The kind before the transformation.
        from_kind: MoleculeKind,
        /// The kind after the transformation.
        to_kind: MoleculeKind,
        /// Human-readable reason for the transformation.
        reason: String,
    },

    /// A verifiable claim was extracted from molecule output (IFBDD).
    ///
    /// Emitted by the claim-extraction pass *before* any verifier runs, so
    /// that the noise channel itself is observable. The pair with
    /// [`Event::ClaimVerified`] carries `claim_id` as the correlation key.
    ClaimEmitted {
        /// Correlation ID shared with the subsequent `ClaimVerified` event.
        claim_id: ClaimId,
        /// Molecule whose output contained the claim.
        molecule_id: MoleculeId,
        /// Zero-based step index that produced the claim.
        step_index: usize,
        /// Classification (citation, numeric, code, link, factual).
        claim_type: ClaimType,
        /// Verbatim excerpt of the claim as it appears in the output.
        claim_text: String,
        /// Optional (start, end) byte offsets into the source artifact.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_span: Option<(usize, usize)>,
    },

    /// Result of verifying a previously emitted claim (IFBDD).
    ///
    /// Closes the loop opened by [`Event::ClaimEmitted`]. Measuring
    /// `cost_latency_ms` per verifier is how we quantify whether the
    /// verification gain pays for its own compute — the whole point of
    /// instrumenting before adding dragons.
    ClaimVerified {
        /// Correlation ID from the originating `ClaimEmitted` event.
        claim_id: ClaimId,
        /// Human-readable verifier identifier (e.g. "doi-lookup", "cargo-check").
        verifier_kind: String,
        /// Confirmed / Refuted / Inconclusive.
        verdict: Verdict,
        /// Wall-clock latency of the verifier invocation, in milliseconds.
        cost_latency_ms: u64,
        /// Optional pointer to supporting evidence (URL, file path, CAS hash).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_ref: Option<String>,
    },

    /// An error occurred during an operation.
    ErrorOccurred {
        /// Free-form context about what was happening.
        context: String,
        /// The error message.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AgentId, MoleculeId, WorkerId};

    #[test]
    fn test_event_serde_roundtrip_worker_spawned() {
        let evt = Event::WorkerSpawned {
            worker_id: WorkerId::new("quartz").unwrap(),
            agent: "polecat".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"worker_spawned\""));
    }

    #[test]
    fn test_event_serde_roundtrip_worker_terminated() {
        let evt = Event::WorkerTerminated {
            worker_id: WorkerId::new("onyx").unwrap(),
            reason: "session timeout".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_dispatched() {
        let evt = Event::MoleculeDispatched {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            worker_id: WorkerId::new("quartz").unwrap(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_transitioned() {
        let evt = Event::MoleculeTransitioned {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            from: MoleculeStatus::Running,
            to: MoleculeStatus::Completed,
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_step_completed() {
        let evt = Event::StepCompleted {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            step: 2,
            total: 8,
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_task_dispatched_bead() {
        use crate::message::{Channel, MessagePriority};

        let evt = Event::TaskDispatched {
            title: "fix the widget".to_owned(),
            target: "cosmon/polecats/jasper".to_owned(),
            priority: MessagePriority::Critical,
            channel: Channel::DoltBead,
            bead_id: Some("cs-abc".to_owned()),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"task_dispatched\""));
        assert!(json.contains("\"critical\""));
        assert!(json.contains("\"dolt_bead\""));
    }

    #[test]
    fn test_event_serde_roundtrip_task_dispatched_jsonl() {
        use crate::message::{Channel, MessagePriority};

        let evt = Event::TaskDispatched {
            title: "run diagnostics".to_owned(),
            target: "cosmon/polecats/opal".to_owned(),
            priority: MessagePriority::Normal,
            channel: Channel::JsonlFile,
            bead_id: None,
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"jsonl_file\""));
    }

    #[test]
    fn test_event_serde_roundtrip_intent_declared() {
        let evt = Event::IntentDeclared {
            agent_id: AgentId::new("witness").unwrap(),
            target_domain: "molecules".to_owned(),
            mutation_type: MutationType::Update,
            expected_scope: "transition cs-20260401-abcd to frozen".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"intent_declared\""));
        assert!(json.contains("\"mutation_type\":\"update\""));
    }

    #[test]
    fn test_mutation_type_display() {
        assert_eq!(MutationType::Read.to_string(), "read");
        assert_eq!(MutationType::Create.to_string(), "create");
        assert_eq!(MutationType::Update.to_string(), "update");
        assert_eq!(MutationType::Delete.to_string(), "delete");
        assert_eq!(MutationType::Execute.to_string(), "execute");
    }

    #[test]
    fn test_event_serde_roundtrip_worker_frozen() {
        let evt = Event::WorkerFrozen {
            worker_id: WorkerId::new("quartz").unwrap(),
            preempted_by: Some(WorkerId::new("topaz").unwrap()),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"worker_frozen\""));
    }

    #[test]
    fn test_event_serde_roundtrip_worker_thawed() {
        let evt = Event::WorkerThawed {
            worker_id: WorkerId::new("quartz").unwrap(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_worker_preempted() {
        let evt = Event::WorkerPreempted {
            incumbent: WorkerId::new("reader").unwrap(),
            challenger: WorkerId::new("executor").unwrap(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_worker_killed() {
        let evt = Event::WorkerKilled {
            worker_id: WorkerId::new("ruby").unwrap(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_worker_respawned() {
        let evt = Event::WorkerRespawned {
            worker_id: WorkerId::new("quartz").unwrap(),
            restart_count: 2,
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_event_serde_roundtrip_claim_emitted() {
        let evt = Event::ClaimEmitted {
            claim_id: ClaimId::new("cl-abc").unwrap(),
            molecule_id: MoleculeId::new("cs-20260412-f49f").unwrap(),
            step_index: 1,
            claim_type: ClaimType::Citation,
            claim_text: "Shannon 1948".to_owned(),
            source_span: Some((42, 55)),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"claim_emitted\""));
        assert!(json.contains("\"claim_type\":\"citation\""));
    }

    #[test]
    fn test_event_serde_roundtrip_claim_verified() {
        let evt = Event::ClaimVerified {
            claim_id: ClaimId::new("cl-abc").unwrap(),
            verifier_kind: "doi-lookup".to_owned(),
            verdict: Verdict::Confirmed,
            cost_latency_ms: 312,
            evidence_ref: Some("https://doi.org/10.1002/j.1538-7305.1948.tb01338.x".to_owned()),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"claim_verified\""));
        assert!(json.contains("\"verdict\":\"confirmed\""));
    }

    #[test]
    fn test_claim_emitted_omits_absent_source_span() {
        let evt = Event::ClaimEmitted {
            claim_id: ClaimId::new("cl-x").unwrap(),
            molecule_id: MoleculeId::new("cs-20260412-aaaa").unwrap(),
            step_index: 0,
            claim_type: ClaimType::Numeric,
            claim_text: "pi ~ 3.14".to_owned(),
            source_span: None,
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(!json.contains("source_span"));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_claim_type_and_verdict_roundtrip() {
        for ct in [
            ClaimType::Citation,
            ClaimType::Numeric,
            ClaimType::Code,
            ClaimType::Link,
            ClaimType::Factual,
        ] {
            let j = serde_json::to_string(&ct).unwrap();
            let b: ClaimType = serde_json::from_str(&j).unwrap();
            assert_eq!(ct, b);
        }
        for v in [Verdict::Confirmed, Verdict::Refuted, Verdict::Inconclusive] {
            let j = serde_json::to_string(&v).unwrap();
            let b: Verdict = serde_json::from_str(&j).unwrap();
            assert_eq!(v, b);
        }
    }

    #[test]
    fn test_event_serde_roundtrip_error_occurred() {
        let evt = Event::ErrorOccurred {
            context: "nucleating molecule".to_owned(),
            message: "formula not found".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn test_envelope_serde_roundtrip() {
        let envelope = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("quartz").unwrap(),
            agent: "polecat".to_owned(),
        });
        let json = serde_json::to_string(&envelope).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, envelope);
        // Envelope flattens the event, so timestamp + kind are siblings
        assert!(json.contains("\"timestamp\""));
        assert!(json.contains("\"kind\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_evolved() {
        let evt = Event::MoleculeEvolved {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            step: 1,
            total: 3,
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_evolved\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_completed() {
        let evt = Event::MoleculeCompleted {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            reason: "all steps done".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_completed\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_collapsed() {
        let evt = Event::MoleculeCollapsed {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            reason: "fatal error".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_collapsed\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_frozen() {
        let evt = Event::MoleculeFrozen {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_frozen\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_thawed() {
        let evt = Event::MoleculeThawed {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_thawed\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_decayed() {
        let evt = Event::MoleculeDecayed {
            molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
            products: vec![
                MoleculeId::new("cs-20260401-bbbb").unwrap(),
                MoleculeId::new("cs-20260401-cccc").unwrap(),
            ],
            reason: "split into subtasks".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_decayed\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_merged() {
        let evt = Event::MoleculeMerged {
            sources: vec![
                MoleculeId::new("cs-20260401-aaaa").unwrap(),
                MoleculeId::new("cs-20260401-bbbb").unwrap(),
            ],
            product: MoleculeId::new("cs-20260401-cccc").unwrap(),
            reason: "consolidate findings".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_merged\""));
    }

    #[test]
    fn test_event_serde_roundtrip_molecule_transformed() {
        use crate::kind::MoleculeKind;
        let evt = Event::MoleculeTransformed {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            from_kind: MoleculeKind::Idea,
            to_kind: MoleculeKind::Task,
            reason: "ready to implement".to_owned(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
        assert!(json.contains("\"kind\":\"molecule_transformed\""));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_all_variants_roundtrip() {
        use crate::kind::MoleculeKind;
        use crate::message::{Channel, MessagePriority};

        let events = vec![
            Event::WorkerSpawned {
                worker_id: WorkerId::new("a").unwrap(),
                agent: "x".to_owned(),
            },
            Event::WorkerTerminated {
                worker_id: WorkerId::new("a").unwrap(),
                reason: "done".to_owned(),
            },
            Event::MoleculeDispatched {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                worker_id: WorkerId::new("a").unwrap(),
            },
            Event::MoleculeTransitioned {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                from: MoleculeStatus::Running,
                to: MoleculeStatus::Frozen,
            },
            Event::StepCompleted {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                step: 0,
                total: 1,
            },
            Event::TaskDispatched {
                title: "test task".to_owned(),
                target: "cosmon/polecats/ruby".to_owned(),
                priority: MessagePriority::High,
                channel: Channel::DoltBead,
                bead_id: Some("cs-xyz".to_owned()),
            },
            Event::IntentDeclared {
                agent_id: AgentId::new("witness").unwrap(),
                target_domain: "workers".to_owned(),
                mutation_type: MutationType::Delete,
                expected_scope: "terminate worker a".to_owned(),
            },
            Event::WorkerFrozen {
                worker_id: WorkerId::new("a").unwrap(),
                preempted_by: None,
            },
            Event::WorkerThawed {
                worker_id: WorkerId::new("a").unwrap(),
            },
            Event::WorkerPreempted {
                incumbent: WorkerId::new("a").unwrap(),
                challenger: WorkerId::new("b").unwrap(),
            },
            Event::WorkerKilled {
                worker_id: WorkerId::new("a").unwrap(),
            },
            Event::WorkerRespawned {
                worker_id: WorkerId::new("a").unwrap(),
                restart_count: 1,
            },
            Event::MoleculeEvolved {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                step: 0,
                total: 2,
            },
            Event::MoleculeCompleted {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                reason: "done".to_owned(),
            },
            Event::MoleculeCollapsed {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                reason: "error".to_owned(),
            },
            Event::MoleculeFrozen {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
            },
            Event::MoleculeThawed {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
            },
            Event::MoleculeDecayed {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                products: vec![MoleculeId::new("cs-20260401-bbbb").unwrap()],
                reason: "split".to_owned(),
            },
            Event::MoleculeMerged {
                sources: vec![
                    MoleculeId::new("cs-20260401-aaaa").unwrap(),
                    MoleculeId::new("cs-20260401-bbbb").unwrap(),
                ],
                product: MoleculeId::new("cs-20260401-cccc").unwrap(),
                reason: "combine".to_owned(),
            },
            Event::MoleculeTransformed {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                from_kind: MoleculeKind::Idea,
                to_kind: MoleculeKind::Task,
                reason: "promote".to_owned(),
            },
            Event::ErrorOccurred {
                context: "ctx".to_owned(),
                message: "msg".to_owned(),
            },
        ];
        for evt in events {
            let json = serde_json::to_string(&evt).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(back, evt, "failed roundtrip for: {json}");
        }
    }
}
