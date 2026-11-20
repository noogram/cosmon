// SPDX-License-Identifier: AGPL-3.0-only

//! Remote presence of a bound pilot attachment (issue #212, W7a): S-ATTACH,
//! S-BEAT, S-DETACH and S-PEERS of `docs/specs/cross-machine-collaboration.md`.
//!
//! Mission and attachment come from the admitted binding. A path that names
//! another mission is refused by admission itself; a path that names another
//! attachment is refused here, before any presence record is read. Neither is
//! ever rewritten to the caller's own. The `presence-store` is the only writer.
//! No lifecycle executor, lease ledger, mailbox or checkpoint store is
//! reachable from this module: presence confers no authority (O8, ADR-168).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::Utc;
use cosmon_core::collaboration::{
    AdmittedCollaborator, AttachmentId, CollaborationCapability, CollaborationScope,
    CollaborationTarget,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::remote_presence::{AttachRequest, RemotePresence, PEERS_PAGE};
use cosmon_core::work_message::MessageKey;
use cosmon_filestore::{PresenceStore, RemotePresenceStoreError};
use cosmon_state::collaboration::{CollaborationBindingReader, CommitError};
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

/// S-ATTACH request body. Role, lease epoch, mission and attachment are not
/// fields: the binding fixes the last two and presence carries no authority.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachBody {
    idempotency_key: MessageKey,
    label: Option<String>,
}

/// S-PEERS query: a cursor past the last attachment of the previous page.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeersQuery {
    after: Option<String>,
    limit: Option<usize>,
}

struct Door {
    admitted: AdmittedCollaborator,
    bindings: CollaborationBindingReader,
    store: PresenceStore,
    mission: MoleculeId,
}

fn api(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::with_status(status, code)
}

fn invalid() -> ApiError {
    api(StatusCode::BAD_REQUEST, "invalid_json_body")
}

fn binding_missing() -> ApiError {
    api(StatusCode::FORBIDDEN, "binding_missing")
}

fn admission_error(error: &CollaborationAdmissionError) -> ApiError {
    let status = match error.http_status() {
        403 => StatusCode::FORBIDDEN,
        503 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api(status, error.code())
}

fn store_error(error: RemotePresenceStoreError) -> ApiError {
    match error {
        RemotePresenceStoreError::Refused(refusal) => {
            let status = match refusal.http_status() {
                400 => StatusCode::BAD_REQUEST,
                404 => StatusCode::NOT_FOUND,
                409 => StatusCode::CONFLICT,
                429 => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            let mut error = api(status, refusal.code());
            if let cosmon_core::remote_presence::RemotePresenceError::HeartbeatTooSoon {
                retry_after_secs,
            } = refusal
            {
                error.retry_after_seconds = Some(retry_after_secs);
            }
            error
        }
        RemotePresenceStoreError::Corrupt(_) => {
            api(StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence")
        }
        RemotePresenceStoreError::Io(_) => {
            api(StatusCode::SERVICE_UNAVAILABLE, "custody_unavailable")
        }
    }
}

/// Admit the caller for `mission` with `scope`. The scope is checked before
/// any binding or presence record is read.
fn door(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    mission: &str,
    scope: CollaborationScope,
) -> Result<Door, ApiError> {
    let token =
        extract_bearer(headers).map_err(|_| api(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|_| api(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let mission = MoleculeId::new(mission).map_err(|_| invalid())?;
    let spark = build_spark_public(state, &jwt, Verb::ObserveMolecule, Some(mission.as_str()))?;
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
        target: &CollaborationTarget::Mission {
            mission: mission.clone(),
        },
        required: scope,
    })
    .map_err(|error| admission_error(&error))?;
    // Admission matched the target; this states the invariant the writes
    // rely on rather than trusting the path a second time.
    if !matches!(
        admitted.capability(),
        CollaborationCapability::PilotMission { mission: bound } if *bound == mission
    ) {
        return Err(binding_missing());
    }
    Ok(Door {
        admitted,
        bindings,
        store: PresenceStore::new(&state_dir),
        mission,
    })
}

/// The path's `{attachment}` must be the one the caller proved. Another
/// attachment, even of the same mission, is refused and never rewritten.
fn own_attachment(door: &Door, path: &str) -> Result<AttachmentId, ApiError> {
    let own = door.admitted.attachment();
    if path != own.as_str() {
        return Err(binding_missing());
    }
    Ok(own.clone())
}

fn commit<T>(door: &Door, effect: impl FnOnce() -> Result<T, ApiError>) -> Result<T, ApiError> {
    commit_collaboration(&door.bindings, &door.admitted, effect).map_err(|error| match error {
        CommitError::Binding(error) => admission_error(&error.into()),
        CommitError::Effect(error) => error,
    })
}

/// Wire projection of one record. The retry key stays with its writer; no
/// host fact, role or lease field exists to project.
fn view(record: &RemotePresence, caller: &AttachmentId) -> Value {
    let mut value = json!({
        "attachment": record.attachment,
        "attached_at": record.attached_at,
        "heartbeat_at": record.heartbeat_at,
        "self": record.attachment == *caller,
    });
    if let Some(label) = &record.label {
        value["label"] = json!(label);
    }
    value
}

fn envelope(door: &Door) -> Value {
    json!({"schema_version": 1, "received_at": Utc::now(), "mission": door.mission})
}

fn own_view(door: &Door, record: &RemotePresence) -> Value {
    let mut value = view(record, door.admitted.attachment());
    value["mission"] = json!(record.mission);
    value
}

/// S-ATTACH: announce the bound attachment on its mission, idempotent by key.
pub(crate) async fn attach(
    State(state): State<Arc<AppState>>,
    Path(mission): Path<String>,
    headers: HeaderMap,
    body: Result<Json<AttachBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &mission, CollaborationScope::SessionsWrite)?;
    let Json(body) = body.map_err(|_| invalid())?;
    commit(&door, || {
        let (record, written) = door
            .store
            .attach_remote(
                &door.mission,
                door.admitted.attachment(),
                AttachRequest {
                    idempotency_key: body.idempotency_key,
                    label: body.label,
                },
                Utc::now(),
            )
            .map_err(store_error)?;
        let mut output = envelope(&door);
        output["attachment"] = own_view(&door, &record);
        output["duplicate"] = json!(!written);
        Ok(Json(output))
    })
}

/// S-BEAT: record a server-observed heartbeat, at most one per 10 seconds.
pub(crate) async fn heartbeat(
    State(state): State<Arc<AppState>>,
    Path((mission, attachment)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &mission, CollaborationScope::SessionsWrite)?;
    let attachment = own_attachment(&door, &attachment)?;
    commit(&door, || {
        let record = door
            .store
            .heartbeat_remote(&door.mission, &attachment, Utc::now())
            .map_err(store_error)?;
        let mut output = envelope(&door);
        output["attachment"] = own_view(&door, &record);
        Ok(Json(output))
    })
}

/// S-DETACH: withdraw the bound attachment's presence; safe to repeat.
pub(crate) async fn detach(
    State(state): State<Arc<AppState>>,
    Path((mission, attachment)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &mission, CollaborationScope::SessionsWrite)?;
    let attachment = own_attachment(&door, &attachment)?;
    commit(&door, || {
        let detached = door
            .store
            .detach_remote(&door.mission, &attachment)
            .map_err(store_error)?;
        let mut output = envelope(&door);
        output["attachment"] = json!(attachment);
        output["detached"] = json!(detached);
        Ok(Json(output))
    })
}

/// S-PEERS: one page of the mission's attachments. Reads only; it takes no
/// lock and creates no file.
pub(crate) async fn peers(
    State(state): State<Arc<AppState>>,
    Path(mission): Path<String>,
    headers: HeaderMap,
    query: Result<Query<PeersQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &mission, CollaborationScope::SessionsRead)?;
    let Query(query) = query.map_err(|_| invalid())?;
    let limit = query.limit.unwrap_or(PEERS_PAGE);
    if limit == 0 || limit > PEERS_PAGE {
        return Err(invalid());
    }
    let after = query
        .after
        .map(AttachmentId::new)
        .transpose()
        .map_err(|_| invalid())?;
    let records = door
        .store
        .remote_presences(&door.mission)
        .map_err(store_error)?;
    let (page, next) =
        cosmon_core::remote_presence::page_peers(&records, &door.mission, after.as_ref(), limit);
    let mut output = envelope(&door);
    output["attachments"] = json!(page
        .into_iter()
        .map(|record| view(record, door.admitted.attachment()))
        .collect::<Vec<_>>());
    output["next_cursor"] = json!(next);
    Ok(Json(output))
}
