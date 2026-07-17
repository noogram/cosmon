// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/noyaux` — discovery endpoint for multi-noyau operators.
//!
//! Returns the list of noyaux the JWT's `sub` is bound to, with the
//! per-noyau binding count and the absolute `galaxies_root` on the
//! adapter host. A multi-noyau operator can call `/v1/noyaux` once
//! and then pick the noyau to scope subsequent calls against —
//! without first guessing a slug.
//!
//! Pipeline:
//!
//! 1. Extract `Authorization: Bearer <jwt>`; 401 if missing.
//! 2. Validate JWT (clause a) → `ValidatedJwt`.
//! 3. **No scope check.** A valid JWT is the whole gate — discovery
//!    surface, not a state-mutating verb (same class as
//!    `/v1/auth/me`).
//! 4. Filter the sealed nucleon map by the JWT's `sub` claim. Empty
//!    list when the principal is not yet bound — 200 OK with
//!    `noyaux: []`, never 401.
//! 5. Project to the wire shape.
//!
//! The route is admission-side and intentionally exempt from the §8p
//! molecule⇔verb bijection check via the `/v1/noyaux` path filter in
//! `tests/api_surface_freeze.rs::is_adapter_only`. No `cs` CLI verb
//! counterpart — multi-noyau enumeration is a wire-side concern.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, RppRejectReason};
use crate::jwt::JwtVerifier;
use crate::AppState;

/// Body schema for `GET /v1/noyaux`.
#[derive(Debug, Serialize, Deserialize)]
pub struct NoyauxResponse {
    /// One row per noyau visible to the JWT's `sub`. Empty when the
    /// principal carries a valid JWT but has no nucleon binding.
    pub noyaux: Vec<NoyauEntry>,
}

/// Single row of [`NoyauxResponse::noyaux`]. Stable, additive-only
/// shape — adding a field is allowed; renaming or removing one is a
/// §8p break that must update the `OpenAPI` spec and the
/// cosmon-remote client in the same change.
#[derive(Debug, Serialize, Deserialize)]
pub struct NoyauEntry {
    /// Noyau identifier (e.g. `"tenant-demo-sandbox"`,
    /// `"operator-sandbox"`).
    pub id: String,
    /// Number of `(iss, sub) → noyau` bindings backing this noyau for
    /// the JWT's `sub`. ≥ 1 by construction (zero-count rows are not
    /// emitted).
    pub binding_count: usize,
    /// Absolute path to the noyau's galaxy tree on the adapter host
    /// (`<galaxies_root>/<id>`). The operator-host CLI uses this to
    /// scope subprocess invocations.
    pub galaxies_root: String,
}

/// `GET /v1/noyaux`. See module docs for the pipeline.
pub async fn list_noyaux(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    // 1. Authorization header.
    let token = extract_bearer(&headers)?;

    // 2. JWT validation — clause (a). Audit-side detail stays in the
    //    audit channel; the wire body carries the stable label.
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;

    // 3. No scope check — discovery semantics (same as /v1/auth/me).

    // 4. Filter the sealed nucleon map by sub.
    let rows = state.nucleon_map.load().noyaux_for_sub(&jwt.sub);

    // 5. Project to the wire shape. Resolving each noyau's
    //    galaxies_root in the response keeps the operator-host CLI
    //    free from inferring `<galaxies_root>/<noyau>` itself.
    let noyaux = rows
        .into_iter()
        .map(|(noyau, binding_count)| {
            let path = state.galaxies_root.join(noyau.as_str());
            NoyauEntry {
                id: noyau.as_str().to_owned(),
                binding_count,
                galaxies_root: path.to_string_lossy().into_owned(),
            }
        })
        .collect();

    let body = NoyauxResponse { noyaux };
    let value = serde_json::to_value(&body)
        .map_err(|_| ApiError::internal("noyaux_serialization_failed"))?;
    Ok(Json(value))
}

/// Extract the JWT bearer from the `Authorization` header. Module-
/// private so the discovery surface can evolve its accepted spellings
/// independently of the molecule routes.
fn extract_bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(|| ApiError::from_reject(&RppRejectReason::MissingAuthorization, None))?;
    let s = header
        .to_str()
        .map_err(|_| ApiError::from_reject(&RppRejectReason::MalformedJwt, None))?;
    let stripped = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .ok_or_else(|| ApiError::from_reject(&RppRejectReason::MalformedJwt, None))?;
    Ok(stripped.trim())
}
