// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem turn input source for the in-process OpenAI-compatible adapter.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_agent_harness::spine::{TurnInput, TurnInputOutcome, TurnInputSource};
use cosmon_core::advisory_attempt::{
    AdvisoryObservation, AdvisorySeatId, AdvisoryUnavailableReason,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::work_message::{
    deliverable, fold, render_for_context, AdapterCapability, ContextObservability,
    ContextObservation, DeliveryAdapter, DeliveryOutcome, LiveInsertion, ObserverId, Receipt,
    SafePoint, Stage, WorkMessageStore, WORK_MESSAGE_SCHEMA_VERSION,
};
use cosmon_state::work_message::FileWorkMessageStore;
use serde::Deserialize;

#[derive(Deserialize)]
struct WorkRef {
    owner_molecule: MoleculeId,
    seat: AdvisorySeatId,
}

/// Work messages for one current member, read only at provider request turns.
pub struct WorkTurnInput {
    member: MoleculeId,
    reference: WorkRef,
    store: FileWorkMessageStore,
}

impl WorkTurnInput {
    /// Discover a declared work reference, if this molecule has one.
    ///
    /// # Errors
    /// Returns a custody or membership error for a present but invalid reference.
    pub fn discover(member_dir: &Path) -> Result<Option<Self>, String> {
        let path = member_dir.join("work-ref.json");
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let reference: WorkRef = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let member = member_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "member directory has no molecule id".to_owned())
            .and_then(|name| MoleculeId::new(name).map_err(|e| e.to_string()))?;
        let roster = member_dir
            .parent()
            .ok_or("member directory has no roster")?;
        let store = FileWorkMessageStore::new(
            PathBuf::from(roster).join(reference.owner_molecule.as_str()),
        );
        let source = Self {
            member,
            reference,
            store,
        };
        source.check_member()?;
        Ok(Some(source))
    }

    fn check_member(&self) -> Result<(), String> {
        let scope = self
            .store
            .load_scope()
            .map_err(|e| e.to_string())?
            .ok_or("work scope missing")?;
        if scope.owner != self.reference.owner_molecule
            || scope.seat_of(&self.member) != Some(&self.reference.seat)
        {
            return Err("caller is not in the current work roster".to_owned());
        }
        Ok(())
    }
}

impl TurnInputSource for WorkTurnInput {
    fn take(&self) -> Result<Vec<TurnInput>, String> {
        self.check_member()?;
        let records = self.store.load_all().map_err(|e| e.to_string())?;
        let scope = records.scope.ok_or("work scope missing")?;
        let now = Utc::now();
        let projection =
            fold(&scope, &records.envelopes, &records.receipts, now).map_err(|e| e.to_string())?;
        deliverable(
            &projection,
            &self.reference.seat,
            DeliveryAdapter::HarnessTurn,
            now,
        )
        .into_iter()
        .map(|envelope| {
            let bytes = self
                .store
                .read_payload(envelope)
                .map_err(|e| e.to_string())?;
            let content = render_for_context(envelope, &bytes).map_err(|e| e.to_string())?;
            Ok(TurnInput {
                key: envelope.key.as_str().to_owned(),
                content,
            })
        })
        .collect()
    }

    fn record(&self, inputs: &[TurnInput], outcome: TurnInputOutcome) -> Result<(), String> {
        // Membership was checked when the request was assembled. A roster
        // revision while the provider is in flight must not erase the result
        // of a request that already carried these bytes.
        let records = self.store.load_all().map_err(|e| e.to_string())?;
        let now = Utc::now();
        for input in inputs {
            let envelope = records
                .envelopes
                .iter()
                .find(|envelope| envelope.key.as_str() == input.key)
                .ok_or_else(|| format!("work envelope {} disappeared", input.key))?;
            if envelope.recipient != self.reference.seat {
                return Err(format!("work envelope {} changed recipient", input.key));
            }
            let observer = ObserverId::Adapter {
                adapter: DeliveryAdapter::HarnessTurn,
            };
            let stage = Stage::DeliveryAttempted {
                adapter: DeliveryAdapter::HarnessTurn,
                mechanism: "harness_turn".to_owned(),
                outcome: match outcome {
                    TurnInputOutcome::Succeeded => DeliveryOutcome::Submitted,
                    TurnInputOutcome::Failed => DeliveryOutcome::Failed {
                        reason: "provider request failed".to_owned(),
                    },
                },
            };
            self.store
                .append_receipt(&Receipt::for_envelope(
                    envelope,
                    observer.clone(),
                    now,
                    stage,
                ))
                .map_err(|e| e.to_string())?;
            if outcome == TurnInputOutcome::Succeeded {
                self.store
                    .append_receipt(&Receipt::for_envelope(
                        envelope,
                        observer,
                        now,
                        Stage::ContextDelivered {
                            adapter: DeliveryAdapter::HarnessTurn,
                            observation: ContextObservation::Observed {
                                evidence: "included in provider request; provider returned success"
                                    .to_owned(),
                            },
                        },
                    ))
                    .map_err(|e| e.to_string())?;
            }
        }
        if outcome == TurnInputOutcome::Succeeded
            && !inputs.is_empty()
            && !records.capabilities.iter().any(|record| {
                record.seat == self.reference.seat && record.adapter == DeliveryAdapter::HarnessTurn
            })
        {
            self.store
                .record_capability(&AdapterCapability {
                    schema_version: WORK_MESSAGE_SCHEMA_VERSION,
                    seat: self.reference.seat.clone(),
                    adapter: DeliveryAdapter::HarnessTurn,
                    harness_version: AdvisoryObservation::Unavailable {
                        reason: AdvisoryUnavailableReason::NotObserved,
                    },
                    live_insertion: LiveInsertion::Supported,
                    context_observation: ContextObservability::Observed,
                    safe_points: vec![SafePoint::BeforeProviderRequest],
                    first_observed_at: now,
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use cosmon_core::work_message::{
        Confidentiality, MessageBudget, MessageKey, SeatDecl, SenderEvidence, Submission, WorkScope,
    };
    use cosmon_hash::Hash;

    #[test]
    fn no_work_reference_uses_the_existing_provider_path() {
        let root = tempfile::tempdir().expect("molecule directory");
        let member_dir = root.path().join("task-20260928-bbbb");
        fs::create_dir_all(&member_dir).expect("member directory");
        assert!(WorkTurnInput::discover(&member_dir)
            .expect("no reference is normal")
            .is_none());
    }

    fn fixture() -> (tempfile::TempDir, WorkTurnInput) {
        let root = tempfile::tempdir().expect("molecule directory");
        let owner = MoleculeId::new("task-20260928-0000").expect("owner id");
        let sender = MoleculeId::new("task-20260928-aaaa").expect("sender id");
        let recipient = MoleculeId::new("task-20260928-bbbb").expect("recipient id");
        let sender_seat = AdvisorySeatId::new("a").expect("sender seat");
        let recipient_seat = AdvisorySeatId::new("b").expect("recipient seat");
        let owner_dir = root.path().join(owner.as_str());
        let member_dir = root.path().join(recipient.as_str());
        fs::create_dir_all(&member_dir).expect("member directory");
        fs::write(
            member_dir.join("work-ref.json"),
            serde_json::json!({
                "owner_molecule": owner,
                "seat": recipient_seat,
            })
            .to_string(),
        )
        .expect("work reference");
        let store = FileWorkMessageStore::new(owner_dir);
        let scope = WorkScope {
            schema_version: WORK_MESSAGE_SCHEMA_VERSION,
            owner,
            seats: BTreeMap::from([
                (
                    sender_seat.clone(),
                    SeatDecl {
                        molecule: sender,
                        required: true,
                        provider_requirement: None,
                    },
                ),
                (
                    recipient_seat.clone(),
                    SeatDecl {
                        molecule: recipient,
                        required: true,
                        provider_requirement: None,
                    },
                ),
            ]),
            budget: MessageBudget {
                max_payload_bytes: 100,
                max_messages_per_seat: 2,
                max_bytes_per_seat: 200,
                default_ttl_secs: 3600,
                redeliver_after_secs: 60,
                max_delivery_attempts: 2,
            },
            declared_at: Utc::now(),
        };
        store.declare(&scope).expect("declared work");
        let bytes = b"peer finding";
        store
            .submit(
                Submission {
                    scope_owner: scope.owner.clone(),
                    scope_revision: scope.revision().expect("revision"),
                    sender: sender_seat,
                    recipient: recipient_seat,
                    key: Some(MessageKey::new("finding-1").expect("message key")),
                    payload_digest: Hash::of_bytes(bytes),
                    payload_bytes: bytes.len() as u64,
                    sender_time: Utc::now(),
                    reply_to: None,
                    phase: None,
                    confidentiality: Confidentiality::Internal,
                    ttl_secs: None,
                    sender_evidence: SenderEvidence::CallerEnvSameUid,
                },
                bytes,
                Utc::now(),
            )
            .expect("admitted envelope");
        let source = WorkTurnInput::discover(&member_dir)
            .expect("work reference")
            .expect("member source");
        (root, source)
    }

    #[test]
    fn successful_request_records_observed_context_after_attempt() {
        let (_root, source) = fixture();
        let inputs = source.take().expect("deliverable input");
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].content.contains("peer finding"));
        let before = source.store.load_all().expect("before receipts");
        assert!(!before
            .receipts
            .iter()
            .any(|receipt| matches!(receipt.stage, Stage::ContextDelivered { .. })));
        source
            .record(&inputs, TurnInputOutcome::Succeeded)
            .expect("successful request receipt");
        let after = source.store.load_all().expect("after receipts");
        assert!(after.receipts.iter().any(|receipt| matches!(
            receipt.stage,
            Stage::DeliveryAttempted {
                outcome: DeliveryOutcome::Submitted,
                ..
            }
        )));
        assert!(after.receipts.iter().any(|receipt| matches!(
            receipt.stage,
            Stage::ContextDelivered {
                observation: ContextObservation::Observed { .. },
                ..
            }
        )));
        assert!(source.take().expect("next turn").is_empty());
    }

    #[test]
    fn failed_request_records_no_context_delivery() {
        let (_root, source) = fixture();
        let inputs = source.take().expect("deliverable input");
        source
            .record(&inputs, TurnInputOutcome::Failed)
            .expect("failed request receipt");
        let records = source.store.load_all().expect("receipts");
        assert!(records.receipts.iter().any(|receipt| matches!(
            receipt.stage,
            Stage::DeliveryAttempted {
                outcome: DeliveryOutcome::Failed { .. },
                ..
            }
        )));
        assert!(!records
            .receipts
            .iter()
            .any(|receipt| matches!(receipt.stage, Stage::ContextDelivered { .. })));
    }

    #[test]
    fn roster_revision_during_request_does_not_erase_delivery_result() {
        let (_root, source) = fixture();
        let inputs = source.take().expect("deliverable input");
        let mut revised = source
            .store
            .load_scope()
            .expect("scope read")
            .expect("scope");
        revised.seats.remove(&source.reference.seat);
        source.store.declare(&revised).expect("revised roster");
        source
            .record(&inputs, TurnInputOutcome::Succeeded)
            .expect("request result");
        let records = source.store.load_all().expect("receipts");
        assert!(records.receipts.iter().any(|receipt| matches!(
            receipt.stage,
            Stage::ContextDelivered {
                observation: ContextObservation::Observed { .. },
                ..
            }
        )));
    }
}
