// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/vitals` — observed, molecule-keyed fleet health for one tenant.
//!
//! Unlike `GET /v1/workers`, this route does not treat a persisted process
//! record as proof of life. It resolves the caller's tenant, obtains that
//! tenant's transport backend, and delegates the complete fold to
//! [`cosmon_state::ops::vitals()`]. The response contains exactly one row per
//! non-terminal molecule, including unassigned backlog rows.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use chrono::Utc;
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_state::ops::{self, VitalsError};
use serde_json::{json, Value};

use crate::admission::Verb;
use crate::auth::scopes::{MOLECULE_READ, MOLECULE_WRITE};
use crate::error::ApiError;
use crate::jwt::JwtVerifier;
use crate::routes::molecules::{
    authorise_scope_public, build_spark_public, extract_bearer, subject_for_jwt,
};
use crate::AppState;

/// Return one tenant's non-terminal molecules with observed worker health.
pub async fn get_vitals(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;
    authorise_scope_public(
        &state,
        &jwt,
        "vitals",
        &[MOLECULE_READ, MOLECULE_WRITE],
        MOLECULE_READ,
    )?;
    let spark = build_spark_public(&state, &jwt, Verb::Vitals, None)?;

    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        });
    }
    let tenant_state_dir = tenant_root.join(".cosmon").join("state");
    let store = FileStore::new(&tenant_state_dir);
    let backend = state.worker_backend.for_tenant(&tenant_root);
    let health_probe = |worker: &cosmon_core::id::WorkerId| {
        backend.is_alive(worker).map_err(|error| error.to_string())
    };
    let lease_missions = cosmon_harvest::pilot_gesture::leases_at(&tenant_state_dir)
        .ok()
        .and_then(|leases| leases.missions().ok())
        .unwrap_or_default();
    let subject = subject_for_jwt(&jwt);

    let view = ops::vitals(
        &store,
        &tenant_state_dir,
        &subject,
        &health_probe,
        &lease_missions,
        Utc::now(),
    )
    .map_err(|error| match error {
        VitalsError::StoreUnavailable(_) => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "store_unavailable",
            request_id: Some(spark.request_id.clone()),
        },
    })?;

    Ok(Json(json!({
        "request_id": spark.request_id,
        "vitals": view,
    })))
}
