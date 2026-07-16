// SPDX-License-Identifier: AGPL-3.0-only

//! Operator-sealed admin provisioning + reload routes
//! (B2 — impl of B1 design
//! `docs/admin-provisioning-design.md`).
//!
//! Auth is a HOST-SIDE SEAL ([`crate::admin_seal::AdminSeal`]), disjoint
//! from the tenant OIDC chain. These routes are the audited renderer
//! over HTTP: they call the SAME `build_binding` /
//! `render_oidc_identity_toml` the operator used by hand, write the
//! binding into `<state_dir>/nucleons/<id>/`, and reload the map
//! **in-process** — no `SIGHUP`, no reboot, no dropped tmux worker. They
//! never widen the deny-by-default binding semantics; they only ADD
//! admitted `(iss, sub) → noyau` lines.
//!
//! Classification: principal `operator`, exposure `adapter-only`, scope
//! `-` (auth is the seal, not `OAuth2`). No `#[verb]` twin — the operator
//! surface is fundamentally an HTTP-ingress concern.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;
use axum::Json as JsonBody;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::new_request_id;
use crate::error::ApiError;
use crate::nucleon_map::HabilitationBindingSpec;
use crate::portee::{PartnerIdentity, PorteeSpec};
use crate::AppState;

/// Request body for `POST /v1/admin/habilitations`. Mirror of the four
/// authorization params `build_binding` validates (design §3.2).
#[derive(Debug, Deserialize)]
pub struct ProvisionHabilitationBody {
    /// Tenant axis. State materialises at `<galaxies_root>/<noyau>/`.
    pub noyau: String,
    /// Directory name under `nucleons/`. Defaults to `noyau`.
    #[serde(default)]
    pub habilitation_id: Option<String>,
    /// The pinned `(iss, sub, aud)` triple.
    pub oidc: OidcBody,
    /// Binding-granted scopes (T23). Empty ⇒ no `[scopes]` section.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Materialise `<galaxies_root>/<noyau>/` if absent. Default `true`.
    #[serde(default = "default_true")]
    pub create_noyau: bool,
}

/// The pinned JWT claim triple carried in the request body.
#[derive(Debug, Deserialize)]
pub struct OidcBody {
    /// JWT `iss` claim — absolute `http(s)://` URL, byte-for-byte equal
    /// to the `IdP` and the minted JWT.
    pub issuer: String,
    /// JWT `sub` claim — the principal identifier.
    pub sub: String,
    /// JWT `aud` claim — pinned to this deployment.
    pub audience: String,
}

const fn default_true() -> bool {
    true
}

/// Response body for a successful provision.
#[derive(Debug, Serialize)]
pub struct ProvisionedHabilitation {
    /// Correlation id (also in the audit log).
    pub request_id: String,
    /// Directory name under `nucleons/`.
    pub habilitation_id: String,
    /// Tenant axis bound.
    pub noyau: String,
    /// Absolute path to the written `oidc-identity.toml`.
    pub binding_path: String,
    /// BLAKE3 hash of the rendered file body (verification, never the
    /// admin token).
    pub seal: String,
    /// Always `true` here — the map was reloaded in-process (no SIGHUP).
    pub reloaded: bool,
    /// `true` ⇒ this call materialised the noyau's galaxy tree.
    pub noyau_created: bool,
}

/// `POST /v1/admin/habilitations` — provision one habilitation
/// (operator-sealed). `201` on a fresh binding, `200` on an idempotent
/// re-provision.
pub async fn provision_habilitation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<JsonBody<ProvisionHabilitationBody>, axum::extract::rejection::JsonRejection>,
) -> Result<(axum::http::StatusCode, Json<ProvisionedHabilitation>), ApiError> {
    // 1. SEALED OPERATOR AUTH — disjoint from OIDC, fail-closed. The
    //    seal is checked BEFORE the body is even inspected so a tenant
    //    JWT never reaches the renderer.
    state.admin_seal.require(&headers)?;

    // 2. Parse + validate the body into the audited spec.
    let JsonBody(b) = body.map_err(|_| {
        ApiError::with_status(axum::http::StatusCode::BAD_REQUEST, "malformed_binding")
    })?;
    let spec = HabilitationBindingSpec {
        noyau: b.noyau.clone(),
        sub: b.oidc.sub,
        issuer: b.oidc.issuer,
        audience: b.oidc.audience,
        nucleon_id: b.habilitation_id,
        phase: None,
        scopes: b.scopes,
        sealed_at: None,
    };

    // 3. Provision (single-writer, atomic, in-process reload + verify).
    let out = state.provisioner.provision(&spec, b.create_noyau).await?;

    Ok((
        out.status_code(),
        Json(ProvisionedHabilitation {
            request_id: new_request_id(),
            habilitation_id: out.habilitation_id,
            noyau: out.noyau,
            binding_path: out.binding_path.display().to_string(),
            seal: format!("blake3:{}", out.seal),
            reloaded: true,
            noyau_created: out.noyau_created,
        }),
    ))
}

/// `GET /v1/admin/habilitations` — list provisioned habilitations
/// (operator introspection). Never returns the admin seal nor any
/// secret — only the binding envelope.
pub async fn list_habilitations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    let summaries = state.nucleon_map.load().summaries();
    Ok(Json(json!({
        "habilitations": summaries,
        "count": summaries.len(),
    })))
}

/// `DELETE /v1/admin/habilitations/{id}` — revoke a habilitation by
/// directory id (remove the binding + reload).
pub async fn revoke_habilitation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    let outcome = state.provisioner.revoke(&id).await?;
    Ok(Json(json!({
        "request_id": new_request_id(),
        "habilitation_id": id,
        "revoked": true,
        "reloaded": true,
        "bindings_after": outcome.bindings_after,
    })))
}

// ─── Portée tooling — one-gesture federation (ADR-0023 G5) ──────────────

/// Request body for `POST /v1/admin/federations`. The one gesture:
/// *« fédère `<partner>` sur `<galaxies>` »*. Materialises N per-galaxy
/// habilitations and groups them as one relation.
#[derive(Debug, Deserialize)]
pub struct FederateBody {
    /// Relation id. Defaults to the partner `sub` when omitted.
    #[serde(default)]
    pub portee_id: Option<String>,
    /// The foreign identity to federate with (`iss`, `sub`).
    pub partner: PartnerBody,
    /// Galaxies the partner may open. Non-empty.
    pub galaxies: Vec<String>,
    /// Scopes granted on every galaxy of the relation. Empty ⇒ no
    /// `[scopes]` section (JWT-scopes-only admission) on each binding.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Materialise each galaxy's tree if absent. Default `true`.
    #[serde(default = "default_true")]
    pub create_noyau: bool,
}

/// The foreign identity carried in [`FederateBody`].
#[derive(Debug, Deserialize)]
pub struct PartnerBody {
    /// JWT `iss` — the peer instance `IdP`, absolute `http(s)://` URL.
    pub issuer: String,
    /// JWT `sub` — the partner principal.
    pub sub: String,
}

/// `POST /v1/admin/federations` — one gesture → N habilitations grouped
/// as one portée (operator-sealed). `201` on a fresh relation, `200`
/// when extending an existing one (additive union).
pub async fn federate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<JsonBody<FederateBody>, axum::extract::rejection::JsonRejection>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    state.admin_seal.require(&headers)?;
    let JsonBody(b) = body.map_err(|_| {
        ApiError::with_status(axum::http::StatusCode::BAD_REQUEST, "malformed_portee")
    })?;
    let spec = PorteeSpec {
        portee_id: b.portee_id,
        partner: PartnerIdentity {
            issuer: b.partner.issuer,
            sub: b.partner.sub,
        },
        galaxies: b.galaxies,
        scopes: b.scopes,
        create_noyau: b.create_noyau,
        created_at: None,
    };
    let out = state.portee_provisioner.federate(&spec).await?;
    let status = if out.created {
        axum::http::StatusCode::CREATED
    } else {
        axum::http::StatusCode::OK
    };
    Ok((
        status,
        Json(json!({
            "request_id": new_request_id(),
            "portee_id": out.portee_id,
            "created": out.created,
            "members": out.members,
            "galaxies_created": out.galaxies_created,
            "reloaded": true,
        })),
    ))
}

/// `GET /v1/admin/federations` — list portées (grouped relations).
pub async fn list_federations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    let federations = state.portee_provisioner.list();
    Ok(Json(json!({
        "federations": federations,
        "count": federations.len(),
    })))
}

/// `DELETE /v1/admin/federations/{id}` — dissolve a whole relation
/// (revoke every backing habilitation + remove the manifest).
pub async fn dissolve_federation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    let revoked = state.portee_provisioner.dissolve(&id).await?;
    Ok(Json(json!({
        "request_id": new_request_id(),
        "portee_id": id,
        "dissolved": true,
        "habilitations_revoked": revoked,
        "reloaded": true,
    })))
}

/// `DELETE /v1/admin/federations/{id}/galaxies/{galaxy}` — revoke one
/// galaxy from a relation, leaving the rest intact.
pub async fn revoke_federation_galaxy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, galaxy)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    let view = state.portee_provisioner.revoke_galaxy(&id, &galaxy).await?;
    Ok(Json(json!({
        "request_id": new_request_id(),
        "portee_id": id,
        "galaxy_revoked": galaxy,
        "remaining": view.members,
        "reloaded": true,
    })))
}

/// `POST /v1/admin/reload` — standalone "reload à chaud": re-read every
/// on-disk binding and atomically publish the new map, picking up
/// bindings staged by ANY channel (host-side `.toml` edit) without a
/// reboot. Idempotent — a reload with no on-disk change is a no-op swap.
pub async fn reload_habilitations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    let outcome = state.provisioner.reload().await;
    if let Some(err) = &outcome.error {
        tracing::error!(event = "admin.reload", error = %err, "reload failed");
        return Err(ApiError::with_status(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reload_failed",
        ));
    }
    Ok(Json(json!({
        "request_id": new_request_id(),
        "reloaded": true,
        "bindings_before": outcome.bindings_before,
        "bindings_after": outcome.bindings_after,
        "new_noyaux": outcome.new_noyaux.iter().map(crate::nucleon_map::Noyau::as_str).collect::<Vec<_>>(),
    })))
}
