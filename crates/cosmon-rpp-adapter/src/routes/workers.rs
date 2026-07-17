// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/workers` — list active workers in the per-tenant noyau.
//!
//! # Pipeline
//!
//! 1. Extract `Authorization: Bearer <jwt>`; 401 if missing.
//! 2. Validate JWT (clause a) → [`ValidatedJwt`].
//! 3. Require `cosmon:worker:read`; emit
//!    `AuthzDecisionEvaluated{verb=list_workers, decision=Allow|Absent}`.
//! 4. Admission boundary — same five clauses as every other route, so
//!    a noyau-A JWT cannot enumerate noyau-B workers.
//! 5. Scan the per-tenant store for molecules carrying a live
//!    [`cosmon_core::process::MoleculeProcess`] record; that record is
//!    the canonical source of truth for "worker bound to molecule"
//!    after the worker-record fold-in. Project each into a
//!    [`WorkerEntry`].
//! 6. Return `{ workers: [...], count: N }`.
//!
//! # Why molecule.process, not fleet.json or tmux directly
//!
//! Pre-fold-in, "which workers are alive?" was a tri-witness query
//! (`fleet.json` + `molecule.assigned_worker` + `tmux list-sessions`) and
//! every reader picked a different reconciliation — the phantom-worker
//! class. The worker-record fold-in
//! collapsed that to a single inline slot on the molecule: `process:
//! Option<MoleculeProcess>`. Presence means "the pilot believes a live
//! worker is bound"; absence means "no worker". Every new reader,
//! including this route, consults `process` first.
//!
//! Tmux is intentionally NOT queried here for two reasons:
//!
//! - Cross-tenant: tmux sessions live in a host-global namespace; a
//!   `tmux list-sessions` shows every noyau's sessions to whoever has
//!   shell access. This route's tenant isolation is enforced by
//!   scoping the scan to `<galaxies_root>/<noyau>`.
//! - Single source of truth: `MoleculeProcess` already carries
//!   `tmux_session`, `pid`, `started_at`, and the worker id; consulting
//!   tmux would re-introduce the multi-witness disagreement the
//!   fold-in fixed.
//!
//! # Empty noyau
//!
//! When no molecule carries a live process record, the response is
//! `{ workers: [], count: 0 }` (200, not 204). That keeps the shape
//! stable for tenant code that always parses the array.
//!
//! # Scope
//!
//! Requires [`crate::auth::scopes::WORKER_READ`] (`cosmon:worker:read`,
//! additive at v1.4). Distinct from `cosmon:molecule:read` because
//! the worker surface exposes session-level facts (tmux session names,
//! PIDs, start instants) that a tenant which only needs molecule state
//! should not see — session metadata can reveal placement and
//! supervision posture.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};
use serde::Serialize;
use serde_json::{json, Value};

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{GRANT_SOURCE_BINDING, GRANT_SOURCE_JWT, WORKER_READ};
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::AppState;
use cosmon_state::instrumentation::{emit_authz_decision_with_source, AuthzDecision};

/// One worker entry in the [`WorkersResponse`] envelope. Stable
/// additive-only wire shape: adding a field is allowed; renaming or
/// removing one is a §8p break.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerEntry {
    /// Molecule the worker is bound to.
    pub molecule_id: String,
    /// Worker identity stamped at `cs tackle` time. Same string the
    /// operator sees in `cs status` rows.
    pub session_name: String,
    /// When the worker process record was created (typically by
    /// `cs tackle`). ISO-8601 / RFC 3339 with sub-second precision.
    pub started_at: String,
    /// Operating-system PID when the transport backend surfaced one.
    /// `null` for backends that do not expose PIDs (legacy or
    /// in-process adapters) — the field is always present so clients
    /// can branch on its absence rather than on a missing key.
    pub pid: Option<u32>,
    /// Tmux session owning the worker process. Same string that
    /// `tmux -L <socket> attach -t <session>` would target.
    pub tmux_session: String,
}

/// Body schema for `GET /v1/workers`.
#[derive(Debug, Serialize)]
pub struct WorkersResponse {
    /// Audit-correlation id for the inbox-materialised admission row.
    pub request_id: String,
    /// Active workers in the caller's noyau, ordered by `started_at`
    /// ascending so paginating clients see a stable enumeration.
    pub workers: Vec<WorkerEntry>,
    /// `workers.len()` — pre-computed so a tenant can render a count
    /// without parsing the array.
    pub count: usize,
}

/// `GET /v1/workers` — see module docs for the pipeline.
pub async fn list_workers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    // 1. Authorization header.
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;

    // 2. JWT validation (clause a).
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;

    // 3. Scope check + AuthzDecisionEvaluated emission.
    authorise_worker_read(&state, &jwt)?;

    // 4. Admission boundary — pins the noyau, consumes a rate-limit
    //    token, materialises the inbox audit row.
    let spark = build_spark(&state, &jwt)?;

    // 5. Scan the per-tenant store for molecules with a live process.
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        // No tenant directory at all — surface as an empty list rather
        // than 404 so a freshly-provisioned noyau can call /workers
        // before its first nucleate without a spurious error.
        let body = WorkersResponse {
            request_id: spark.request_id.clone(),
            workers: Vec::new(),
            count: 0,
        };
        return Ok(Json(serde_json::to_value(&body).unwrap_or(Value::Null)));
    }
    let tenant_state_dir = tenant_root.join(".cosmon").join("state");
    let store = FileStore::new(&tenant_state_dir);

    let molecules = store
        .list_molecules(&MoleculeFilter::default())
        .map_err(|_| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "store_unavailable",
            request_id: Some(spark.request_id.clone()),
        })?;

    let mut workers: Vec<WorkerEntry> = molecules
        .into_iter()
        .filter_map(|mol| {
            let process = mol.process.as_ref()?;
            Some(WorkerEntry {
                molecule_id: mol.id.as_str().to_owned(),
                session_name: process.worker_id.as_str().to_owned(),
                started_at: process.started_at.to_rfc3339(),
                pid: process.pid,
                tmux_session: process.tmux_session.clone(),
            })
        })
        .collect();

    // Stable ordering — by `started_at` ascending so a paginating
    // client sees the oldest workers first and the listing is
    // deterministic across calls.
    workers.sort_by(|a, b| a.started_at.cmp(&b.started_at));

    let body = WorkersResponse {
        request_id: spark.request_id.clone(),
        count: workers.len(),
        workers,
    };
    Ok(Json(serde_json::to_value(&body).unwrap_or(Value::Null)))
}

/// Authorise the `cosmon:worker:read` scope against JWT scopes ∪
/// binding-granted scopes, emitting the matching
/// `AuthzDecisionEvaluated` event. Mirrors the per-module pattern in
/// `routes::events_stream`.
fn authorise_worker_read(state: &Arc<AppState>, jwt: &ValidatedJwt) -> Result<(), ApiError> {
    let nucleon_map = state.nucleon_map.load();
    let binding_scopes = nucleon_map.allowed_scopes_for_audience(&jwt.iss, &jwt.sub, &jwt.aud);
    let (decision, grant_source) = if jwt.has_scope(WORKER_READ) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_JWT))
    } else if binding_scopes.iter().any(|s| s == WORKER_READ) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_BINDING))
    } else {
        (AuthzDecision::Absent, None)
    };

    emit_authz_decision_with_source(
        &state.state_dir,
        "list_workers",
        &format!("jwt:{}", jwt.sub),
        Some(WORKER_READ),
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

/// Build the admission [`Spark`] for the workers route.
fn build_spark(state: &Arc<AppState>, jwt: &ValidatedJwt) -> Result<Spark, ApiError> {
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
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
    http_request_to_spark(&rig, jwt, Verb::ListWorkers, None)
        .map_err(|e| ApiError::from_reject(&e, Some(new_request_id())))
}

/// Extract the JWT bearer. Mirrors the per-module helper in
/// `routes::molecules` / `routes::events_stream`.
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

// Suppress imports kept for cross-module symmetry — same pattern as
// `routes::quota::_unused`. Reviewers reading this in isolation see
// the full set of admission types in scope.
#[allow(dead_code)]
fn _unused() {
    let _ = json!({});
}
