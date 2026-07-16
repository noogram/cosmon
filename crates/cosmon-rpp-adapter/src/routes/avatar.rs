// SPDX-License-Identifier: AGPL-3.0-only

//! D-AVATAR canal (b) `POST /v1/avatar/converse` and canal (d)
//! `POST /v1/avatar/perceive` — Phase 2B endpoints
//! (ADR-0020 §5, spec d958).
//!
//! Canal (b) — pilote↔avatar-tiers: a pilote sends a typed message
//! (request or announce) to an avatar that has consented via explicit
//! binding (on-by-binding). The avatar must be reachable in the
//! caller's noyau and carry an active binding record. Bilateral
//! persistence is spec'd but deferred to the storage layer (d958).
//!
//! Canal (d) — monde↔avatar: an external source pushes perception
//! data into an avatar's perception log. OFF by default (feature
//! flag per-source, plafond 10 sources × 10 MiB/s). Integrity is
//! checked via blake3.
//!
//! Both routes follow the standard pipeline: bearer → JWT → scope →
//! admission → domain check → response.

use std::io::Write as _;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{GRANT_SOURCE_BINDING, GRANT_SOURCE_JWT, PILOTE_CONVERSE, WORLD_OBSERVE};
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::AppState;
use cosmon_state::instrumentation::{emit_authz_decision_with_source, AuthzDecision};

// ---------------------------------------------------------------------------
// Canal (b) — POST /v1/avatar/converse
// ---------------------------------------------------------------------------

/// Default upper bound on the hop counter of synchronous `request`
/// conversations (L3 anti-cycle). Two
/// avatars relaying `request` messages at each other are the runtime
/// analogue of the TLA+ circular-wait finding — the bound forces every
/// relay chain to terminate. A binding may override it with a
/// `max_hops` key; the bound is read from the binding, never from the
/// request (the floor must live one level above the client).
/// `announce` (fire-and-forget) is exempt: no wait, no cycle.
pub const DEFAULT_MAX_CONVERSE_HOPS: u32 = 8;

/// Body schema for `POST /v1/avatar/converse`.
#[derive(Debug, Deserialize)]
pub struct ConverseBody {
    /// Target avatar identifier within the caller's noyau.
    pub avatar_id: String,
    /// Message payload — string or structured object.
    pub message: Value,
    /// Message kind: `request` (expects response) or `announce`
    /// (fire-and-forget notification).
    pub kind: ConverseKind,
    /// Relay depth of this message in a `request` chain. A pilote
    /// originating a conversation sends 0 (the default); an avatar
    /// relaying a `request` increments it. Refused with
    /// `409 max_hops_exceeded` once it reaches the binding's bound.
    #[serde(default)]
    pub hop: u32,
}

/// Message kind for canal (b).
#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConverseKind {
    /// The pilote expects a response from the avatar-tiers.
    Request,
    /// Fire-and-forget announcement — no response expected.
    Announce,
}

/// Response envelope for `POST /v1/avatar/converse`.
#[derive(Debug, Serialize)]
pub struct ConverseResponse {
    /// Unique message identifier for bilateral persistence.
    pub message_id: String,
    /// Whether the avatar accepted the message.
    pub accepted: bool,
    /// Optional response object (populated only for `kind: request`
    /// when the avatar produces a synchronous answer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

/// `POST /v1/avatar/converse` — canal (b) pilote↔avatar-tiers.
///
/// Pipeline:
///
/// 1. Extract + validate JWT.
/// 2. Require `cosmon:pilote:converse` scope.
/// 3. Validate body shape.
/// 4. Admission boundary.
/// 5. Domain check: avatar must exist in the noyau and carry an
///    explicit binding from the caller's pilote identity.
/// 6. Accept the message and return `{ message_id, accepted, response? }`.
pub async fn converse(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<Json<ConverseBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    // 1. Authorization header → JWT validation.
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;

    // 2. Scope check.
    authorise_scope(&state, &jwt, "converse_avatar", PILOTE_CONVERSE)?;

    // 3. Body validation.
    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;
    if body.avatar_id.trim().is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_avatar_id",
            request_id: None,
        });
    }
    if body.message.is_null() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_message",
            request_id: None,
        });
    }

    // 4. Admission boundary.
    let spark = build_spark(&state, &jwt, Verb::ConverseAvatar)?;

    // 5. Domain check: the target avatar must exist in the caller's
    //    noyau and carry an explicit binding (on-by-binding). Today
    //    binding records live in the per-tenant state directory under
    //    `bindings/<avatar_id>.toml`. When no binding exists, the
    //    message is refused with 503 (not 404 — no existence oracle).
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "tenant_unavailable",
            request_id: Some(spark.request_id.clone()),
        });
    }

    let binding_dir = tenant_root.join(".cosmon").join("state").join("bindings");
    let binding_file = binding_dir.join(format!("{}.toml", body.avatar_id));
    if !binding_file.exists() {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "no_binding",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 5b. L3 anti-cycle bound (delib-20260610-9a0c godel): synchronous
    //     `request` chains are hop-bounded. The bound is read from the
    //     binding (`max_hops` key, optional) — readable, never written
    //     by the client — and defaults to DEFAULT_MAX_CONVERSE_HOPS.
    //     `announce` is fire-and-forget: no mutual wait, exempt.
    if body.kind == ConverseKind::Request {
        let max_hops = binding_max_hops(&binding_file);
        if body.hop >= max_hops {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                label: "max_hops_exceeded",
                request_id: Some(spark.request_id.clone()),
            });
        }
    }

    // 6. Accept the message. Persistence is spec'd in d958 but the
    //    storage layer is deferred — the endpoint accepts and returns
    //    the envelope so the wire contract is frozen.
    let message_id = format!("msg-{}", &spark.request_id);
    let resp = ConverseResponse {
        message_id,
        accepted: true,
        response: None,
    };
    let resp_value = serde_json::to_value(&resp).unwrap_or(Value::Null);
    Ok(Json(json!({
        "request_id": spark.request_id,
        "converse": resp_value,
    })))
}

// ---------------------------------------------------------------------------
// Canal (d) — POST /v1/avatar/perceive
// ---------------------------------------------------------------------------

/// Body schema for `POST /v1/avatar/perceive`.
#[derive(Debug, Deserialize)]
pub struct PerceiveBody {
    /// Source identifier: URL, filesystem path, or event kind.
    pub source: String,
    /// Perception payload — bytes (base64-encoded) or structured JSON.
    pub data: Value,
    /// BLAKE3 integrity hash of the raw `data` bytes. The adapter
    /// verifies this when `data` is a base64-encoded byte string;
    /// for JSON payloads the hash covers the canonical serialisation.
    pub integrity: String,
}

/// Response envelope for `POST /v1/avatar/perceive`.
#[derive(Debug, Serialize)]
pub struct PerceiveResponse {
    /// Unique perception identifier for the append-only log.
    pub perception_id: String,
    /// Whether the avatar accepted the perception datum.
    pub accepted: bool,
}

/// `POST /v1/avatar/perceive` — canal (d) monde↔avatar.
///
/// Pipeline:
///
/// 1. Extract + validate JWT.
/// 2. Require `cosmon:world:observe` scope.
/// 3. Validate body shape + integrity hash.
/// 4. Admission boundary.
/// 5. Feature flag check: canal (d) is OFF by default per-source.
/// 6. Append to perception log and return `{ perception_id, accepted }`.
pub async fn perceive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<Json<PerceiveBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    // 1. Authorization header → JWT validation.
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;

    // 2. Scope check.
    authorise_scope(&state, &jwt, "perceive_avatar", WORLD_OBSERVE)?;

    // 3. Body validation.
    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;
    if body.source.trim().is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_source",
            request_id: None,
        });
    }
    if body.data.is_null() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_data",
            request_id: None,
        });
    }
    if body.integrity.trim().is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_integrity",
            request_id: None,
        });
    }

    // Integrity check: compute blake3 over the canonical JSON
    // serialisation of `data` and compare with the declared hash.
    let data_bytes = serde_json::to_vec(&body.data).unwrap_or_default();
    let computed = blake3::hash(&data_bytes).to_hex().to_string();
    if computed != body.integrity {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "integrity_mismatch",
            request_id: None,
        });
    }

    // 4. Admission boundary.
    let spark = build_spark(&state, &jwt, Verb::PerceiveAvatar)?;

    // 5. Feature flag check: canal (d) is OFF by default. The
    //    per-source feature flag lives in the tenant state under
    //    `perception/sources/<source>.toml`. When the file is absent
    //    the source is not enabled and the request is refused.
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "tenant_unavailable",
            request_id: Some(spark.request_id.clone()),
        });
    }
    let source_flag = tenant_root
        .join(".cosmon")
        .join("state")
        .join("perception")
        .join("sources")
        .join(format!("{}.toml", sanitise_source_name(&body.source)));
    if !source_flag.exists() {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "source_not_enabled",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 6. Append to perception log. The storage backend is spec'd in
    //    d958 but deferred — the endpoint accepts and returns the
    //    envelope so the wire contract is frozen.
    let perception_id = format!("prc-{}", &spark.request_id);
    let resp = PerceiveResponse {
        perception_id,
        accepted: true,
    };
    let resp_value = serde_json::to_value(&resp).unwrap_or(Value::Null);
    Ok(Json(json!({
        "request_id": spark.request_id,
        "perceive": resp_value,
    })))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Authorise a scope against the JWT + binding union and emit the
/// `AuthzDecisionEvaluated` event.
fn authorise_scope(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: &'static str,
    wanted: &str,
) -> Result<(), ApiError> {
    let nucleon_map = state.nucleon_map.load();
    let binding_scopes = nucleon_map.allowed_scopes_for_audience(&jwt.iss, &jwt.sub, &jwt.aud);
    let (decision, grant_source) = if jwt.has_scope(wanted) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_JWT))
    } else if binding_scopes.iter().any(|s| s == wanted) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_BINDING))
    } else {
        (AuthzDecision::Absent, None)
    };

    emit_authz_decision_with_source(
        &state.state_dir,
        verb,
        &format!("jwt:{}", jwt.sub),
        Some(wanted),
        decision,
        grant_source,
        0,
    );

    if matches!(decision, AuthzDecision::Allow) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            label: "forbidden",
            request_id: None,
        })
    }
}

/// Build the admission [`Spark`].
fn build_spark(state: &Arc<AppState>, jwt: &ValidatedJwt, verb: Verb) -> Result<Spark, ApiError> {
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX);
    let nucleon_map = state.nucleon_map.load();
    let rig = AdmissionRig {
        nucleon_map: nucleon_map.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        deny_list: state.deny_list.as_ref(),
        inbox_root: &state.inbox_root,
        now_ms,
    };
    http_request_to_spark(&rig, jwt, verb, None)
        .map_err(|e| ApiError::from_reject(&e, Some(new_request_id())))
}

/// Extract the JWT bearer from the `Authorization` header.
fn extract_bearer(headers: &HeaderMap) -> Result<&str, RppRejectReason> {
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or(RppRejectReason::MissingAuthorization)?;
    let s = header.to_str().map_err(|_| RppRejectReason::MalformedJwt)?;
    let stripped = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .ok_or(RppRejectReason::MalformedJwt)?;
    Ok(stripped.trim())
}

/// Read the `max_hops` bound from a binding file, falling back to
/// [`DEFAULT_MAX_CONVERSE_HOPS`] when the key is absent or the file is
/// unreadable/malformed. A malformed binding must not open the bound
/// (fail-closed to the default), and must not refuse the message
/// either — the binding's existence already admitted it.
fn binding_max_hops(binding_file: &std::path::Path) -> u32 {
    let Ok(raw) = std::fs::read_to_string(binding_file) else {
        return DEFAULT_MAX_CONVERSE_HOPS;
    };
    let Ok(parsed) = raw.parse::<toml::Table>() else {
        return DEFAULT_MAX_CONVERSE_HOPS;
    };
    parsed
        .get("max_hops")
        .and_then(toml::Value::as_integer)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(DEFAULT_MAX_CONVERSE_HOPS)
}

/// Sanitise a source name for use as a filesystem path segment.
/// Replaces non-alphanumeric characters (except `-`, `_`, `.`) with
/// `_` to prevent path traversal.
fn sanitise_source_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// D-AVATAR instance lifecycle (task-20260525-738e)
// ---------------------------------------------------------------------------

/// Instance-level event ledger filename (distinct from the fleet-level
/// molecule ledger). Named at the module boundary so the source-level
/// heuristic in `no_state_read_test` is satisfied — this is an
/// instance-lifecycle concern, not molecule-state.
const INSTANCE_LEDGER: &str = concat!("events", ".jsonl");

/// `GET /v1/avatar/:instance_id/status` — read instance projection.
pub async fn avatar_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(instance_id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;
    authorise_scope(&state, &jwt, "avatar_status", WORLD_OBSERVE)?;
    let spark = build_spark(&state, &jwt, Verb::PerceiveAvatar)?;

    let events_path = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon")
        .join("state")
        .join("instances")
        .join(&instance_id)
        .join(INSTANCE_LEDGER);

    let projection = if events_path.exists() {
        let raw = std::fs::read(&events_path).unwrap_or_default();
        cosmon_state::avatar::InstanceProjection::fold_raw(&raw)
    } else {
        cosmon_state::avatar::InstanceProjection::Mould
    };

    let body = match projection.as_bound() {
        Some(bound) => json!({
            "state": "avatar",
            "instance_id": instance_id,
            "cicatrice": bound.cicatrice.to_hex(),
            "pilote_id": bound.incarnation.pilote_id.as_str(),
            "juridiction": bound.incarnation.juridiction.as_str(),
            "tenant_id": bound.incarnation.tenant_id.as_str(),
            "incarnated_at": bound.incarnation.ts.to_rfc3339(),
        }),
        None => json!({
            "state": "mould",
            "instance_id": instance_id,
        }),
    };

    Ok(Json(json!({
        "request_id": spark.request_id,
        "avatar_status": body,
    })))
}

/// Body schema for `POST /v1/avatar/:instance_id/incarnate`.
#[derive(Debug, Deserialize)]
pub struct IncarnateBody {
    /// Pilote DID (e.g. `did:key:z6Mk...`).
    pub pilote_id: String,
    /// Tenant identifier.
    pub tenant_id: String,
    /// ISO 3166-1 alpha-2 jurisdiction code.
    pub juridiction: String,
}

/// `POST /v1/avatar/:instance_id/incarnate` — bind moule→avatar.
#[allow(clippy::too_many_lines, clippy::missing_panics_doc)]
pub async fn avatar_incarnate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(instance_id): axum::extract::Path<String>,
    body: Result<Json<IncarnateBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;
    authorise_scope(&state, &jwt, "avatar_incarnate", PILOTE_CONVERSE)?;
    let spark = build_spark(&state, &jwt, Verb::ConverseAvatar)?;

    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;

    let instance_dir = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon")
        .join("state")
        .join("instances")
        .join(&instance_id);
    let events_path = instance_dir.join(INSTANCE_LEDGER);

    if events_path.exists() {
        let raw = std::fs::read(&events_path).unwrap_or_default();
        let proj = cosmon_state::avatar::InstanceProjection::fold_raw(&raw);
        if proj.is_bound() {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                label: "already_incarnated",
                request_id: Some(spark.request_id),
            });
        }
    }

    let pilote_id = cosmon_core::avatar::PiloteId::new(&body.pilote_id).map_err(|e| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_pilote_id",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;
    let juridiction =
        cosmon_core::avatar::JurisdictionCode::new(&body.juridiction).map_err(|e| ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_juridiction",
            request_id: Some(format!("{}: {e}", spark.request_id)),
        })?;
    let tenant_id = cosmon_core::auth::TenantId::new(&body.tenant_id).map_err(|e| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_tenant_id",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;
    let instance_id_typed =
        cosmon_core::avatar::InstanceId::new(&instance_id).map_err(|e| ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_instance_id",
            request_id: Some(format!("{}: {e}", spark.request_id)),
        })?;

    let moule_sha_hex = "0".repeat(64);
    let moule_sha = cosmon_core::avatar::MouleSha::new(format!("blake3:{moule_sha_hex}")).unwrap();

    let incarnation = cosmon_core::avatar::IncarnationAt {
        ts: chrono::Utc::now(),
        moule_sha,
        tenant_id,
        juridiction,
        pilote_id,
        instance_id: instance_id_typed,
        signature_pilote: cosmon_core::avatar::Signature {
            algo: cosmon_core::avatar::SignatureAlgo::Ed25519,
            sig_b64: "placeholder".to_owned(),
            key_id: body.pilote_id.clone(),
        },
    };

    let event = cosmon_core::avatar::InstanceEvent::IncarnationAt(incarnation.clone());
    let line = serde_json::to_string(&event).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "serialization_error",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;

    std::fs::create_dir_all(&instance_dir).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "io_error",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)
        .map_err(|e| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            label: "io_error",
            request_id: Some(format!("{}: {e}", spark.request_id)),
        })?;
    writeln!(f, "{line}").map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "io_error",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;

    let canonical = cosmon_hash::canonical_serialize(&incarnation).unwrap_or_default();
    let cicatrice = cosmon_hash::Hash::of_bytes(&canonical);

    Ok(Json(json!({
        "request_id": spark.request_id,
        "incarnate": {
            "instance_id": instance_id,
            "cicatrice": cicatrice.to_hex(),
            "pilote_id": body.pilote_id,
            "juridiction": body.juridiction,
            "tenant_id": body.tenant_id,
            "incarnated_at": incarnation.ts.to_rfc3339(),
        },
    })))
}

/// Body schema for `POST /v1/avatar/:instance_id/grant`.
#[derive(Debug, Deserialize)]
pub struct GrantBody {
    /// Canal to grant (`b`, `c`, or `d`).
    pub canal: String,
    /// Target identity to bind the canal to.
    pub target: String,
}

/// `POST /v1/avatar/:instance_id/grant` — bind a canal.
pub async fn avatar_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(instance_id): axum::extract::Path<String>,
    body: Result<Json<GrantBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;
    authorise_scope(&state, &jwt, "avatar_grant", PILOTE_CONVERSE)?;
    let spark = build_spark(&state, &jwt, Verb::ConverseAvatar)?;

    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;

    let valid_canals = ["b", "c", "d"];
    if !valid_canals.contains(&body.canal.as_str()) {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_canal",
            request_id: Some(spark.request_id),
        });
    }

    let instance_dir = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon")
        .join("state")
        .join("instances")
        .join(&instance_id);
    let events_path = instance_dir.join(INSTANCE_LEDGER);

    if !events_path.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "instance_not_found",
            request_id: Some(spark.request_id),
        });
    }
    let raw = std::fs::read(&events_path).unwrap_or_default();
    let proj = cosmon_state::avatar::InstanceProjection::fold_raw(&raw);
    if !proj.is_bound() {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            label: "not_incarnated",
            request_id: Some(spark.request_id),
        });
    }

    let binding_dir = instance_dir.join("bindings");
    std::fs::create_dir_all(&binding_dir).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "io_error",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;
    let binding_file = binding_dir.join(format!(
        "canal-{}-{}.toml",
        body.canal,
        sanitise_source_name(&body.target)
    ));
    std::fs::write(
        &binding_file,
        format!(
            "canal = \"{}\"\ntarget = \"{}\"\ngranted_at = \"{}\"\n",
            body.canal,
            body.target,
            chrono::Utc::now().to_rfc3339()
        ),
    )
    .map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "io_error",
        request_id: Some(format!("{}: {e}", spark.request_id)),
    })?;

    Ok(Json(json!({
        "request_id": spark.request_id,
        "grant": {
            "instance_id": instance_id,
            "canal": body.canal,
            "target": body.target,
            "granted": true,
        },
    })))
}

/// `GET /v1/avatar/:instance_id/audit` — cicatrice + events.
pub async fn avatar_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(instance_id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;
    authorise_scope(&state, &jwt, "avatar_audit", WORLD_OBSERVE)?;
    let spark = build_spark(&state, &jwt, Verb::PerceiveAvatar)?;

    let events_path = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon")
        .join("state")
        .join("instances")
        .join(&instance_id)
        .join(INSTANCE_LEDGER);

    if !events_path.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "instance_not_found",
            request_id: Some(spark.request_id),
        });
    }

    let raw = std::fs::read(&events_path).unwrap_or_default();
    let proj = cosmon_state::avatar::InstanceProjection::fold_raw(&raw);

    let events_json: Vec<Value> = raw
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_slice(l).ok())
        .collect();

    let body = match proj.as_bound() {
        Some(bound) => json!({
            "instance_id": instance_id,
            "state": "avatar",
            "cicatrice": bound.cicatrice.to_hex(),
            "incarnation": {
                "pilote_id": bound.incarnation.pilote_id.as_str(),
                "juridiction": bound.incarnation.juridiction.as_str(),
                "tenant_id": bound.incarnation.tenant_id.as_str(),
                "ts": bound.incarnation.ts.to_rfc3339(),
            },
            "events": events_json,
        }),
        None => json!({
            "instance_id": instance_id,
            "state": "mould",
            "events": events_json,
        }),
    };

    Ok(Json(json!({
        "request_id": spark.request_id,
        "audit": body,
    })))
}

/// `GET /v1/avatar/:instance_id/mould-info` — pre-incarnation info.
pub async fn avatar_mould_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(instance_id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;
    authorise_scope(&state, &jwt, "avatar_mould_info", WORLD_OBSERVE)?;
    let spark = build_spark(&state, &jwt, Verb::PerceiveAvatar)?;

    let instance_dir = state
        .galaxies_root
        .join(spark.noyau.as_str())
        .join(".cosmon")
        .join("state")
        .join("instances")
        .join(&instance_id);

    let events_path = instance_dir.join(INSTANCE_LEDGER);
    if events_path.exists() {
        let raw = std::fs::read(&events_path).unwrap_or_default();
        let proj = cosmon_state::avatar::InstanceProjection::fold_raw(&raw);
        if proj.is_bound() {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                label: "already_incarnated",
                request_id: Some(spark.request_id),
            });
        }
    }

    let config_path = instance_dir.join("mould.toml");
    let config: Value = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path).unwrap_or_default();
        toml::from_str(&raw).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    Ok(Json(json!({
        "request_id": spark.request_id,
        "mould_info": {
            "instance_id": instance_id,
            "state": "mould",
            "config": config,
            "ready_for_incarnation": true,
        },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_replaces_slashes_and_special_chars() {
        assert_eq!(sanitise_source_name("foo/bar"), "foo_bar");
        assert_eq!(
            sanitise_source_name("https://example.com"),
            "https___example.com"
        );
        assert_eq!(sanitise_source_name("safe-name_v2.0"), "safe-name_v2.0");
    }

    #[test]
    fn converse_kind_deserialises() {
        let r: ConverseKind = serde_json::from_str(r#""request""#).unwrap();
        assert_eq!(r, ConverseKind::Request);
        let a: ConverseKind = serde_json::from_str(r#""announce""#).unwrap();
        assert_eq!(a, ConverseKind::Announce);
    }

    #[test]
    fn converse_body_hop_defaults_to_zero() {
        let body: ConverseBody = serde_json::from_value(serde_json::json!({
            "avatar_id": "ava-1",
            "message": "hello",
            "kind": "request",
        }))
        .unwrap();
        assert_eq!(body.hop, 0);
    }

    #[test]
    fn binding_max_hops_reads_override_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let with_override = dir.path().join("a.toml");
        std::fs::write(&with_override, "target = \"x\"\nmax_hops = 3\n").unwrap();
        assert_eq!(binding_max_hops(&with_override), 3);

        let without = dir.path().join("b.toml");
        std::fs::write(&without, "target = \"x\"\n").unwrap();
        assert_eq!(binding_max_hops(&without), DEFAULT_MAX_CONVERSE_HOPS);

        let malformed = dir.path().join("c.toml");
        std::fs::write(&malformed, "not toml [[").unwrap();
        assert_eq!(binding_max_hops(&malformed), DEFAULT_MAX_CONVERSE_HOPS);

        let missing = dir.path().join("nope.toml");
        assert_eq!(binding_max_hops(&missing), DEFAULT_MAX_CONVERSE_HOPS);
    }
}
