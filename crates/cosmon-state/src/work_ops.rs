// SPDX-License-Identifier: AGPL-3.0-only

//! Typed work operations shared by local and admitted non-local callers.
//!
//! Argument parsing and identity admission stay at outer boundaries. This
//! module accepts an [`AdmittedWorkCaller`] and performs custody mutations
//! under the owning work's lock, so local and future non-local adapters reuse
//! the same scope, quota, idempotency and receipt semantics.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_core::advisory_attempt::AdvisorySeatId;
use cosmon_core::work_message::{
    accept_consumption, deliverable, fold_with_history, Admission, AdmittedWorkCaller,
    Confidentiality, Consumption, ContextObservation, DeliveryAdapter, DeliveryOutcome,
    Disposition, Envelope, MessageKey, ObserverId, Receipt, ScopeRevision, Stage, Submission,
    WorkMessageError, WorkMessageStore, WorkProjection,
};
use cosmon_hash::Hash;

use crate::work_message::{FileWorkMessageStore, WorkStoreError};

/// Typed request to admit one message for an already admitted caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    /// Exact scope revision the caller read.
    pub scope_revision: ScopeRevision,
    /// Receiving seat.
    pub recipient: AdvisorySeatId,
    /// Caller-retained idempotency key, or a request to derive one.
    pub key: Option<MessageKey>,
    /// Sender clock observation.
    pub sender_time: DateTime<Utc>,
    /// Envelope answered by this message.
    pub reply_to: Option<MessageKey>,
    /// Free-form phase observation.
    pub phase: Option<String>,
    /// Payload confidentiality class.
    pub confidentiality: Confidentiality,
    /// Optional lifetime override.
    pub ttl_secs: Option<u64>,
}

/// Typed request to retrieve messages for an already admitted caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// Exact scope revision the caller read.
    pub scope_revision: ScopeRevision,
    /// Adapter performing the pull.
    pub adapter: DeliveryAdapter,
    /// Fixed mechanism name selected by the boundary.
    pub mechanism: String,
    /// Whether to leave the canonical receipt stream untouched.
    pub peek: bool,
    /// Honest context observation, when the adapter can make one.
    pub context_observation: Option<ContextObservation>,
}

/// One envelope and verified payload returned by [`WorkOperations::pull`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledMessage {
    /// Canonical immutable envelope.
    pub envelope: Envelope,
    /// Payload bytes verified against the envelope digest.
    pub payload: Vec<u8>,
}

/// Typed recipient acknowledgment for one exact envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgeRequest {
    /// Exact scope revision the caller read.
    pub scope_revision: ScopeRevision,
    /// Message key being acknowledged.
    pub key: MessageKey,
    /// Digest the caller received with that key.
    pub payload_digest: Hash,
    /// Recipient's protocol disposition.
    pub disposition: Disposition,
    /// Already admitted reply key, if any.
    pub reply: Option<MessageKey>,
    /// Optional bounded note bytes stored before the receipt.
    pub note: Option<Vec<u8>>,
}

/// Result of an idempotent acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcknowledgeOutcome {
    /// A new receipt was appended.
    Recorded(Receipt),
    /// The envelope already had a consumption report.
    Duplicate,
}

/// Filesystem-backed implementation of typed work operations.
#[derive(Debug, Clone)]
pub struct WorkOperations {
    store: FileWorkMessageStore,
}

impl WorkOperations {
    /// Bind operations to the canonical directory of one owning molecule.
    #[must_use]
    pub fn new(owner_dir: PathBuf) -> Self {
        Self {
            store: FileWorkMessageStore::new(owner_dir),
        }
    }

    /// Admit payload bytes using only the injected caller for sender identity.
    ///
    /// # Errors
    /// Refuses stale scopes, callers outside the roster, quota excess, key
    /// collisions, payload mismatch and custody failures.
    pub fn send(
        &self,
        caller: &AdmittedWorkCaller,
        request: SendRequest,
        payload: &[u8],
        admitted_at: DateTime<Utc>,
    ) -> Result<Admission, WorkStoreError> {
        self.store.submit(
            Submission {
                scope_owner: caller.scope_owner().clone(),
                scope_revision: request.scope_revision,
                sender: caller.seat().clone(),
                recipient: request.recipient,
                key: request.key,
                payload_digest: Hash::of_bytes(payload),
                payload_bytes: payload.len() as u64,
                sender_time: request.sender_time,
                reply_to: request.reply_to,
                phase: request.phase,
                confidentiality: request.confidentiality,
                ttl_secs: request.ttl_secs,
                sender_evidence: caller.sender_evidence(),
            },
            payload,
            admitted_at,
        )
    }

    /// Return verified deliverable payloads and optionally append observations.
    ///
    /// # Errors
    /// Refuses stale scopes and callers outside the roster, and returns custody
    /// errors for malformed records or payload integrity defects.
    pub fn pull(
        &self,
        caller: &AdmittedWorkCaller,
        request: &PullRequest,
        now: DateTime<Utc>,
    ) -> Result<Vec<PulledMessage>, WorkStoreError> {
        self.with_lock(|| {
            let projection = self.current_projection(caller, request.scope_revision, now)?;
            let envelopes = deliverable(&projection, caller.seat(), request.adapter, now);
            let messages = envelopes
                .iter()
                .map(|envelope| {
                    self.store
                        .read_payload(envelope)
                        .map(|payload| PulledMessage {
                            envelope: (*envelope).clone(),
                            payload,
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !request.peek {
                for message in &messages {
                    self.store.append_receipt(&Receipt::for_envelope(
                        &message.envelope,
                        ObserverId::Adapter {
                            adapter: request.adapter,
                        },
                        now,
                        Stage::DeliveryAttempted {
                            adapter: request.adapter,
                            mechanism: request.mechanism.clone(),
                            outcome: DeliveryOutcome::Submitted,
                        },
                    ))?;
                    if let Some(observation) = &request.context_observation {
                        self.store.append_receipt(&Receipt::for_envelope(
                            &message.envelope,
                            ObserverId::Adapter {
                                adapter: request.adapter,
                            },
                            now,
                            Stage::ContextDelivered {
                                adapter: request.adapter,
                                observation: observation.clone(),
                            },
                        ))?;
                    }
                }
            }
            Ok(messages)
        })
    }

    /// Persist a recipient-bound, key-and-digest-bound consumption report.
    ///
    /// The optional note becomes durable before the receipt that references
    /// its digest. Repeating an already recorded report is a no-op.
    ///
    /// # Errors
    /// Refuses stale scopes, a non-recipient caller, a wrong digest, an unknown
    /// key or reply, an oversized note, and custody failures.
    pub fn acknowledge(
        &self,
        caller: &AdmittedWorkCaller,
        request: AcknowledgeRequest,
        now: DateTime<Utc>,
    ) -> Result<AcknowledgeOutcome, WorkStoreError> {
        self.acknowledge_inner(caller, request, now, || Ok(()))
    }

    #[cfg(test)]
    fn acknowledge_with_hook(
        &self,
        caller: &AdmittedWorkCaller,
        request: AcknowledgeRequest,
        now: DateTime<Utc>,
        hook: impl FnOnce() -> Result<(), WorkStoreError>,
    ) -> Result<AcknowledgeOutcome, WorkStoreError> {
        self.acknowledge_inner(caller, request, now, hook)
    }

    fn acknowledge_inner(
        &self,
        caller: &AdmittedWorkCaller,
        request: AcknowledgeRequest,
        now: DateTime<Utc>,
        hook: impl FnOnce() -> Result<(), WorkStoreError>,
    ) -> Result<AcknowledgeOutcome, WorkStoreError> {
        self.with_lock(|| {
            let projection = self.current_projection(caller, request.scope_revision, now)?;
            let view = projection
                .envelopes
                .get(&request.key)
                .ok_or_else(|| WorkMessageError::UnknownEnvelope(request.key.clone()))?;
            if view.envelope.payload_digest != request.payload_digest {
                return Err(WorkMessageError::EnvelopeDigestMismatch {
                    key: request.key.clone(),
                    expected: request.payload_digest,
                    current: view.envelope.payload_digest,
                }
                .into());
            }
            let note_digest = request.note.as_ref().map(|note| Hash::of_bytes(note));
            if request
                .note
                .as_ref()
                .is_some_and(|note| note.len() as u64 > projection.budget.max_payload_bytes)
            {
                return Err(WorkMessageError::PayloadTooLarge {
                    bytes: request.note.as_ref().map_or(0, |note| note.len() as u64),
                    max: projection.budget.max_payload_bytes,
                }
                .into());
            }
            match accept_consumption(
                &projection,
                caller.seat(),
                &request.key,
                request.disposition,
                request.reply,
                note_digest,
                now,
            )? {
                Consumption::Record(receipt) => {
                    if let (Some(note), Some(digest)) = (&request.note, note_digest) {
                        self.store.put_note(&digest, note)?;
                    }
                    hook()?;
                    self.store.append_receipt(&receipt)?;
                    Ok(AcknowledgeOutcome::Recorded(receipt))
                }
                Consumption::Duplicate(_) => Ok(AcknowledgeOutcome::Duplicate),
            }
        })
    }

    fn current_projection(
        &self,
        caller: &AdmittedWorkCaller,
        expected_revision: ScopeRevision,
        now: DateTime<Utc>,
    ) -> Result<WorkProjection, WorkStoreError> {
        let records = self.store.load_all()?;
        let scope = records.scope.ok_or(WorkStoreError::NoScope)?;
        if scope.owner != *caller.scope_owner() {
            return Err(WorkMessageError::WrongScopeOwner {
                submitted: caller.scope_owner().to_string(),
                current: scope.owner.to_string(),
            }
            .into());
        }
        if !scope.seats.contains_key(caller.seat()) {
            return Err(WorkMessageError::NotAMember {
                role: "caller",
                seat: caller.seat().clone(),
            }
            .into());
        }
        let current = scope.revision()?;
        if expected_revision != current {
            return Err(WorkMessageError::StaleScopeRevision {
                submitted: expected_revision,
                current,
            }
            .into());
        }
        fold_with_history(
            &scope,
            &records.history,
            &records.envelopes,
            &records.receipts,
            now,
        )
        .map_err(Into::into)
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, WorkStoreError>,
    ) -> Result<T, WorkStoreError> {
        let lock = self.store.lock()?;
        let result = operation();
        let unlock = fs2::FileExt::unlock(&lock).map_err(WorkStoreError::from);
        match (result, unlock) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone as _;
    use cosmon_core::id::MoleculeId;
    use cosmon_core::work_message::{
        MessageBudget, SeatDecl, SenderEvidence, WorkScope, WORK_MESSAGE_SCHEMA_VERSION,
    };

    use super::*;

    fn time(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).unwrap()
    }

    fn seat(name: &str) -> AdvisorySeatId {
        AdvisorySeatId::new(name).unwrap()
    }

    fn owner() -> MoleculeId {
        MoleculeId::new("task-20261002-0000").unwrap()
    }

    fn scope() -> WorkScope {
        let declaration = |molecule: &str| SeatDecl {
            molecule: MoleculeId::new(molecule).unwrap(),
            required: true,
            provider_requirement: None,
        };
        WorkScope {
            schema_version: WORK_MESSAGE_SCHEMA_VERSION,
            owner: owner(),
            seats: BTreeMap::from([
                (seat("implement"), declaration("task-20261002-0001")),
                (seat("review"), declaration("task-20261002-0002")),
            ]),
            budget: MessageBudget {
                max_payload_bytes: 100,
                max_messages_per_seat: 4,
                max_bytes_per_seat: 400,
                default_ttl_secs: 3_600,
                redeliver_after_secs: 60,
                max_delivery_attempts: 2,
            },
            declared_at: time(0),
        }
    }

    fn caller(name: &str) -> AdmittedWorkCaller {
        AdmittedWorkCaller::new(owner(), seat(name), SenderEvidence::AdmittedNonLocal)
    }

    fn send_request(scope: &WorkScope, key: &str, recipient: &str) -> SendRequest {
        SendRequest {
            scope_revision: scope.revision().unwrap(),
            recipient: seat(recipient),
            key: Some(MessageKey::new(key).unwrap()),
            sender_time: time(1),
            reply_to: None,
            phase: None,
            confidentiality: Confidentiality::Internal,
            ttl_secs: None,
        }
    }

    fn setup() -> (tempfile::TempDir, WorkOperations, WorkScope) {
        let dir = tempfile::tempdir().unwrap();
        let scope = scope();
        FileWorkMessageStore::new(dir.path().to_path_buf())
            .declare(&scope)
            .unwrap();
        let operations = WorkOperations::new(dir.path().to_path_buf());
        (dir, operations, scope)
    }

    #[test]
    fn non_local_caller_is_recorded_without_ambient_identity() {
        let (_dir, operations, scope) = setup();
        let admitted = operations
            .send(
                &caller("review"),
                send_request(&scope, "finding-1", "implement"),
                b"finding",
                time(2),
            )
            .unwrap();
        let Admission::Admit(envelope) = admitted else {
            panic!("first submission was duplicate");
        };
        assert_eq!(envelope.sender, seat("review"));
        assert_eq!(envelope.sender_evidence, SenderEvidence::AdmittedNonLocal);
    }

    #[test]
    fn wrong_seat_revision_and_changed_duplicate_are_refused() {
        let (dir, operations, scope) = setup();
        let state_path = dir.path().join("state.json");
        let events_path = dir.path().join("events.jsonl");
        std::fs::write(&state_path, b"state sentinel\n").unwrap();
        std::fs::write(&events_path, b"event sentinel\n").unwrap();
        let lifecycle_before = (
            std::fs::read(&state_path).unwrap(),
            std::fs::read(&events_path).unwrap(),
        );
        let outsider =
            AdmittedWorkCaller::new(owner(), seat("outsider"), SenderEvidence::AdmittedNonLocal);
        assert!(operations
            .send(
                &outsider,
                send_request(&scope, "finding-1", "implement"),
                b"finding",
                time(2),
            )
            .unwrap_err()
            .to_string()
            .contains("not a member"));
        let mut stale = send_request(&scope, "finding-1", "implement");
        stale.scope_revision = ScopeRevision(Hash::of_bytes(b"stale"));
        assert!(operations
            .send(&caller("review"), stale, b"finding", time(2))
            .unwrap_err()
            .to_string()
            .contains("stale scope revision"));
        operations
            .send(
                &caller("review"),
                send_request(&scope, "finding-1", "implement"),
                b"finding",
                time(2),
            )
            .unwrap();
        assert!(operations
            .send(
                &caller("review"),
                send_request(&scope, "finding-1", "implement"),
                b"changed",
                time(3),
            )
            .unwrap_err()
            .to_string()
            .contains("different payload"));
        assert_eq!(std::fs::read(state_path).unwrap(), lifecycle_before.0);
        assert_eq!(std::fs::read(events_path).unwrap(), lifecycle_before.1);
    }

    #[test]
    fn acknowledgment_is_recipient_key_and_digest_bound() {
        let (dir, operations, scope) = setup();
        let digest = Hash::of_bytes(b"finding");
        operations
            .send(
                &caller("review"),
                send_request(&scope, "finding-1", "implement"),
                b"finding",
                time(2),
            )
            .unwrap();
        let request = AcknowledgeRequest {
            scope_revision: scope.revision().unwrap(),
            key: MessageKey::new("finding-1").unwrap(),
            payload_digest: digest,
            disposition: Disposition::Considered,
            reply: None,
            note: Some(b"checked".to_vec()),
        };
        assert!(operations
            .acknowledge(&caller("review"), request.clone(), time(3))
            .unwrap_err()
            .to_string()
            .contains("not the recipient"));
        let mut wrong_digest = request.clone();
        wrong_digest.payload_digest = Hash::of_bytes(b"other");
        assert!(operations
            .acknowledge(&caller("implement"), wrong_digest, time(3))
            .unwrap_err()
            .to_string()
            .contains("not expected digest"));
        assert!(matches!(
            operations
                .acknowledge(&caller("implement"), request, time(3))
                .unwrap(),
            AcknowledgeOutcome::Recorded(_)
        ));
        assert!(dir
            .path()
            .join("work/notes")
            .read_dir()
            .unwrap()
            .next()
            .is_some());
    }

    #[test]
    fn note_is_durable_before_ack_and_retry_records_one_consumption() {
        let (dir, operations, scope) = setup();
        operations
            .send(
                &caller("review"),
                send_request(&scope, "finding-1", "implement"),
                b"finding",
                time(2),
            )
            .unwrap();
        let request = AcknowledgeRequest {
            scope_revision: scope.revision().unwrap(),
            key: MessageKey::new("finding-1").unwrap(),
            payload_digest: Hash::of_bytes(b"finding"),
            disposition: Disposition::Considered,
            reply: None,
            note: Some(b"checked".to_vec()),
        };
        assert!(matches!(
            operations.acknowledge_with_hook(
                &caller("implement"),
                request.clone(),
                time(3),
                || Err(WorkStoreError::Injected),
            ),
            Err(WorkStoreError::Injected)
        ));
        assert!(dir
            .path()
            .join("work/notes")
            .read_dir()
            .unwrap()
            .next()
            .is_some());
        assert!(matches!(
            operations
                .acknowledge(&caller("implement"), request, time(3))
                .unwrap(),
            AcknowledgeOutcome::Recorded(_)
        ));
        let records = FileWorkMessageStore::new(dir.path().to_path_buf())
            .load_all()
            .unwrap();
        assert_eq!(
            records
                .receipts
                .iter()
                .filter(|receipt| matches!(receipt.stage, Stage::Consumed { .. }))
                .count(),
            1
        );
    }
}
