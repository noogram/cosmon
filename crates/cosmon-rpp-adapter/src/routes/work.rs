// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit advisory exchange for a binding-derived work seat (W4).
//! No lifecycle executor, transport backend or lease writer is reachable here.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Utc};
use cosmon_core::advisory_attempt::AdvisorySeatId;
use cosmon_core::collaboration::{AdmittedCollaborator, CollaborationScope, CollaborationTarget};
use cosmon_core::id::MoleculeId;
use cosmon_core::work_message::{
    fold_with_history, Admission, Confidentiality, ContextObservation, DeliveryAdapter,
    Disposition, MessageKey, ScopeRevision, Stage, WorkMessageError, WorkMessageStore, WorkScope,
};
use cosmon_filestore::FileStore;
use cosmon_hash::Hash;
use cosmon_state::collaboration::{CollaborationBindingReader, CommitError};
use cosmon_state::work_message::{FileWorkMessageStore, WorkStoreError};
use cosmon_state::work_ops::{
    AcknowledgeOutcome, AcknowledgeRequest, PullRequest, SendRequest, WorkOperations,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admission::Verb;
use crate::collaboration::{
    admit_collaborator, commit_collaboration, CollaborationAdmission, CollaborationAdmissionError,
};
use crate::error::ApiError;
use crate::jwt::JwtVerifier;
use crate::routes::molecules::{build_spark_public, extract_bearer};
use crate::AppState;

const PAGE: usize = 100;
const RESPONSE_BYTES: usize = 1_048_576;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBody {
    scope_revision: ScopeRevision,
    recipient: AdvisorySeatId,
    idempotency_key: MessageKey,
    sender_time: DateTime<Utc>,
    reply_to: Option<MessageKey>,
    phase: Option<String>,
    confidentiality: Confidentiality,
    ttl_secs: Option<u64>,
    payload: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PullBody {
    scope_revision: ScopeRevision,
    #[serde(default = "pull_adapter")]
    adapter: DeliveryAdapter,
    context_observation: Option<WireContextObservation>,
    #[serde(default)]
    peek: bool,
}

// Only the transport schema is strict; old canonical observations remain readable.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum WireContextObservation {
    Observed { evidence: String },
    Unknown { reason: String },
}

impl WireContextObservation {
    fn validate(&self) -> Result<(), ApiError> {
        let text = match self {
            Self::Observed { evidence } => evidence,
            Self::Unknown { reason } => reason,
        };
        if text.len() > 4096 {
            return Err(api(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"));
        }
        Ok(())
    }
}

fn pull_adapter() -> DeliveryAdapter {
    DeliveryAdapter::Pull
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeekQuery {
    scope_revision: ScopeRevision,
    #[serde(default = "pull_adapter")]
    adapter: DeliveryAdapter,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageQuery {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AckBody {
    scope_revision: ScopeRevision,
    payload_digest: Hash,
    disposition: Disposition,
    reply: Option<MessageKey>,
    note: Option<String>,
}

struct Door {
    admitted: AdmittedCollaborator,
    bindings: CollaborationBindingReader,
    dir: PathBuf,
}

fn api(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::with_status(status, code)
}
fn invalid() -> ApiError {
    api(StatusCode::BAD_REQUEST, "invalid_json_body")
}
fn admission_error(error: &CollaborationAdmissionError) -> ApiError {
    let status = match error.http_status() {
        403 => StatusCode::FORBIDDEN,
        503 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api(status, error.code())
}

fn work_error(error: WorkStoreError) -> ApiError {
    use WorkMessageError as E;
    let (status, code) = match error {
        WorkStoreError::NoScope => (StatusCode::NOT_FOUND, "unknown_work"),
        WorkStoreError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::UNPROCESSABLE_ENTITY, "evidence_unreadable")
        }
        WorkStoreError::Io(_) => (StatusCode::SERVICE_UNAVAILABLE, "custody_unavailable"),
        WorkStoreError::Domain(E::EnvelopeExpired(_)) => (StatusCode::GONE, "envelope_expired"),
        WorkStoreError::Domain(E::StaleScopeRevision { .. }) => {
            (StatusCode::CONFLICT, "stale_scope_revision")
        }
        WorkStoreError::Domain(
            E::NotAMember { .. } | E::NotTheRecipient { .. } | E::WrongScopeOwner { .. },
        ) => (StatusCode::FORBIDDEN, "not_a_member"),
        WorkStoreError::Domain(E::PayloadTooLarge { .. }) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
        }
        WorkStoreError::Domain(E::KeyCollision { .. }) | WorkStoreError::EnvelopeConflict(_) => {
            (StatusCode::CONFLICT, "key_collision")
        }
        WorkStoreError::Domain(E::ReplyToUnknown(_) | E::UnknownEnvelope(_)) => {
            (StatusCode::NOT_FOUND, "unknown_envelope")
        }
        WorkStoreError::Domain(E::SeatBudgetExhausted { .. }) => {
            (StatusCode::TOO_MANY_REQUESTS, "budget_exhausted")
        }
        WorkStoreError::Domain(E::PayloadDigestMismatch(_) | E::EnvelopeDigestMismatch { .. })
        | WorkStoreError::PayloadMismatch => (StatusCode::UNPROCESSABLE_ENTITY, "digest_mismatch"),
        WorkStoreError::Domain(E::InvalidIdentifier { .. } | E::InvalidTtl(_)) => {
            (StatusCode::BAD_REQUEST, "invalid_json_body")
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence"),
    };
    api(status, code)
}

fn commit<T>(door: &Door, effect: impl FnOnce() -> Result<T, ApiError>) -> Result<T, ApiError> {
    commit_collaboration(&door.bindings, &door.admitted, effect).map_err(|error| match error {
        CommitError::Binding(error) => admission_error(&error.into()),
        CommitError::Effect(error) => error,
    })
}

fn door(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    owner: &str,
    scope: CollaborationScope,
) -> Result<Door, ApiError> {
    let token =
        extract_bearer(headers).map_err(|_| api(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|_| api(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    // Scope is checked before any work resource or binding read. The common
    // boundary still applies the identity, ingress budget and kill switches.
    let owner = MoleculeId::new(owner).map_err(|_| invalid())?;
    let spark = build_spark_public(state, &jwt, Verb::ObserveMolecule, Some(owner.as_str()))?;
    let state_dir = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon/state");
    let bindings = CollaborationBindingReader::new(&state_dir);
    let admitted = admit_collaborator(&CollaborationAdmission {
        bindings: &bindings,
        nucleon_map: &state.nucleon_map.load(),
        jwt: &jwt,
        tenant: &spark.noyau,
        headers,
        target: &CollaborationTarget::Work {
            owner: owner.clone(),
        },
        required: scope,
    })
    .map_err(|error| admission_error(&error))?;
    let dir = FileStore::new(&state_dir).molecule_dir(&owner);
    Ok(Door {
        admitted,
        bindings,
        dir,
    })
}

fn scope(door: &Door) -> Result<WorkScope, ApiError> {
    let scope = FileWorkMessageStore::new(door.dir.clone())
        .load_scope()
        .map_err(work_error)?
        .ok_or_else(|| work_error(WorkStoreError::NoScope))?;
    scope
        .validate()
        .map_err(|_| api(StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence"))?;
    let caller = door
        .admitted
        .work_caller()
        .ok_or_else(|| api(StatusCode::FORBIDDEN, "binding_missing"))?;
    if scope.owner != *caller.scope_owner() || !scope.seats.contains_key(caller.seat()) {
        return Err(api(StatusCode::FORBIDDEN, "not_a_member"));
    }
    Ok(scope)
}

fn response(scope: &WorkScope, now: DateTime<Utc>) -> Result<Value, ApiError> {
    Ok(
        json!({"schema_version": 1, "received_at": now, "scope_revision": scope.revision().map_err(|_| api(StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence"))?}),
    )
}

/// W-LIST: paginated roster metadata visible only to an admitted member.
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    Path(owner): Path<String>,
    headers: HeaderMap,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &owner, CollaborationScope::WorkRead)?;
    let Query(query) = query.map_err(|_| invalid())?;
    let limit = query.limit.unwrap_or(PAGE);
    if limit == 0 || limit > PAGE {
        return Err(invalid());
    }
    let scope = scope(&door)?;
    let mut body = response(&scope, Utc::now())?;
    body["owner"] = json!(scope.owner);
    body["budget"] = json!(scope.budget);
    body["seats"] = json!(scope
        .seats
        .iter()
        .skip(query.offset)
        .take(limit)
        .collect::<std::collections::BTreeMap<_, _>>());
    body["next_offset"] = if query.offset.saturating_add(limit) < scope.seats.len() {
        json!(query.offset.saturating_add(limit))
    } else {
        Value::Null
    };
    Ok(Json(body))
}

/// W-SEND: sender identity comes exclusively from the admitted binding.
pub(crate) async fn send(
    State(state): State<Arc<AppState>>,
    Path(owner): Path<String>,
    headers: HeaderMap,
    body: Result<Json<SendBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &owner, CollaborationScope::WorkWrite)?;
    let Json(body) = body.map_err(|_| invalid())?;
    commit(&door, || {
        let scope = scope(&door)?;
        let caller = door
            .admitted
            .work_caller()
            .ok_or_else(|| api(StatusCode::FORBIDDEN, "binding_missing"))?;
        let now = Utc::now();
        let admission = WorkOperations::new(door.dir.clone())
            .send(
                &caller,
                SendRequest {
                    scope_revision: body.scope_revision,
                    recipient: body.recipient,
                    key: Some(body.idempotency_key),
                    sender_time: body.sender_time,
                    reply_to: body.reply_to,
                    phase: body.phase,
                    confidentiality: body.confidentiality,
                    ttl_secs: body.ttl_secs,
                },
                body.payload.as_bytes(),
                now,
            )
            .map_err(work_error)?;
        let (envelope, duplicate) = match admission {
            Admission::Admit(envelope) => (envelope, false),
            Admission::Duplicate(envelope) => (envelope, true),
        };
        let mut output = response(&scope, now)?;
        output["envelope"] = json!(envelope);
        output["duplicate"] = json!(duplicate);
        Ok(Json(output))
    })
}

/// W-PEEK: bounded payload read that appends no observations.
pub(crate) async fn peek(
    State(state): State<Arc<AppState>>,
    Path(owner): Path<String>,
    headers: HeaderMap,
    query: Result<Query<PeekQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &owner, CollaborationScope::WorkRead)?;
    let Query(query) = query.map_err(|_| invalid())?;
    inbox(
        &door,
        PullBody {
            scope_revision: query.scope_revision,
            adapter: query.adapter,
            context_observation: None,
            peek: true,
        },
    )
}

/// W-PULL: explicit bounded delivery, with revocation checked at commit.
pub(crate) async fn pull(
    State(state): State<Arc<AppState>>,
    Path(owner): Path<String>,
    headers: HeaderMap,
    body: Result<Json<PullBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &owner, CollaborationScope::WorkRead)?;
    let Json(body) = body.map_err(|_| invalid())?;
    commit(&door, || inbox(&door, body))
}

fn inbox(door: &Door, body: PullBody) -> Result<Json<Value>, ApiError> {
    if let Some(observation) = &body.context_observation {
        observation.validate()?;
    }
    let scope = scope(door)?;
    let now = Utc::now();
    let caller = door
        .admitted
        .work_caller()
        .ok_or_else(|| api(StatusCode::FORBIDDEN, "binding_missing"))?;
    let records = FileWorkMessageStore::new(door.dir.clone())
        .load_all()
        .map_err(work_error)?;
    let projection = fold_with_history(
        &scope,
        &records.history,
        &records.envelopes,
        &records.receipts,
        now,
    )
    .map_err(|_| api(StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence"))?;
    if !projection.findings.is_empty() {
        return Err(api(StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence"));
    }
    let mut output = response(&scope, now)?;
    output["messages"] = json!([]);
    output["receipts"] = json!([]);
    let mut remaining = RESPONSE_BYTES.saturating_sub(
        serde_json::to_vec(&output)
            .map_err(|_| ApiError::internal("corrupt_evidence"))?
            .len(),
    );
    // Reports for messages sent by this seat are observations, never new
    // inbox envelopes. Bound them independently and keep their wire type.
    let mut receipts = Vec::new();
    for receipt in records
        .receipts
        .iter()
        .filter(|r| {
            matches!(r.stage, Stage::Consumed { .. })
                && records
                    .envelopes
                    .iter()
                    .any(|e| e.key == r.key && e.sender == *caller.seat())
        })
        .take(PAGE)
    {
        let size = serde_json::to_vec(receipt)
            .map_err(|_| ApiError::internal("corrupt_evidence"))?
            .len()
            + 1;
        if size > remaining {
            break;
        }
        remaining -= size;
        receipts.push(receipt);
    }
    output["receipts"] = json!(receipts);
    let mut messages = Vec::new();
    WorkOperations::new(door.dir.clone())
        .pull_bounded(
            &caller,
            &PullRequest {
                scope_revision: body.scope_revision,
                adapter: body.adapter,
                mechanism: "rpp-explicit-pull".into(),
                peek: body.peek,
                // A remote report is advisory; this boundary cannot observe a model.
                context_observation: body.context_observation.map(|_| {
                    ContextObservation::Unknown {
                        reason: "remote model input is unobservable to the host".into(),
                    }
                }),
            },
            now,
            PAGE,
            RESPONSE_BYTES as u64,
            |message| {
                let text = std::str::from_utf8(&message.payload)
                    .map_err(|_| WorkStoreError::ExistingPayloadMismatch)?;
                let value = json!({"envelope": message.envelope, "payload": text});
                let size = serde_json::to_vec(&value)?.len() + 1;
                if size > remaining {
                    if messages.is_empty() {
                        return Err(WorkMessageError::PayloadTooLarge {
                            bytes: size as u64,
                            max: remaining as u64,
                        }
                        .into());
                    }
                    return Ok(false);
                }
                remaining -= size;
                messages.push(value);
                Ok(true)
            },
        )
        .map_err(work_error)?;
    output["messages"] = json!(messages);
    Ok(Json(output))
}

/// W-ACK: recipient-bound report, separate from a result message.
pub(crate) async fn acknowledge(
    State(state): State<Arc<AppState>>,
    Path((owner, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<AckBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &owner, CollaborationScope::WorkWrite)?;
    let Json(body) = body.map_err(|_| invalid())?;
    if body.note.as_ref().is_some_and(|note| note.len() > 4096) {
        return Err(api(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"));
    }
    let key = MessageKey::new(key).map_err(|_| invalid())?;
    commit(&door, || {
        let scope = scope(&door)?;
        let caller = door
            .admitted
            .work_caller()
            .ok_or_else(|| api(StatusCode::FORBIDDEN, "binding_missing"))?;
        let now = Utc::now();
        let outcome = WorkOperations::new(door.dir.clone())
            .acknowledge(
                &caller,
                AcknowledgeRequest {
                    scope_revision: body.scope_revision,
                    key,
                    payload_digest: body.payload_digest,
                    disposition: body.disposition,
                    reply: body.reply,
                    note: body.note.map(String::into_bytes),
                },
                now,
            )
            .map_err(work_error)?;
        let mut output = response(&scope, now)?;
        match outcome {
            AcknowledgeOutcome::Recorded(receipt) => {
                output["receipt"] = json!(receipt);
                output["duplicate"] = json!(false);
            }
            AcknowledgeOutcome::Duplicate => {
                output["duplicate"] = json!(true);
            }
        }
        Ok(Json(output))
    })
}
