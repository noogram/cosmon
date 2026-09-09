// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/:id` and `POST /v1/molecules` — V0 read + V1
//! mutation cuts of the molecule surface, refactored library-direct
//! (T-RPP-LIB-DIRECT).
//!
//! GET pipeline (V0):
//!
//! 1. Extract `Authorization: Bearer <jwt>`; 401 if missing.
//! 2. Validate JWT (clause a) → [`ValidatedJwt`].
//! 3. Require `cosmon:molecule:read` (or `:write`, which implies read);
//!    emit `AuthzDecisionEvaluated{verb=observe, decision=Allow|Absent}`
//!    so the audit trail is symmetric with the POST/list/tag/freeze/…
//!    routes. ADR-080 §6.5 — every wire route enforces a scope.
//! 4. Run [`http_request_to_spark`] (clauses a–d, materialise inbox).
//! 5. Resolve the per-tenant `<galaxies_root>/<noyau>/.cosmon/state` and
//!    load the molecule via `cosmon_state::ops::observe`. **No
//!    subprocess.**
//! 6. Render the canonical wire shape via [`ObserveJson::from_view`] and
//!    return.
//!
//! POST pipeline (V1 mutation cut, ADR-080 §10.2):
//!
//! 1. Extract + validate JWT.
//! 2. Require `cosmon:molecule:write` scope; emit
//!    `AuthzDecisionEvaluated{verb=nucleate, decision=Allow|Absent}`
//!    (the same instrumentation pattern as observe, T-AUTHZ-INSTR).
//! 3. Validate body shape: `{ formula, kind?, variables?, tags? }`.
//! 4. Admission boundary (`http_request_to_spark`).
//! 5. Resolve the per-tenant store + formulas dirs and call
//!    `cosmon_state::ops::nucleate` directly. **No subprocess.**
//! 6. Project the persisted molecule via `ObserveJson::from_view` and
//!    emit 201 + `Location: /v1/molecules/<id>`.
//!
//! Errors are mapped through [`ApiError::from_reject`] / [`ApiError`]
//! so the wire body never leaks `sub` / `nucleon_id` / tenant identity.
//!
//! # Why library-direct
//!
//! A remote-pilot strace audit caught the V0 container shelling out to
//! the in-image `cs` binary on every request. The library-first promise
//! was held
//! at the cs-cli boundary but **not** at the §8j RPP boundary. This
//! module is the fix — both routes now invoke `cosmon_state::ops`
//! verbs in-process, the container ships only `cs-rpp-adapter`, and
//! a fresh strace audit shows zero `clone()`/`execve()` on either
//! route.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use cosmon_core::auth::{JwtClaims, Subject};
use cosmon_core::harvest_door::HarvestOptions;
use cosmon_core::id::{FleetId, MoleculeId};
use cosmon_core::tag::Tag;
use cosmon_filestore::{harvest_door, FileStore};
use cosmon_process_witness::process_start_time;
use cosmon_state::instrumentation::{emit_authz_decision_with_source, AuthzDecision};
use cosmon_state::ops::{
    self, CollapseError, CollapseJson, CollapseRequest, EnsembleError, EnsembleJson,
    EnsembleRequest, FreezeError, FreezeJson, FreezeRequest, MoleculeView, NucleateError,
    NucleateRequest, ObserveError, ObserveJson, OpsError, StuckError, StuckJson, StuckRequest,
    TagError, TagJson, ThawError, ThawJson, ThawRequest,
};
use cosmon_state::StateStore;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use cosmon_runtime::tackle_exec::TackleExecError;
use cosmon_runtime::{DispatchPin, LibraryExecutor};

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::drain;
use crate::error::{ApiError, RppRejectReason};
use crate::events_bus::MoleculeEvent;
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::worker_env::{EnvelopedBackend, SharedBackend, WorkerEnvelope};
use crate::AppState;

// Scope catalog lives in `crate::auth::scopes` (since v1.0.0-rc,
// `task-20260522-b538` §3). Re-exported here under the original local
// names to limit churn at the ~9 `authorise_scope` call sites.
//
// `WORKER_SPAWN` is **new** in v1.0.0-rc — it gates `tackle`
// **in addition to** `MOLECULE_WRITE` (composition AND), so a tenant
// that grants only `:write` cannot burn the operator's Anthropic
// budget by spawning workers (delib-20260522-a069 §D5, torvalds
// §Piège #3).
pub use crate::auth::scopes::MOLECULE_READ as SCOPE_MOLECULE_READ;
pub use crate::auth::scopes::MOLECULE_WRITE as SCOPE_MOLECULE_WRITE;
pub use crate::auth::scopes::WORKER_SPAWN as SCOPE_WORKER_SPAWN;
use crate::auth::scopes::{GRANT_SOURCE_BINDING, GRANT_SOURCE_JWT};

/// Hard per-noyau ceiling on **concurrently live workers**, checked at the
/// pre-spawn seam of [`tackle_molecule`] (delib-20260709-943e M3, turing
/// exploit #3 defense).
///
/// `cosmon:worker:spawn` proves the caller is *allowed* to spawn; it says
/// nothing about *how many*. Each `tackle` burns real Anthropic credit and
/// drops a git worktree on disk, so an unbounded stream of `tackle` calls —
/// even from a legitimately-scoped tenant — is a budget-burn and
/// disk-exhaustion vector. This ceiling is the well-founded bound: a noyau
/// already at the cap cannot open a `(N+1)`-th worker until one drains.
///
/// The count is read from ground truth (molecules carrying a live
/// [`cosmon_core::process::MoleculeProcess`] record in the noyau's own
/// fleet state — the same witness `GET /v1/workers` uses), so it is
/// self-correcting: a worker that dies drops out of the count on its next
/// evaluation. An in-RAM acquire/release counter was rejected because the
/// adapter never observes the detached worker's termination and would leak
/// the slot forever.
///
/// V0 is a single fleet-wide constant (turing's "hard ceiling"). Per-noyau
/// tuning via `rpp.toml` is a strictly additive follow-up — same doctrine as
/// the scope catalog (adding a knob is a minor bump, never a regression).
pub const DEFAULT_TACKLE_CEILING_PER_NOYAU: usize = 4;

/// Count the noyau's currently-live workers from its own fleet state.
///
/// Ground-truth witness = active [`cosmon_core::process::MoleculeProcess`]
/// slots whose recorded process identity is still witnessed externally.
/// Records without a PID retain the existing conservative behaviour because
/// tmux-backed adapters do not always expose one. Reads from
/// `<tenant_root>/.cosmon/state`.
///
/// Fail-open on a store read error (returns 0): the ceiling is a
/// resource-abuse guard, not an authorization boundary, and blocking every
/// tackle because the fleet state momentarily failed to parse would convert
/// a transient read hiccup into a self-inflicted denial of service. The
/// per-`sub` rate limiter still caps request volume on that path. A read
/// error is surfaced via `tracing::warn!` for operator visibility.
fn count_live_workers(tenant_root: &Path) -> usize {
    let tenant_state_dir = tenant_root.join(".cosmon").join("state");
    let store = FileStore::new(&tenant_state_dir);
    match store.list_molecules(&cosmon_state::MoleculeFilter::default()) {
        Ok(molecules) => molecules
            .iter()
            .filter_map(|m| m.process.as_ref())
            .filter(|process| {
                process.is_active() && recorded_process_is_live(process.pid, process.pid_start_time)
            })
            .count(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                tenant_state_dir = %tenant_state_dir.display(),
                "tackle ceiling: failed to read fleet state, treating live-worker count as 0"
            );
            0
        }
    }
}

/// Return whether the recorded process identity still names the same process.
///
/// `None` remains live by design: PID-less adapters are supervised through
/// their own transport witness. A PID must carry the launch fingerprint that
/// was captured at spawn; `kill(pid, 0)` alone proves only that *some* process
/// owns that number and would otherwise preserve a phantom slot after PID
/// reuse.
fn recorded_process_is_live(pid: Option<u32>, pid_start_time: Option<u64>) -> bool {
    let Some(pid) = pid else {
        return true;
    };
    let Some(expected_start_time) = pid_start_time else {
        return false;
    };
    process_start_time(pid).is_some_and(|actual| actual == expected_start_time)
}

/// Compute the effective `(decision, grant_source)` pair for a scope
/// check that consults both the JWT and the admin-nucleon binding.
///
/// Order of precedence:
///
/// 1. Any of `wanted_any` present in the JWT scopes → `Allow + "jwt"`.
/// 2. Otherwise, any of `wanted_any` present in the binding-granted
///    scopes → `Allow + "binding"`.
/// 3. Otherwise → `Absent + None`.
///
/// The function is total and pure. It does **not** widen the tenant
/// isolation invariant: the binding-granted scopes are read from the
/// `(iss, sub)`-specific `Resolved` record, and the subsequent
/// admission boundary still enforces the audience pin
/// (`CrossTenantPivot`) before reaching the per-tenant store. The
/// scope union therefore cannot grant access to a `noyau` other than
/// the one the binding declares (ADR-080 §8j).
fn effective_scope_decision(
    jwt: &ValidatedJwt,
    binding_scopes: &[String],
    wanted_any: &[&str],
) -> (AuthzDecision, Option<&'static str>) {
    if wanted_any.iter().any(|w| jwt.has_scope(w)) {
        return (AuthzDecision::Allow, Some(GRANT_SOURCE_JWT));
    }
    if wanted_any
        .iter()
        .any(|w| binding_scopes.iter().any(|b| b == w))
    {
        return (AuthzDecision::Allow, Some(GRANT_SOURCE_BINDING));
    }
    (AuthzDecision::Absent, None)
}

/// Public re-export of the private [`authorise_scope`] for the sibling
/// `quota` route module — the function is identical, and copy-pasting
/// it would risk drift on the audit-event semantics. The name is
/// `_public` to mark that it is intentionally a tiny shim and not part
/// of the §8p frozen surface (it is internal plumbing). Added for the
/// `/v1/quota` route.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn authorise_scope_public(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: &'static str,
    wanted_any: &[&str],
    audit_scope: &str,
) -> Result<(), ApiError> {
    authorise_scope(state, jwt, verb, wanted_any, audit_scope)
}

/// Public re-export of the private [`build_spark`] helper for the
/// sibling `quota` route module. Same rationale as
/// [`authorise_scope_public`] — keeps the admission boundary signature
/// uniform across routes.
pub(crate) fn build_spark_public(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: Verb,
    target: Option<&str>,
) -> Result<Spark, ApiError> {
    build_spark(state, jwt, verb, target)
}

/// Authorise a verb against the JWT + binding-scope union and emit the
/// `AuthzDecisionEvaluated` event. Returns `Ok(())` when the scope
/// check admits and `Err(ApiError)` on `Absent`. T23 — collapses what
/// were 9 copy-pasted blocks across the route handlers into a single
/// audited call.
fn authorise_scope(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: &'static str,
    wanted_any: &[&str],
    audit_scope: &str,
) -> Result<(), ApiError> {
    let nucleon_map = state.nucleon_map.load();
    let binding_scopes = nucleon_map.allowed_scopes_for_audience(&jwt.iss, &jwt.sub, &jwt.aud);
    let (decision, grant_source) = effective_scope_decision(jwt, binding_scopes, wanted_any);
    emit_authz_decision_with_source(
        &state.state_dir,
        verb,
        &format!("jwt:{}", jwt.sub),
        Some(audit_scope),
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

/// `GET /v1/molecules/{id}`. See module docs for the pipeline.
pub async fn get_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    // 1. Authorization header.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;

    // 2. JWT validation.
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 3. Scope check + AuthzDecisionEvaluated emission. ADR-080 §6.5
    //    requires every read route to enforce `cosmon:molecule:read`
    //    (or write, which implies read). The check unions the JWT
    //    scopes with admin-nucleon binding-granted scopes (T23,
    //    `task-20260513-3a9e`) so admin identities can write through
    //    the API even when the upstream IdP (Forgejo) only issues
    //    `openid`. Cross-tenant isolation is unaffected — the
    //    audience pin in admission rejects pivots independently.
    authorise_scope(
        &state,
        &jwt,
        "observe",
        &[SCOPE_MOLECULE_READ, SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_READ,
    )?;

    // 4. Admission boundary.
    let spark = build_spark(&state, &jwt, Verb::ObserveMolecule, Some(&molecule_id_str))?;

    // 5. Library-direct read. The molecule id must parse as a
    //    well-formed `MoleculeId` — a malformed id is a 404 rather
    //    than a 400 (turing §8.2.3 — never emit an existence oracle
    //    on the wire).
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

    let view = run_observe(&state, &spark, &jwt, &molecule_id)?;
    let body = ObserveJson::from_view(&view, view.data.id.as_str());
    let body_value = serde_json::to_value(&body).unwrap_or(Value::Null);

    Ok(Json(json!({
        "request_id": spark.request_id,
        "molecule": body_value,
    })))
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

/// Body schema for `POST /v1/molecules`.
#[derive(Debug, Deserialize)]
pub struct NucleateBody {
    /// Formula name (required). Resolved by the cosmon ops layer in
    /// the per-tenant `<galaxies_root>/<noyau>/.cosmon/formulas/`
    /// directory; an unknown formula collapses to a 404.
    pub formula: String,
    /// Optional molecule kind (`task`, `idea`, `decision`, …).
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional variables map (`{key: value}`).
    #[serde(default)]
    pub variables: Option<Value>,
    /// Optional tag list. Each entry is parsed via `Tag::new`.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Optional molecule ids the new molecule is blocked by (B1
    /// moussage resident). Additive field: lets a
    /// tenant nucleate a drainable DAG through the §8p surface. Each
    /// id must reference an existing molecule in the tenant store
    /// (dangling refs are refused — 404 `blocked_by_not_found`).
    #[serde(default)]
    pub blocked_by: Option<Vec<String>>,
}

/// `POST /v1/molecules` — V1 mutation cut. See module docs for pipeline.
///
/// # Panics
///
/// Infallible: every constant `&'static str` referenced as a fleet id
/// here (`"default"`) parses through the cosmon-core validator. The
/// `expect` documents that contractually rather than panicking
/// dynamically.
#[allow(clippy::too_many_lines)]
pub async fn post_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<Json<NucleateBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ApiError> {
    // 1. Authorization header → JWT validation.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 2. Scope check + AuthzDecisionEvaluated emission (JWT scopes
    //    ∪ binding-granted scopes — see [`authorise_scope`]).
    authorise_scope(
        &state,
        &jwt,
        "nucleate",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;

    // 3. Body validation (after scope check so a missing scope yields
    //    403, not 400 — the scope is the gate, the body shape is the
    //    payload).
    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;
    let formula = body.formula.trim().to_owned();
    if formula.is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_formula",
            request_id: None,
        });
    }
    let variables = parse_variables(body.variables.as_ref()).map_err(|label| ApiError {
        status: StatusCode::BAD_REQUEST,
        label,
        request_id: None,
    })?;
    let tags = body.tags.unwrap_or_default();

    // 4. Admission boundary (clauses a–d, materialise inbox).
    let spark = build_spark(&state, &jwt, Verb::NucleateMolecule, None)?;

    // 5. Library-direct nucleation against the tenant's store +
    //    formulas dir.
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        // No tenant directory at all — surface as 503 because the
        // request is well-formed but the substrate is not staged.
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "tenant_unavailable",
            request_id: Some(spark.request_id.clone()),
        });
    }
    let tenant_state_dir = tenant_root.join(".cosmon").join("state");
    let tenant_formulas_dir = tenant_root.join(".cosmon").join("formulas");
    let store = FileStore::new(&tenant_state_dir);

    let subject = subject_for_jwt(&jwt);
    let mut request = NucleateRequest::for_formula(formula);
    request.kind = body.kind;
    request.variables = variables.into_iter().collect();
    request.tags = tags;
    request.blocked_by = body.blocked_by.unwrap_or_default();
    request.fleet =
        FleetId::new("default").expect("`default` is always a valid fleet id at the boundary");

    let view = ops::nucleate(
        &store,
        &tenant_state_dir,
        &tenant_formulas_dir,
        &subject,
        request,
    )
    .map_err(|e| nucleate_error_to_api(&e, &spark.request_id))?;

    // 6. Project the wire shape using the existing observe renderer so
    //    the response body and a follow-up GET are byte-stable.
    let observe_view = MoleculeView {
        data: view.data.clone(),
        // Match the cs-cli's read path: a freshly nucleated molecule
        // has a single-snapshot coupling report with no metrics yet —
        // mirror what `cs observe :id --json` would emit by re-running
        // the read-only projection in process.
        metrics: cosmon_state::wait::coupling_report_snapshot(&tenant_state_dir, &view.data.id),
        ghost: ops::detect_ghost(&view.data),
        // A freshly nucleated molecule has consumed no LLM tokens yet.
        api_tokens: None,
        // ...and has not been tackled, so no `ModelSelected` event exists.
        model: None,
    };
    let body = ObserveJson::from_view(&observe_view, view.molecule_dir.to_string_lossy().as_ref());
    let body_value = serde_json::to_value(&body).unwrap_or(Value::Null);

    let molecule_id = view.data.id.as_str().to_owned();

    // Publish to the SSE bus (task-20260522-c46a, workflow c).
    // Nucleation is a (None → first_status) transition; cs-cli's wire
    // shape labels the new status `"active"` (see NucleateJson) so we
    // forward that exact string to keep the SSE payload stable with
    // the REST envelope.
    state.events.publish(MoleculeEvent::state_changed(
        spark.noyau.as_str(),
        &molecule_id,
        "",
        body.status,
    ));

    let response_body = json!({
        "request_id": spark.request_id.clone(),
        "molecule": body_value,
    });
    let location = format!("/v1/molecules/{molecule_id}");
    let mut resp = (StatusCode::CREATED, Json(response_body)).into_response();
    if let Ok(value) = location.parse() {
        resp.headers_mut().insert(header::LOCATION, value);
    }
    Ok(resp)
}

/// Build the admission [`Spark`] common to both routes.
fn build_spark(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: Verb,
    target: Option<&str>,
) -> Result<Spark, ApiError> {
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
    http_request_to_spark(&rig, jwt, verb, target)
        .map_err(|e| state.reject_with_request_id(e, new_request_id()))
}

/// Run `cosmon_state::ops::observe` against the per-tenant `FileStore`.
///
/// Maps the lib error shape to the wire-stable [`ApiError`] surface.
/// `MoleculeNotFound` collapses to 404 (turing §8.2.3 — no existence
/// oracle); `StoreUnavailable` is 503.
fn run_observe(
    state: &Arc<AppState>,
    spark: &Spark,
    jwt: &ValidatedJwt,
    molecule_id: &MoleculeId,
) -> Result<MoleculeView, ApiError> {
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
    let subject = subject_for_jwt(jwt);
    ops::observe(&store, &tenant_state_dir, &subject, molecule_id).map_err(|e| match &e {
        ObserveError::MoleculeNotFound(_) => ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        },
        ObserveError::StoreUnavailable(_) => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: ops_error_label(&e),
            request_id: Some(spark.request_id.clone()),
        },
        _ => ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            label: ops_error_label(&e),
            request_id: Some(spark.request_id.clone()),
        },
    })
}

/// Resolve the per-tenant store, load the molecule, and return both the
/// projected [`MoleculeView`] and the resolved tenant state directory.
///
/// The sibling `result` route ([`crate::routes::result`]) needs the
/// on-disk tenant state dir to locate the molecule's *persistent*
/// directory (`<state_dir>/fleets/<fleet>/molecules/<id>/`) from which it
/// reads the canonical deliverable. `run_observe` discards the state dir;
/// this shim threads it back out without duplicating the tenant
/// resolution + error-mapping logic. The `_public` marker (matching
/// [`authorise_scope_public`] / [`build_spark_public`]) flags this as
/// internal plumbing, not part of the §8p frozen surface.
pub(crate) fn observe_with_state_dir_public(
    state: &Arc<AppState>,
    spark: &Spark,
    jwt: &ValidatedJwt,
    molecule_id: &MoleculeId,
) -> Result<(MoleculeView, std::path::PathBuf), ApiError> {
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
    let subject = subject_for_jwt(jwt);
    let view =
        ops::observe(&store, &tenant_state_dir, &subject, molecule_id).map_err(|e| match &e {
            ObserveError::MoleculeNotFound(_) => ApiError {
                status: StatusCode::NOT_FOUND,
                label: "not_found",
                request_id: Some(spark.request_id.clone()),
            },
            ObserveError::StoreUnavailable(_) => ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                label: ops_error_label(&e),
                request_id: Some(spark.request_id.clone()),
            },
            _ => ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                label: ops_error_label(&e),
                request_id: Some(spark.request_id.clone()),
            },
        })?;
    Ok((view, tenant_state_dir))
}

/// Translate a [`NucleateError`] into the wire-stable [`ApiError`].
fn nucleate_error_to_api(err: &NucleateError, request_id: &str) -> ApiError {
    let status = match err.http_status() {
        404 => StatusCode::NOT_FOUND,
        400 => StatusCode::BAD_REQUEST,
        503 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let label: &'static str = match err {
        NucleateError::FormulaNotFound(_) => "formula_not_found",
        NucleateError::FormulaParse(_) => "formula_parse_failed",
        NucleateError::InvalidKind(_) => "invalid_kind",
        NucleateError::InvalidTag(_) => "invalid_tag",
        NucleateError::MissingVariable(_) => "missing_variable",
        NucleateError::EmptyVariable(_) => "empty_variable",
        NucleateError::InvalidBlockedBy(_) => "invalid_blocked_by",
        NucleateError::BlockedByNotFound(_) => "blocked_by_not_found",
        NucleateError::Domain(_) => "nucleate_failed",
        NucleateError::StoreUnavailable(_) => "store_unavailable",
    };
    ApiError {
        status,
        label,
        request_id: Some(request_id.to_owned()),
    }
}

/// Turn a generic `OpsError` into a stable label without leaking the
/// raw message. Today only used by the observe → 503 path.
fn ops_error_label<E: OpsError>(_err: &E) -> &'static str {
    "store_unavailable"
}

/// Build a `Subject` for the lib ops layer from the validated JWT.
///
/// The `sub` claim drives the subject id; scopes are passed through
/// untouched. Failure to construct collapses to `Subject::operator()`
/// — but only when `sub` is empty (which the JWT validator already
/// rejects). The fallback exists so the path is total without raising
/// a panic.
fn subject_for_jwt(jwt: &ValidatedJwt) -> Subject {
    let claims = JwtClaims {
        sub: jwt.sub.clone(),
        scopes: jwt.scopes.clone(),
    };
    Subject::from_jwt_claims(&claims).unwrap_or_else(|_| Subject::operator())
}

/// Suppress unused-import warnings for the doc-paths we cite inline.
#[allow(dead_code)]
fn _unused(_: &Path) {}

/// Body schema for `POST /v1/molecules/:id/tags`.
#[derive(Debug, Deserialize)]
pub struct TagBody {
    /// Tags to add (optional). Each entry is parsed via [`Tag::new`].
    #[serde(default)]
    pub add: Option<Vec<String>>,
    /// Tags to remove (optional). Each entry is parsed via [`Tag::new`].
    #[serde(default)]
    pub remove: Option<Vec<String>>,
}

/// `POST /v1/molecules/:id/tags` — V1 mutation cut for tagging
/// (T-CST-V0).
///
/// Pipeline:
///
/// 1. Extract + validate JWT.
/// 2. Require `cosmon:molecule:write` scope; emit
///    `AuthzDecisionEvaluated{verb=tag, decision=Allow|Absent}`.
/// 3. Validate body shape: `{ add?: [string], remove?: [string] }`.
///    At least one entry across `add` ∪ `remove` is required.
/// 4. Admission boundary (`http_request_to_spark`).
/// 5. Resolve the per-tenant store and call `cosmon_state::ops::tag`.
/// 6. Project the wire shape through [`TagJson`] and return
///    `200 OK { request_id, tag: TagJson }` — byte-identical to
///    `cs --json tag`.
#[allow(clippy::too_many_lines)]
pub async fn tag_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    body: Result<Json<TagBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_scope(
        &state,
        &jwt,
        "tag",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;

    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;
    let add_strs = body.add.unwrap_or_default();
    let remove_strs = body.remove.unwrap_or_default();
    if add_strs.is_empty() && remove_strs.is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "empty_tag_request",
            request_id: None,
        });
    }
    let add_tags: Vec<Tag> = add_strs
        .into_iter()
        .map(Tag::new)
        .collect::<Result<_, _>>()
        .map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_tag",
            request_id: None,
        })?;
    let remove_tags: Vec<Tag> = remove_strs
        .into_iter()
        .map(Tag::new)
        .collect::<Result<_, _>>()
        .map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_tag",
            request_id: None,
        })?;

    let spark = build_spark(&state, &jwt, Verb::TagMolecule, Some(&molecule_id_str))?;

    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

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

    let subject_kind = format!("jwt:{}", jwt.sub);
    let delta = ops::tag(
        &store,
        &tenant_state_dir,
        &subject_kind,
        &molecule_id,
        &add_tags,
        &remove_tags,
    )
    .map_err(|e| match &e {
        TagError::MoleculeNotFound(_) => ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        },
        TagError::EmptyRequest => ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "empty_tag_request",
            request_id: Some(spark.request_id.clone()),
        },
        TagError::ProtectedReservation(_) => ApiError {
            status: StatusCode::FORBIDDEN,
            label: "protected_runtime_reservation",
            request_id: Some(spark.request_id.clone()),
        },
        TagError::ProtectedDecisionOptIn(_) => ApiError {
            status: StatusCode::FORBIDDEN,
            label: "protected_runtime_decision_opt_in",
            request_id: Some(spark.request_id.clone()),
        },
        TagError::StoreUnavailable(_) => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "store_unavailable",
            request_id: Some(spark.request_id.clone()),
        },
    })?;

    let body = TagJson::from_delta(&delta);
    let body_value = serde_json::to_value(&body).unwrap_or(Value::Null);

    // Publish a tag delta as a generic `event_appended` (tags are not
    // a lifecycle transition — they are an event on the molecule's
    // append-only log). The body carries the same delta as the REST
    // response so a subscriber reconstructs the operation 1:1.
    state.events.publish(MoleculeEvent::event_appended(
        spark.noyau.as_str(),
        molecule_id.as_str(),
        json!({"kind": "tag", "delta": body_value.clone()}),
    ));

    Ok(Json(json!({
        "request_id": spark.request_id,
        "tag": body_value,
    })))
}

// ---------------------------------------------------------------------------
// T-CST-EXPAND verbs (ensemble / collapse / freeze / thaw / stuck)
// ---------------------------------------------------------------------------

/// Query parameters for `GET /v1/molecules`.
#[derive(Debug, Deserialize, Default)]
pub struct EnsembleQuery {
    /// Filter by status (`pending`, `running`, `frozen`, …).
    #[serde(default)]
    pub status: Option<String>,
    /// Filter by molecule kind (`task`, `idea`, …).
    #[serde(default)]
    pub kind: Option<String>,
    /// Repeated `?tag=<glob>` (deserialized via the parent helper because
    /// `serde_urlencoded` does not natively repeat).
    #[serde(default)]
    pub tag: Option<String>,
    /// Optional fleet filter.
    #[serde(default)]
    pub fleet: Option<String>,
}

/// `GET /v1/molecules` — V1 listing cut (T-CST-EXPAND).
///
/// Pipeline mirrors [`get_molecule`]: extract bearer, validate JWT,
/// scope-check, admission boundary, library-direct
/// `cosmon_state::ops::ensemble`. Read-only, so the scope is the
/// `cosmon:molecule:read` family (relaxed for V0 — accept either read
/// or write, since both imply visibility).
pub async fn list_molecules(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EnsembleQuery>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_scope(
        &state,
        &jwt,
        "ensemble",
        &[SCOPE_MOLECULE_READ, SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_READ,
    )?;

    let spark = build_spark(&state, &jwt, Verb::EnsembleMolecule, None)?;

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
    let subject = subject_for_jwt(&jwt);

    let request = EnsembleRequest {
        status: query.status,
        kind: query.kind,
        tag_globs: query.tag.into_iter().collect(),
        fleet: query.fleet,
    };
    let view =
        ops::ensemble(&store, &tenant_state_dir, &subject, request).map_err(|e| match &e {
            EnsembleError::InvalidFilter(_) => ApiError {
                status: StatusCode::BAD_REQUEST,
                label: "invalid_filter",
                request_id: Some(spark.request_id.clone()),
            },
            EnsembleError::StoreUnavailable(_) => ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                label: "store_unavailable",
                request_id: Some(spark.request_id.clone()),
            },
        })?;

    let body = EnsembleJson::from_view(&view);
    let body_value = serde_json::to_value(&body).unwrap_or(Value::Null);
    Ok(Json(json!({
        "request_id": spark.request_id,
        "ensemble": body_value,
    })))
}

/// Body schema for `POST /v1/molecules/:id/collapse`.
#[derive(Debug, Deserialize)]
pub struct CollapseBody {
    /// Free-form reason (mandatory).
    pub reason: String,
    /// Structured cause attribution.
    #[serde(default)]
    pub cause: Option<String>,
    /// Account alias (only with `cause = rate_limit`).
    #[serde(default)]
    pub account: Option<String>,
    /// Quota currency name (only with `cause = rate_limit`).
    #[serde(default)]
    pub kind: Option<String>,
}

/// `POST /v1/molecules/:id/collapse` — V1 mutation cut (T-CST-EXPAND).
pub async fn collapse_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    body: Result<Json<CollapseBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_scope(
        &state,
        &jwt,
        "collapse",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;

    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;
    if body.reason.trim().is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "missing_reason",
            request_id: None,
        });
    }

    let spark = build_spark(&state, &jwt, Verb::CollapseMolecule, Some(&molecule_id_str))?;
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

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
    let subject = subject_for_jwt(&jwt);

    let request = CollapseRequest {
        reason: body.reason,
        cause: body.cause,
        account: body.account,
        kind: body.kind,
        reason_kind: None,
    };
    let view =
        ops::collapse(&store, &tenant_state_dir, &subject, &molecule_id, request).map_err(|e| {
            match &e {
                CollapseError::MoleculeNotFound(_) => ApiError {
                    status: StatusCode::NOT_FOUND,
                    label: "not_found",
                    request_id: Some(spark.request_id.clone()),
                },
                CollapseError::InvalidCause(_) => ApiError {
                    status: StatusCode::BAD_REQUEST,
                    label: "invalid_cause",
                    request_id: Some(spark.request_id.clone()),
                },
                CollapseError::MismatchedAccountKind(_) => ApiError {
                    status: StatusCode::BAD_REQUEST,
                    label: "mismatched_account_kind",
                    request_id: Some(spark.request_id.clone()),
                },
                CollapseError::AlreadyCompleted(_) => ApiError {
                    status: StatusCode::CONFLICT,
                    label: "already_completed",
                    request_id: Some(spark.request_id.clone()),
                },
                CollapseError::StoreUnavailable(_) => ApiError {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    label: "store_unavailable",
                    request_id: Some(spark.request_id.clone()),
                },
            }
        })?;

    let body = CollapseJson::from_view(&view);
    let body_value = serde_json::to_value(&body).unwrap_or(Value::Null);

    state.events.publish(MoleculeEvent::state_changed(
        spark.noyau.as_str(),
        molecule_id.as_str(),
        body.previous_status.clone(),
        body.status,
    ));

    Ok(Json(json!({
        "request_id": spark.request_id,
        "collapse": body_value,
    })))
}

/// Body schema for `POST /v1/molecules/:id/freeze`.
///
/// Fusion v1.0.0-rc: `state` is mandatory and
/// dispatches between the former `/freeze` (`state: "frozen"`) and
/// `/thaw` (`state: "active"`) routes. The legacy `/thaw` endpoint is
/// preserved as a 410-Gone migration handler — see
/// [`thaw_gone_handler`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreezeBody {
    /// Target lifecycle status. `Frozen` pauses, `Active` resumes.
    pub state: FreezeState,
    /// Optional reason recorded against the state transition.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Target lifecycle status carried by [`FreezeBody`].
///
/// `deny_unknown_fields` on the parent struct catches typos; the
/// `#[serde(rename_all = "lowercase")]` keeps the wire vocabulary
/// stable (lowercase enum values, mirroring the `OpenAPI` `enum:
/// [frozen, active]`).
#[derive(Debug, Deserialize, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FreezeState {
    /// Pause a `Running` molecule. Mirrors V0 `ops::freeze`.
    Frozen,
    /// Resume a `Frozen` molecule. Mirrors V0 `ops::thaw`.
    Active,
}

/// `POST /v1/molecules/:id/freeze` — fusion route (T-CST-EXPAND,
/// v1.0.0-rc). Dispatches to `ops::freeze` or
/// `ops::thaw` based on `state`.
#[allow(clippy::too_many_lines)]
pub async fn freeze_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    body: Result<Json<FreezeBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_scope(
        &state,
        &jwt,
        "freeze",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;

    // Body is required post-fusion: `state` decides the dispatch.
    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;

    // Audit-trail verb matches the actual operation dispatched. This
    // keeps `grep verb=thaw` over the whisper inbox meaningful even
    // though the wire route is now fused.
    let audit_verb = match body.state {
        FreezeState::Frozen => Verb::FreezeMolecule,
        FreezeState::Active => Verb::ThawMolecule,
    };
    let spark = build_spark(&state, &jwt, audit_verb, Some(&molecule_id_str))?;
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

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
    let subject = subject_for_jwt(&jwt);

    let (body_value, prev_status, new_status): (Value, String, &'static str) = match body.state {
        FreezeState::Frozen => {
            let request = FreezeRequest {
                reason: body.reason,
            };
            let view = ops::freeze(&store, &tenant_state_dir, &subject, &molecule_id, request)
                .map_err(|e| match &e {
                    FreezeError::MoleculeNotFound(_) => ApiError {
                        status: StatusCode::NOT_FOUND,
                        label: "not_found",
                        request_id: Some(spark.request_id.clone()),
                    },
                    FreezeError::TerminalStatus(_, _) => ApiError {
                        status: StatusCode::CONFLICT,
                        label: "terminal_status",
                        request_id: Some(spark.request_id.clone()),
                    },
                    FreezeError::StoreUnavailable(_) => ApiError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        label: "store_unavailable",
                        request_id: Some(spark.request_id.clone()),
                    },
                })?;
            let json = FreezeJson::from_view(&view);
            let prev = json.previous_status.clone();
            let new_state = json.status;
            (
                serde_json::to_value(&json).unwrap_or(Value::Null),
                prev,
                new_state,
            )
        }
        FreezeState::Active => {
            let request = ThawRequest {
                reason: body.reason,
            };
            let view = ops::thaw(&store, &tenant_state_dir, &subject, &molecule_id, request)
                .map_err(|e| match &e {
                    ThawError::MoleculeNotFound(_) => ApiError {
                        status: StatusCode::NOT_FOUND,
                        label: "not_found",
                        request_id: Some(spark.request_id.clone()),
                    },
                    ThawError::InvalidStatus(_, _) => ApiError {
                        status: StatusCode::CONFLICT,
                        label: "invalid_status",
                        request_id: Some(spark.request_id.clone()),
                    },
                    ThawError::StoreUnavailable(_) => ApiError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        label: "store_unavailable",
                        request_id: Some(spark.request_id.clone()),
                    },
                })?;
            let json = ThawJson::from_view(&view);
            let prev = json.previous_status.clone();
            let new_state = json.status;
            (
                serde_json::to_value(&json).unwrap_or(Value::Null),
                prev,
                new_state,
            )
        }
    };

    state.events.publish(MoleculeEvent::state_changed(
        spark.noyau.as_str(),
        molecule_id.as_str(),
        prev_status,
        new_status,
    ));

    Ok(Json(json!({
        "request_id": spark.request_id,
        "freeze": body_value,
    })))
}

/// `POST /v1/molecules/:id/thaw` — **removed** in v1.0.0-rc, returns
/// 410 Gone with a pointer to the fused `freeze {state: "active"}`
/// endpoint.
///
/// Kept mounted for **2 minor releases** (until v1.2.0) for tenant
/// migration ergonomics; then dropped to fall back to axum 404.
pub async fn thaw_gone_handler(
    AxumPath(_molecule_id_str): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "error": "endpoint_removed",
            "hint": "POST /v1/molecules/{id}/freeze {\"state\":\"active\",\"reason\":\"...\"}",
            "removed_in": "v1.0.0-rc",
            "fallback_until": "v1.2.0",
        })),
    )
}

/// Body schema for `POST /v1/molecules/:id/stuck`.
#[derive(Debug, Deserialize)]
pub struct StuckBody {
    /// Mandatory free-form reason.
    pub reason: String,
}

/// `POST /v1/molecules/:id/stuck` — V1 mutation cut (T-CST-EXPAND).
pub async fn stuck_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    body: Result<Json<StuckBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_scope(
        &state,
        &jwt,
        "stuck",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;

    let Json(body) = body.map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "invalid_json_body",
        request_id: None,
    })?;
    if body.reason.trim().is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "empty_reason",
            request_id: None,
        });
    }

    let spark = build_spark(&state, &jwt, Verb::StuckMolecule, Some(&molecule_id_str))?;
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

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
    let subject = subject_for_jwt(&jwt);

    let request = StuckRequest {
        reason: body.reason,
    };
    let view = ops::stuck(&store, &tenant_state_dir, &subject, &molecule_id, request).map_err(
        |e| match &e {
            StuckError::MoleculeNotFound(_) => ApiError {
                status: StatusCode::NOT_FOUND,
                label: "not_found",
                request_id: Some(spark.request_id.clone()),
            },
            StuckError::EmptyReason => ApiError {
                status: StatusCode::BAD_REQUEST,
                label: "empty_reason",
                request_id: Some(spark.request_id.clone()),
            },
            StuckError::TerminalStatus(_, _) => ApiError {
                status: StatusCode::CONFLICT,
                label: "terminal_status",
                request_id: Some(spark.request_id.clone()),
            },
            StuckError::StoreUnavailable(_) => ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                label: "store_unavailable",
                request_id: Some(spark.request_id.clone()),
            },
        },
    )?;

    let prev_status = view.previous_status.to_string();
    let body = StuckJson::from_view(&view);
    let new_status = body.status;
    let body_value = serde_json::to_value(&body).unwrap_or(Value::Null);

    state.events.publish(MoleculeEvent::state_changed(
        spark.noyau.as_str(),
        molecule_id.as_str(),
        prev_status,
        new_status,
    ));

    Ok(Json(json!({
        "request_id": spark.request_id,
        "stuck": body_value,
    })))
}

// ---------------------------------------------------------------------------
// Tackle (`POST /v1/molecules/:id/tackle`) — library-direct (issue #54 U6)
// ---------------------------------------------------------------------------

/// `POST /v1/molecules/:id/tackle` — the dispatch cut, library-direct.
///
/// Since issue #54 U6 the route performs the dispatch **in-process**:
/// [`cosmon_runtime::LibraryExecutor`] runs the `plan → execute`
/// sequence (`cosmon_core::tackle_plan` decision half, worktree +
/// ledger-before-spawn effect half) and spawns the worker through the
/// adapter's [`crate::worker_env::EnvelopedBackend`] — the transport
/// port clamped with the §3.5 env allow-list. No `cs` binary is
/// involved; the ADR-080 §3.5 clause (e) subprocess envelope is
/// retired (see the ADR's U6 amendment).
///
/// Pipeline:
///
/// 1. Extract + validate JWT.
/// 2. Require `cosmon:molecule:write` AND `cosmon:worker:spawn`; emit
///    `AuthzDecisionEvaluated{verb=tackle, decision=Allow|Absent}`.
/// 3. Admission boundary (`http_request_to_spark`).
/// 4. Library-direct existence check + live-worker idempotence check +
///    per-noyau ceiling.
/// 5. In-process dispatch via the library executor; the worker session
///    metadata comes back as a typed [`cosmon_runtime::TackleReceipt`].
///
/// Errors mapped to:
/// - **404 `not_found`** — molecule id malformed or absent from store
///   (turing §8.2.3 — no existence oracle).
/// - **409 `already_active`** — the molecule already carries a live
///   worker process record (idempotence guard, checked in-process).
/// - **409 `not_tackleable`** — the molecule is in a terminal state.
/// - **429 `tackle_ceiling`** — the per-noyau live-worker ceiling.
/// - **501 `tackle_unsupported_step`** — the molecule's current formula
///   step is an execution kind the library executor does not cover yet
///   (gate / native / query / llm). The refusal is TYPED and names the
///   step kind in the body — never a silent fallback to a subprocess.
/// - **503 `worker_spawn_failed`** — the transport backend could not
///   open the worker session (the ledger has been rolled back).
/// - **503 `tackle_unavailable`** — stable fallback for any other
///   dispatch failure (store fault, git fault, unknown adapter).
#[allow(clippy::too_many_lines)] // Authentication, admission, and spawn stay auditable in order.
pub async fn tackle_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Response, ApiError> {
    // 1. Authorization header → JWT validation.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 2. Scope check + AuthzDecisionEvaluated emission. Tackle is the
    //    only verb that requires TWO scopes simultaneously
    //    (composition AND, not OR): `:molecule:write` for the state
    //    transition + `:worker:spawn` because spawning a worker burns
    //    real Anthropic budget. Audit emits both decisions so the
    //    operator can grep `grant_source` to distinguish "tenant has
    //    spawn" from "binding grants spawn implicitly"
    //    (task-20260522-b538 §3.4).
    authorise_scope(
        &state,
        &jwt,
        "tackle",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;
    authorise_scope(
        &state,
        &jwt,
        "tackle:spawn",
        &[SCOPE_WORKER_SPAWN],
        SCOPE_WORKER_SPAWN,
    )?;

    let spark = build_spark(&state, &jwt, Verb::TackleMolecule, Some(&molecule_id_str))?;
    tracing::debug!(
        request_id = %spark.request_id,
        noyau = %spark.noyau.as_str(),
        molecule_id = %molecule_id_str,
        "tackle admitted; validating target before subprocess dispatch"
    );

    // 4. Reject a malformed molecule id at the route boundary —
    //    collapses to 404 rather than letting `cs tackle` fail with a
    //    confusing message (turing §8.2.3, no existence oracle).
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| {
        tracing::debug!(
            request_id = %spark.request_id,
            molecule_id = %molecule_id_str,
            "tackle rejected: malformed molecule id"
        );
        ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        }
    })?;

    // Resolve the molecule through the same store path as GET before
    // dispatching. A dispatch error may mention "not found" for unrelated
    // reasons (for example a Git worktree collision); once this lookup
    // succeeds that text cannot be used as the API's molecule-existence
    // oracle.
    let molecule = run_observe(&state, &spark, &jwt, &molecule_id)?;

    // Idempotence guard, in-process: a molecule already carrying a LIVE
    // worker process record is not re-dispatched. This is the same
    // witness the ceiling below counts (active record + external PID
    // identity), so the two guards cannot disagree about liveness.
    if molecule
        .data
        .process
        .as_ref()
        .is_some_and(|p| p.is_active() && recorded_process_is_live(p.pid, p.pid_start_time))
    {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            label: "already_active",
            request_id: Some(spark.request_id.clone()),
        });
    }

    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        tracing::debug!(
            request_id = %spark.request_id,
            noyau = %spark.noyau.as_str(),
            tenant_root = %tenant_root.display(),
            "tackle rejected: tenant root does not exist"
        );
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 4b. Per-noyau live-worker ceiling — the pre-spawn seam
    //     (delib-20260709-943e M3, turing exploit #3). `:worker:spawn`
    //     grants the *right* to spawn; the ceiling bounds the *count*.
    //     Refuse the (N+1)-th concurrent worker with a stable
    //     `429 tackle_ceiling` BEFORE the subprocess is invoked, so no
    //     Anthropic credit is burned and no worktree is dropped once the
    //     cap is reached. Self-correcting: the count reads live process
    //     records from the noyau's own fleet state.
    let live_workers = count_live_workers(&tenant_root);
    if live_workers >= DEFAULT_TACKLE_CEILING_PER_NOYAU {
        tracing::warn!(
            noyau = %spark.noyau.as_str(),
            live_workers,
            ceiling = DEFAULT_TACKLE_CEILING_PER_NOYAU,
            request_id = %spark.request_id,
            "tackle refused: per-noyau live-worker ceiling reached"
        );
        state.metrics.record_reject("tackle_ceiling");
        return Err(ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            label: "tackle_ceiling",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 5. In-process dispatch. The worker envelope (allow-list env +
    //    tenant state-dir pin + artifact dir + key/model, ADR-080 §3.5
    //    as amended by U6) is clamped onto the spawn by
    //    `EnvelopedBackend`; the executor performs plan → worktree →
    //    ledger-before-spawn → spawn, with rollback symmetry on
    //    failure. `TackledBy::Human`: the dispatch answers a tenant's
    //    direct gesture, so the anti-preemption lease is sticky exactly
    //    as a CLI `cs tackle` would be.
    let artifact_dir = state
        .artifact_root
        .join(spark.noyau.as_str())
        .join(&molecule_id_str);
    // Best-effort mkdir (e653 spec): a failed mkdir does not abort the
    // dispatch — the worker fails later with a clearer error if the dir
    // truly cannot exist.
    let _ = std::fs::create_dir_all(&artifact_dir);
    let envelope = WorkerEnvelope {
        tenant_root: tenant_root.clone(),
        artifact_dir: Some(artifact_dir),
        anthropic_api_key: state.anthropic_api_key.clone(),
        claude_model: state.claude_model.clone(),
    };
    let backend = EnvelopedBackend::new(state.worker_backend.for_tenant(&tenant_root), &envelope);
    let executor = LibraryExecutor::new(&tenant_root, backend)
        .with_tackled_by(cosmon_core::tackle::TackledBy::Human);
    let dispatch_id = molecule_id.clone();
    // `Box` the typed error across the join so clippy's large-Err bound
    // holds; unboxed again at the match below.
    let dispatched = tokio::task::spawn_blocking(move || {
        executor
            .tackle(&dispatch_id, &DispatchPin::default())
            .map_err(Box::new)
    })
    .await
    .map_err(|_| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "tackle_unavailable",
        request_id: Some(spark.request_id.clone()),
    })?;
    let receipt = match dispatched.map_err(|boxed| *boxed) {
        Ok(receipt) => receipt,
        // The typed parity refusal names what it refuses: the step kind
        // (`gate` / `native` / `query` / `llm`) and the step id are
        // first-party static/formula tokens, safe on the wire — and a
        // caller can route the molecule through an operator-side
        // `cs tackle` from the label alone.
        Err(TackleExecError::UnsupportedStep { step_id, kind, .. }) => {
            tracing::warn!(
                request_id = %spark.request_id,
                molecule_id = %molecule_id_str,
                step_id = %step_id,
                step_kind = kind,
                "tackle refused: step kind unsupported by the library executor"
            );
            return Ok((
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "tackle_unsupported_step",
                    "step_kind": kind,
                    "step_id": step_id,
                    "request_id": spark.request_id,
                })),
            )
                .into_response());
        }
        Err(e) => {
            trace_tackle_dispatch_rejection(&spark, &molecule_id_str, &e);
            return Err(tackle_exec_error_to_response(&e, &spark.request_id));
        }
    };

    let body = json!({
        "molecule_id": molecule_id_str,
        "worker_session": receipt.session_name,
        "spawned_at": chrono::Utc::now().to_rfc3339(),
    });

    // tackle drives the molecule into `Running`. The previous status
    // is "pending" by §8j construction (a molecule with a live worker
    // was refused 409 above).
    state.events.publish(MoleculeEvent::state_changed(
        spark.noyau.as_str(),
        &molecule_id_str,
        "pending",
        "running",
    ));

    Ok(Json(json!({
        "request_id": spark.request_id,
        "tackle": body,
    }))
    .into_response())
}

/// Map a library-dispatch failure onto the wire.
///
/// Every outcome is a stable label; no store/git/transport detail
/// crosses the HTTP boundary (turing G9) — the diagnosis lives in the
/// server log ([`trace_tackle_dispatch_rejection`]).
///
/// The one refusal that carries structure is
/// [`TackleExecError::UnsupportedStep`]: the current formula step is an
/// execution kind the library executor does not cover yet (issue #54
/// U6, option (b) — the enumerated parity gap of the ADR-080
/// amendment). It answers **501 `tackle_unsupported_step`** with the
/// step kind NAMED in the body — a typed refusal, never a silent
/// fallback to a subprocess. The kind is a static token (`gate` /
/// `native` / `query` / `llm`), first-party by construction.
fn tackle_exec_error_to_response(err: &TackleExecError, request_id: &str) -> ApiError {
    match err {
        TackleExecError::NotTackleable { .. } => ApiError {
            status: StatusCode::CONFLICT,
            label: "not_tackleable",
            request_id: Some(request_id.to_owned()),
        },
        TackleExecError::UnsupportedStep { .. } => ApiError {
            status: StatusCode::NOT_IMPLEMENTED,
            label: "tackle_unsupported_step",
            request_id: Some(request_id.to_owned()),
        },
        TackleExecError::Spawn { .. } | TackleExecError::OrphanRetained { .. } => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "worker_spawn_failed",
            request_id: Some(request_id.to_owned()),
        },
        // The rollback wrapper adds *what was preserved*, never a different
        // failure class: the wire label stays the one the underlying
        // failure earns, and the preservation detail lives in the log.
        TackleExecError::RolledBackPreserving { source, .. } => {
            tackle_exec_error_to_response(source, request_id)
        }
        TackleExecError::State(_)
        | TackleExecError::Id(_)
        | TackleExecError::Ledger(_)
        | TackleExecError::UnknownAdapter(_)
        | TackleExecError::Git(_) => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "tackle_unavailable",
            request_id: Some(request_id.to_owned()),
        },
    }
}

/// Record the dispatch evidence needed to diagnose a tackle rejection
/// without leaking it into the HTTP response.
///
/// Emitted at **`warn`**, not `debug`. A tackle that fails is an
/// operational event, not trace noise: `tackle_unavailable` is a
/// catch-all, so the response alone cannot tell a git fault from a
/// store fault from a transport failure. The typed error rendered here
/// is the only thing that can, and at `debug` it is unreachable in
/// practice (production runs at `info`). The detail still does not
/// cross the HTTP boundary — the caller gets a label and a
/// `request_id`, and whoever holds the logs can correlate the two.
fn trace_tackle_dispatch_rejection(spark: &Spark, molecule_id: &str, err: &TackleExecError) {
    tracing::warn!(
        request_id = %spark.request_id,
        molecule_id,
        error = %err,
        "tackle dispatch rejected"
    );
}

// ---------------------------------------------------------------------------
// B2 bounded drain — run (`POST /v1/molecules/:id/run`)
// ---------------------------------------------------------------------------

/// `POST /v1/molecules/:id/run` — start the resident drain loop on the
/// DAG rooted at `:id` (B2 bounded drain, ADR-124; in-process since
/// issue #54 U6).
///
/// The client DEMANDS, the server DECIDES: the request
/// carries only the root molecule id; everything that governs the
/// drain — what gets tackled, when, under which bounds — is resolved
/// server-side. The B1/B2/B3 bounds come from the tenant's sealed
/// binding ([`crate::nucleon_map::DrainBounds`], operator-written,
/// readable via `GET /v1/quota`, never writable through any §8p
/// route) and land on the in-process loop as
/// [`cosmon_runtime::RunBounds`] plus the pre-loop depth check (see
/// [`crate::drain::run_drain`]). The loop runs INSIDE the tenant
/// container, co-located with the `StateStore` and `trunk.lock`
/// (design (a) — an advisory flock only binds holders on the same
/// filesystem).
///
/// The route returns **202 Accepted** as soon as the loop is spawned:
/// a drain dispatches real Claude workers and is hours-shaped, so the
/// HTTP boundary stays a request door, not a progress cockpit. The
/// spawn publishes `drain.started` on the events bus; the detached
/// task publishes `drain.terminated` with the NAMED reason token
/// (I4) when the loop exits — `drained`, `budget_exhausted`,
/// `molecule_quota_exceeded`, `max_depth_exceeded`, `timeout`, or
/// `error` (see [`crate::drain::exit_token`]). `teardown_failed` is
/// reserved for an attempted harvest a sealed `cs done` refused; the
/// library drain does not attempt harvest yet (the ADR-080 U6
/// amendment enumerates that follow-up).
///
/// Errors mapped to:
/// - **404 `not_found`** — molecule id malformed or tenant root
///   absent (turing §8.2.3 — no existence oracle).
/// - **409 `drain_already_active`** — a resident loop is already
///   draining this noyau (single-writer-trunk: one loop per noyau).
/// - **403** — missing `cosmon:molecule:write` or
///   `cosmon:worker:spawn` (same composed grid as tackle: a drain
///   spawns workers, i.e. burns Anthropic credit).
pub async fn run_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Response, ApiError> {
    // 1. Authorization header → JWT validation.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 2. Scope check — the same two-scope composition as tackle
    //    (`:molecule:write` AND `:worker:spawn`): the drain's whole
    //    point is to spawn workers, which burns real Anthropic budget.
    authorise_scope(
        &state,
        &jwt,
        "run",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;
    authorise_scope(
        &state,
        &jwt,
        "run:spawn",
        &[SCOPE_WORKER_SPAWN],
        SCOPE_WORKER_SPAWN,
    )?;

    // 3. Admission boundary (clauses a–d, materialise inbox).
    let spark = build_spark(&state, &jwt, Verb::RunMolecule, Some(&molecule_id_str))?;

    // 4. Malformed root id collapses to 404 (turing §8.2.3 — no
    //    existence oracle), same boundary as tackle.
    let root_molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 5. Bounds — resolved from the same sealed binding admission used,
    //    audience included (the read face of `GET /v1/quota` and this
    //    enforcement face project from one `Resolved`, so they cannot
    //    disagree). Pinning the audience is what makes that true: a
    //    principal federated on N galaxies holds N bindings, and an
    //    audience-blind lookup would enforce the first-sorting galaxy's
    //    budget. An absent `[drain_bounds]` section resolves to the
    //    server defaults: a tenant drain is NEVER unbounded (godel Q3,
    //    B3 obligatory).
    let bounds = state
        .nucleon_map
        .load()
        .resolve_for_audience(&jwt.iss, &jwt.sub, &jwt.aud)
        .map_or_else(crate::nucleon_map::DrainBounds::default, |r| r.drain_bounds);

    // 6. One resident loop per noyau (MCStitch I1 single-writer-trunk:
    //    a second loop would serialise on `trunk.lock` while burning
    //    budget). The slot is released by the detached task when the
    //    loop exits.
    let noyau = spark.noyau.as_str().to_owned();
    if !state.drains.try_acquire(&noyau) {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            label: "drain_already_active",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 7. Spawn the resident loop, detached — in-process since issue
    //    #54 U6: the same DAG loop `cs run <root>` executes
    //    ([`crate::drain::run_drain`]), with the library executor as
    //    its dispatch seam and the worker envelope clamped onto every
    //    spawn. The loop's deadline stays a NAMED exit (I4 —
    //    `timeout`), now as [`cosmon_runtime::ShutdownReason::Deadline`]
    //    instead of exit code 124.
    let drain_timeout = state.drain_timeout;
    let drain_timeout_secs = drain_timeout.as_secs();

    let bounds_json = json!({
        "budget": bounds.budget,
        "max_depth": bounds.max_depth,
        "max_molecules": bounds.max_molecules,
    });
    state.events.publish(MoleculeEvent::drain_started(
        &noyau,
        &molecule_id_str,
        bounds_json.clone(),
    ));

    let started_at = chrono::Utc::now().to_rfc3339();
    let root_artifact_dir = state
        .artifact_root
        .join(spark.noyau.as_str())
        .join(&molecule_id_str);
    let _ = std::fs::create_dir_all(&root_artifact_dir);
    let envelope = WorkerEnvelope {
        tenant_root: tenant_root.clone(),
        artifact_dir: Some(root_artifact_dir),
        anthropic_api_key: state.anthropic_api_key.clone(),
        claude_model: state.claude_model.clone(),
    };
    let backend = EnvelopedBackend::new(state.worker_backend.for_tenant(&tenant_root), &envelope);
    // Default actor class: `runtime:<pid>` — the drain's dispatches are
    // runtime claims (never sticky), exactly as `cs run`'s were.
    let executor = LibraryExecutor::new(&tenant_root, backend);
    spawn_resident_drain(
        Arc::clone(&state),
        tenant_root,
        root_molecule_id,
        bounds,
        drain_timeout,
        executor,
        noyau,
        molecule_id_str.clone(),
    );

    let body = json!({
        "request_id": spark.request_id,
        "drain": {
            "root": molecule_id_str,
            "status": "started",
            "bounds": bounds_json,
            "timeout_secs": drain_timeout_secs,
            "started_at": started_at,
        },
    });
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

// ---------------------------------------------------------------------------
// The harvest door — done (`POST /v1/molecules/:id/done`)
// ---------------------------------------------------------------------------

/// Body schema for `POST /v1/molecules/:id/done` — the full parameter set of
/// `cs done`.
///
/// `deny_unknown_fields` rather than a permissive parse: a body carrying
/// `strategu: "ff-only"` must be told, not silently harvested with the
/// default. Every field but `reason` is optional and defaults to what
/// `cs done` itself defaults to, so a body of `{"reason": "..."}` is the
/// documented bare harvest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoneBody {
    /// Why this molecule is being closed. **Mandatory**, and never
    /// fabricated: the withdrawn `land` gesture invented a generic sentence,
    /// which a year later is indistinguishable from one somebody meant.
    pub reason: Option<String>,
    /// `merge` (default) or `ff-only`.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Proceed even if the molecule is not in a terminal state.
    #[serde(default)]
    pub force: Option<bool>,
    /// Silent no-op when the molecule is not `Completed` or already merged.
    #[serde(default)]
    pub if_completed: Option<bool>,
    /// Skip merging the worker's branch into the base branch.
    #[serde(default)]
    pub no_merge: Option<bool>,
    /// Skip removing the git worktree.
    #[serde(default)]
    pub no_worktree_remove: Option<bool>,
    /// Skip deleting the worker's branch after the merge.
    #[serde(default)]
    pub no_branch_delete: Option<bool>,
    /// Skip killing the worker's session.
    #[serde(default)]
    pub no_kill: Option<bool>,
    /// Disable auto-propel escalation on merge conflict.
    #[serde(default)]
    pub no_auto_propel: Option<bool>,
    /// Custom message sent to the worker during auto-propel escalation.
    #[serde(default)]
    pub propel_message: Option<String>,
    /// Maximum number of auto-propel escalation retries.
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// Skip the blocking `[hooks] pre_done` gate for this invocation.
    #[serde(default)]
    pub skip_pre_done_hook: Option<bool>,
    /// Run the `[hooks] post_merge` deploy hook off the reference trunk.
    #[serde(default)]
    pub deploy_off_trunk: Option<bool>,
}

impl DoneBody {
    /// Fold the body into the domain options, refusing an unknown strategy.
    fn into_options(self) -> Result<HarvestOptions, &'static str> {
        let strategy = match self.strategy.as_deref() {
            None => cosmon_core::harvest_door::MergeStrategy::Merge,
            Some(token) => cosmon_core::harvest_door::MergeStrategy::from_token(token)
                .ok_or("unsupported_parameter")?,
        };
        let mut options = HarvestOptions::new(self.reason.unwrap_or_default());
        options.strategy = strategy;
        options.force = self.force.unwrap_or(false);
        options.if_completed = self.if_completed.unwrap_or(false);
        options.no_merge = self.no_merge.unwrap_or(false);
        options.no_worktree_remove = self.no_worktree_remove.unwrap_or(false);
        options.no_branch_delete = self.no_branch_delete.unwrap_or(false);
        options.no_kill = self.no_kill.unwrap_or(false);
        // The one default that differs from `cs done`'s, and the one
        // ADR-176 decision the D4 reversal does not carry with it: D6
        // disarms auto-propel on this path *by construction*. Escalation
        // injects a natural-language instruction — partly authored by the
        // requester through the molecule briefing — into a live worker
        // session, to resolve a conflict on the trunk, and renders the
        // result as `merged_after_n_escalation(s)`, a success label. That
        // is not a merge parameter, it is an agent dispatch wearing one,
        // so it stays off unless the requester asks for it and holds the
        // spawn scope that says they may spend agent budget.
        options.no_auto_propel = self.no_auto_propel.unwrap_or(true);
        options.propel_message = self.propel_message;
        options.max_retries = self.max_retries.unwrap_or(0);
        options.skip_pre_done_hook = self.skip_pre_done_hook.unwrap_or(false);
        options.deploy_off_trunk = self.deploy_off_trunk.unwrap_or(false);
        Ok(options)
    }
}

/// `POST /v1/molecules/:id/done` — the harvest door (ADR-176, issue #51),
/// as amended by the reversal of D4.
///
/// # A door, and now the operator's own verb through it
///
/// The route once refused every body but an empty one: "no option crosses
/// the wire" (D4), on the argument that *a derogation requested by its
/// beneficiary is not a derogation*. That argument holds only where the
/// requester is a constrained principal distinct from the party the gate
/// protects. On the deployment that exists — one galaxy, one nucleon, one
/// user — the requester **is** the operator, so the gate protected nobody
/// and withholding `--strategy` from someone merging into their own trunk
/// was an amputation of their own verb.
///
/// So the body carries the full argument set of `cs done`, folded into
/// [`HarvestOptions`] and carried unchanged to the merge. What did *not*
/// move is the authority: the JWT authenticates the requester, and the
/// operator-sealed grant authorises the effect, verified inside the trunk
/// lock with every fact re-derived there (ADR-176 D1, ADR-172 D3). A galaxy
/// that has not armed `[harvest_authority] required` still refuses
/// `not_authorized` for every call.
///
/// Restricting *which* molecules a requester may close remains the
/// multi-tenant question and is deliberately not answered here: no
/// ownership check, no `owner` field (D5 stands).
///
/// # The reason is mandatory here and nowhere else
///
/// `cs done` accepts `--reason` and records nothing when none is given —
/// the operator at their own terminal authors the history this writes. A
/// requester reaching over §8p does not, and the trunk-side reason is the
/// only account a later reader has of why someone else's molecule was
/// closed. A body with no reason is refused `missing_reason`; the door does
/// not invent one, which is the gap the reporters named in `land`.
///
/// # Never 202
///
/// The other out-of-process route on this surface (`/run`) answers 202
/// because a drain is hours-shaped. This one must not. A 202 on a
/// transaction that may integrate nothing rebuilds exactly the defect issue
/// #51 reports — the tenant reads success, the branch is stranded, and
/// nobody is coming.
///
/// # Named refusals
///
/// Every outcome is one of the [`DoorRefusal`] labels.
/// `base_not_fast_forward` maps to 503 alone: ADR-176 D7 classes it an
/// operator *configuration* error, decidable at arming time, and charging
/// it to the requester as a 4xx would convert the operator's
/// misconfiguration into the tenant's failure class.
///
/// # The effect half
///
/// The decision half runs in-process ([`cosmon_filestore::harvest_door::decide`]),
/// so every pre-effect refusal and the `already_landed` idempotence answer
/// with no `cs` binary present. The effect half — the sealed harvest
/// transaction, whose one implementation is `cmd/done.rs` — is injected
/// through [`crate::harvest_effect::HarvestEffectPort`]. A deployment that
/// declared no implementation answers the typed
/// **501 `harvest_effect_unavailable`** rather than a subprocess or a lie.
///
/// [`DoorRefusal`]: cosmon_core::harvest_door::DoorRefusal
pub async fn done_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    // 1. Authorization header → JWT validation. This proves *who asks*.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 2. Scope. `cosmon:molecule:write` alone — deliberately NOT the
    //    `+ worker:spawn` composition that tackle and run carry. Those two
    //    burn Anthropic credit; a harvest does not, unless auto-propel is
    //    armed, and auto-propel injects text into a live worker session.
    //    Requesting escalation is therefore the one option on this route
    //    that spends agent budget, and it is refused without the spawn
    //    scope below rather than by widening the whole route's gate.
    authorise_scope(
        &state,
        &jwt,
        "done",
        &[SCOPE_MOLECULE_WRITE],
        SCOPE_MOLECULE_WRITE,
    )?;

    // 3. Admission boundary (clauses a–d, materialise inbox).
    let spark = build_spark(&state, &jwt, Verb::DoneMolecule, Some(&molecule_id_str))?;

    // 4. The body. Empty is legal only in the sense that it fails the same
    //    way `{}` does — with `missing_reason`, named, rather than with a
    //    parse error the requester cannot act on.
    let parsed: DoneBody = if body.is_empty() {
        serde_json::from_slice(b"{}")
    } else {
        serde_json::from_slice(&body)
    }
    .map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        label: "unsupported_parameter",
        request_id: Some(spark.request_id.clone()),
    })?;
    let options = parsed.into_options().map_err(|label| ApiError {
        status: StatusCode::BAD_REQUEST,
        label,
        request_id: Some(spark.request_id.clone()),
    })?;
    if let Err(refusal) = options.validate() {
        return Err(door_refusal_to_api_error(refusal, &spark.request_id));
    }
    // Auto-propel is the one option that spends agent budget: it sends a
    // natural-language instruction into a live worker session and retries
    // the merge. Arming it therefore needs the spawn scope tackle and run
    // carry, checked here rather than on the whole route so a plain harvest
    // is not made to claim a budget it never touches.
    if !options.no_auto_propel && options.max_retries > 0 {
        authorise_scope(
            &state,
            &jwt,
            "done",
            &[SCOPE_WORKER_SPAWN],
            SCOPE_WORKER_SPAWN,
        )?;
    }

    // 5. Malformed id and absent tenant root both collapse to 404 — the same
    //    no-existence-oracle boundary the rest of the surface holds.
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        });
    }

    // 6. The decision half, in-process: the same library body the door has
    //    always run, over the tenant's own state files. Every pre-effect
    //    refusal — and the `already_landed` idempotent success — answers
    //    here without the `cs` binary existing at all.
    match decide_harvest_in_process(&tenant_root, &molecule_id, &options, &spark.request_id).await?
    {
        harvest_door::DoorDecision::AlreadyLanded { merged } => {
            let outcome = cosmon_core::harvest_door::DoorOutcome::AlreadyLanded { merged };
            // The retry answers with the same three facts the first call
            // did — including *why* nothing was integrated, when nothing
            // was. An idempotent reply that drops a field is not the same
            // reply.
            let non_integration = if merged {
                None
            } else {
                read_non_integration_tag(&tenant_root, &molecule_id).await
            };
            let body = json!({
                "request_id": spark.request_id,
                "harvest": {
                    "molecule": molecule_id_str,
                    "outcome": outcome.as_str(),
                    "merged": outcome.merged(),
                    "non_integration": non_integration,
                },
            });
            return Ok((StatusCode::OK, Json(body)).into_response());
        }
        harvest_door::DoorDecision::Proceed => {}
    }

    // 7. The effect half, through the port. The options travel unchanged
    //    from the request body to the merge; that is the whole point of the
    //    D4 reversal, and `a_requested_strategy_arrives_at_the_merge` in
    //    `cmd/done.rs` is what keeps it true.
    let (outcome, non_integration) = run_harvest_effect(
        &state,
        &tenant_root,
        &molecule_id,
        &options,
        &spark.request_id,
    )
    .await?;

    // `merged` is not decoration: `closed_without_merge` is a success, and
    // a client that read the 200 alone would believe the branch shipped.
    // The reason tag travels with it so the requester does not have to
    // fetch the result route to learn why nothing was integrated.
    let body = json!({
        "request_id": spark.request_id,
        "harvest": {
            "molecule": molecule_id_str,
            "outcome": outcome.as_str(),
            "merged": outcome.merged(),
            "non_integration": non_integration,
        },
    });
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// Run the door's effect half through the deployment's port and interpret
/// the result the way [`cosmon_filestore::harvest_door::land`] does.
///
/// `spawn_blocking` because both the effect and the trunk-side re-read are
/// synchronous work. The port's typed answers map onto the wire:
/// [`HarvestEffectError::Unavailable`] to the honest `501`, a named refusal
/// to its own status, and anything else to the anonymous `harvest_failed`.
///
/// Returns the outcome and, when the closure integrated nothing, the
/// kebab-case `non_integration` reason read back under the same blocking
/// task. The door's own vocabulary cannot carry that tag — it is a
/// `cosmon-state` type and the domain crate is upstream of it — so the
/// route reads it where the state is already open rather than inventing a
/// second spelling of it.
async fn run_harvest_effect(
    state: &Arc<AppState>,
    tenant_root: &std::path::Path,
    molecule_id: &MoleculeId,
    options: &HarvestOptions,
    request_id: &str,
) -> Result<(cosmon_core::harvest_door::DoorOutcome, Option<String>), ApiError> {
    let effect = Arc::clone(&state.harvest_effect);
    let root = tenant_root.to_path_buf();
    let id = molecule_id.clone();
    let opts = options.clone();
    let state_dir = tenant_root.join(".cosmon").join("state");
    let config_path = tenant_root.join(".cosmon").join("config.toml");
    let unavailable = ApiError {
        status: StatusCode::NOT_IMPLEMENTED,
        label: "harvest_effect_unavailable",
        request_id: Some(request_id.to_owned()),
    };
    let failed = ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "harvest_failed",
        request_id: Some(request_id.to_owned()),
    };

    let joined = tokio::task::spawn_blocking(move || {
        let store = FileStore::new(&state_dir);
        let cfg = cosmon_filestore::load_project_config(&config_path)
            .unwrap_or_else(|_| cosmon_core::config::ProjectConfig::default());
        let mut bridge = PortBackedEffect {
            port: effect.as_ref(),
            root,
        };
        let outcome = harvest_door::land(&store, &cfg, &id, &opts, &mut bridge)?;
        // Read back inside the same blocking task: the effect has
        // returned, the store is open, and the tag is exactly the one the
        // result route publishes for this molecule.
        let reason = if outcome.merged() {
            None
        } else {
            non_integration_tag(&store, &id)
        };
        Ok::<_, harvest_door::LandError>((outcome, reason))
    })
    .await;

    let Ok(result) = joined else {
        return Err(failed);
    };
    match result {
        Ok(outcome) => Ok(outcome),
        Err(harvest_door::LandError::Refused(refused)) => {
            Err(door_refusal_to_api_error(refused.refusal, request_id))
        }
        Err(harvest_door::LandError::Fault(cosmon_core::error::CosmonError::MoleculeNotFound(
            _,
        ))) => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(request_id.to_owned()),
        }),
        Err(harvest_door::LandError::EffectFailed(message))
            if message == crate::harvest_effect::UNAVAILABLE_MARKER =>
        {
            tracing::warn!(
                request_id = %request_id,
                molecule_id = %molecule_id,
                "the door admitted the harvest, but this deployment declared no \
                 harvest effect (ADR-176 §11) — refusing harvest_effect_unavailable"
            );
            Err(unavailable)
        }
        Err(_) => Err(failed),
    }
}

/// Bridge the adapter's [`HarvestEffectPort`] onto the door's
/// [`SealedHarvestEffect`] seam.
///
/// Two traits rather than one because they answer to different owners: the
/// door's seam is a library contract shared with the CLI, and the adapter's
/// port is the deployment's choice of implementation. This is the ten lines
/// that keep them from becoming one type with two reasons to change.
///
/// [`HarvestEffectPort`]: crate::harvest_effect::HarvestEffectPort
/// [`SealedHarvestEffect`]: cosmon_filestore::harvest_door::SealedHarvestEffect
struct PortBackedEffect<'a> {
    port: &'a dyn crate::harvest_effect::HarvestEffectPort,
    root: std::path::PathBuf,
}

impl harvest_door::SealedHarvestEffect for PortBackedEffect<'_> {
    fn binds_trunk_lock(&self) -> bool {
        self.port.binds_trunk_lock()
    }

    fn harvest(&mut self, molecule: &MoleculeId, options: &HarvestOptions) -> Result<(), String> {
        use crate::harvest_effect::HarvestEffectError;
        match self.port.harvest(&self.root, molecule, options) {
            Ok(()) => Ok(()),
            Err(HarvestEffectError::Unavailable) => {
                Err(crate::harvest_effect::UNAVAILABLE_MARKER.to_owned())
            }
            Err(HarvestEffectError::Failed(message)) => Err(message),
            Err(HarvestEffectError::Refused(refusal)) => Err(refusal.as_str().to_owned()),
        }
    }
}

/// Run the door's in-process decision half over the tenant's own state and
/// map its typed answers onto the wire.
///
/// `spawn_blocking` because the store reads are synchronous filesystem
/// work. A refused decision becomes its named [`ApiError`]; a
/// `MoleculeNotFound` fault collapses to `404 not_found` — the same
/// no-existence-oracle boundary the rest of the surface holds; any other
/// fault stays an anonymous `harvest_failed`.
async fn decide_harvest_in_process(
    tenant_root: &std::path::Path,
    molecule_id: &MoleculeId,
    options: &HarvestOptions,
    request_id: &str,
) -> Result<harvest_door::DoorDecision, ApiError> {
    let tenant_state_dir = tenant_root.join(".cosmon").join("state");
    let tenant_config_path = tenant_root.join(".cosmon").join("config.toml");
    let decision_molecule = molecule_id.clone();
    let decision_options = options.clone();
    let decision = tokio::task::spawn_blocking(move || {
        let store = FileStore::new(&tenant_state_dir);
        let cfg = cosmon_filestore::load_project_config(&tenant_config_path)
            .unwrap_or_else(|_| cosmon_core::config::ProjectConfig::default());
        harvest_door::decide(&store, &cfg, &decision_molecule, &decision_options)
    })
    .await
    .map_err(|_| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: "harvest_failed",
        request_id: Some(request_id.to_owned()),
    })?;

    decision.map_err(|err| match err {
        harvest_door::LandError::Refused(refused) => {
            door_refusal_to_api_error(refused.refusal, request_id)
        }
        harvest_door::LandError::Fault(cosmon_core::error::CosmonError::MoleculeNotFound(_)) => {
            ApiError {
                status: StatusCode::NOT_FOUND,
                label: "not_found",
                request_id: Some(request_id.to_owned()),
            }
        }
        _ => ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            label: "harvest_failed",
            request_id: Some(request_id.to_owned()),
        },
    })
}

/// The kebab-case `non_integration` reason recorded on a molecule, or
/// `None` when the work is on the trunk (or the read fails).
///
/// One spelling of the tag for both harvest replies. It is
/// `cosmon_state`'s own `as_str`, the same string `GET /v1/molecules/:id/
/// result` publishes in its `integration` block — a second spelling here
/// would be a second vocabulary as far as a client script is concerned.
fn non_integration_tag(store: &FileStore, molecule: &MoleculeId) -> Option<String> {
    use cosmon_state::StateStore as _;
    store
        .load_molecule(molecule)
        .ok()
        .and_then(|m| m.non_integration)
        .map(|ni| ni.reason.as_str().to_owned())
}

/// [`non_integration_tag`] off the async path: the store read is
/// synchronous filesystem work, so it goes to the blocking pool like every
/// other state read on this route.
async fn read_non_integration_tag(
    tenant_root: &std::path::Path,
    molecule: &MoleculeId,
) -> Option<String> {
    let state_dir = tenant_root.join(".cosmon").join("state");
    let id = molecule.clone();
    tokio::task::spawn_blocking(move || non_integration_tag(&FileStore::new(&state_dir), &id))
        .await
        .ok()
        .flatten()
}

/// Map a named door refusal to its wire status and label.
///
/// One mapping for both arrival paths — the in-process decision half and
/// the subprocess effect's exit code — so a refusal cannot change status
/// depending on which half of the door produced it.
fn door_refusal_to_api_error(
    refusal: cosmon_core::harvest_door::DoorRefusal,
    request_id: &str,
) -> ApiError {
    use cosmon_core::harvest_door::DoorRefusal;

    let status = match refusal {
        // The requester holds a token but no authority, or holds neither —
        // and, for a reservation, a human attached a condition only a human
        // lifts. Both are 403 for the same reason: the door is not going to
        // do this for anybody who asks the same way again.
        DoorRefusal::NotAuthorized | DoorRefusal::ReservationRequiresSeal => StatusCode::FORBIDDEN,
        // State conflicts: the work is not landable *right now*, and the
        // requester can tell from the label what would change that.
        DoorRefusal::NotCompleted | DoorRefusal::MergeConflict => StatusCode::CONFLICT,
        // A bounded queue at its bound. 429 rather than 409 because the
        // honest reading is "later, not never" — and because the ceiling is
        // an operator's quantity, which is what 429 means everywhere else on
        // this surface.
        DoorRefusal::BacklogFull | DoorRefusal::PreDoneRefused => StatusCode::TOO_MANY_REQUESTS,
        // A fault of the argument set, not of the world: the caller can fix
        // it by saying why, and the door will not say it for them.
        DoorRefusal::MissingReason => StatusCode::BAD_REQUEST,
        // ADR-176 D7 — an operator configuration fault, never charged to the
        // requester as a 4xx.
        DoorRefusal::BaseNotFastForward => StatusCode::SERVICE_UNAVAILABLE,
    };
    ApiError {
        status,
        label: refusal.as_str(),
        request_id: Some(request_id.to_owned()),
    }
}

/// Detach the resident drain: run the in-process DAG loop
/// ([`crate::drain::run_drain`]) to its named termination, publish
/// `drain.terminated` with the stable reason token, and release the
/// noyau's drain slot. The route returns 202 while this lives on.
///
/// `spawn_blocking` because the runtime loop is synchronous (it sleeps
/// between ticks); the drain slot is released on EVERY exit path,
/// including a panicking loop (the `JoinError` arm), so a wedged drain
/// can never brick the noyau's slot for the life of the process.
#[allow(clippy::too_many_arguments)] // The drain's inputs are exactly these; a struct would rename, not reduce.
fn spawn_resident_drain(
    state: Arc<AppState>,
    tenant_root: std::path::PathBuf,
    root_molecule_id: MoleculeId,
    bounds: crate::nucleon_map::DrainBounds,
    timeout: std::time::Duration,
    executor: LibraryExecutor<EnvelopedBackend<SharedBackend>>,
    noyau: String,
    root_id: String,
) {
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || {
            drain::run_drain(&tenant_root, &root_molecule_id, &bounds, timeout, executor)
        })
        .await
        .unwrap_or(drain::DrainOutcome {
            token: drain::token::ERROR,
            detail: None,
        });
        let reason = outcome.token;
        tracing::info!(
            target: "cosmon_rpp_adapter::drain",
            noyau = %noyau,
            root = %root_id,
            reason,
            detail = outcome.detail.as_deref(),
            "resident drain terminated"
        );
        state.events.publish(MoleculeEvent::drain_terminated(
            &noyau,
            &root_id,
            reason,
            outcome.detail.as_deref(),
        ));
        state.drains.release(&noyau);
    });
}

/// Extract a `Vec<(key, value)>` from the JSON `variables` field.
fn parse_variables(raw: Option<&Value>) -> Result<Vec<(String, String)>, &'static str> {
    let Some(value) = raw else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let map: &Map<String, Value> = value.as_object().ok_or("variables_not_object")?;
    let mut out = Vec::with_capacity(map.len());
    for (k, v) in map {
        match v {
            Value::String(s) => out.push((k.clone(), s.clone())),
            Value::Null => {}
            _ => return Err("variables_not_string"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn extracts_bearer_token() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc.def.ghi"),
        );
        assert_eq!(extract_bearer(&h).unwrap(), "abc.def.ghi");
    }

    #[test]
    fn missing_header_yields_typed_error() {
        let h = HeaderMap::new();
        let err = extract_bearer(&h).unwrap_err();
        assert!(matches!(err, RppRejectReason::MissingAuthorization));
    }

    #[test]
    fn non_bearer_scheme_is_malformed() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Basic abc"),
        );
        let err = extract_bearer(&h).unwrap_err();
        assert!(matches!(err, RppRejectReason::MalformedJwt));
    }

    /// The drain slot is one-per-noyau and reusable after release.
    #[test]
    fn drain_registry_single_slot_per_noyau() {
        let reg = crate::DrainRegistry::default();
        assert!(reg.try_acquire("a"));
        assert!(!reg.try_acquire("a"), "second drain on the same noyau");
        assert!(reg.try_acquire("b"), "independent noyau is independent");
        assert!(reg.is_active("a"));
        reg.release("a");
        assert!(!reg.is_active("a"));
        assert!(reg.try_acquire("a"), "slot reusable after release");
    }

    #[test]
    fn parse_variables_none_or_null_yields_empty() {
        assert!(parse_variables(None).unwrap().is_empty());
        let null = Value::Null;
        assert!(parse_variables(Some(&null)).unwrap().is_empty());
    }

    #[test]
    fn parse_variables_accepts_string_values() {
        let v = json!({"topic": "hello", "owner": "operator-demo"});
        let out = parse_variables(Some(&v)).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|(k, v)| k == "topic" && v == "hello"));
        assert!(out
            .iter()
            .any(|(k, v)| k == "owner" && v == "operator-demo"));
    }

    #[test]
    fn parse_variables_rejects_non_string_value() {
        let v = json!({"topic": 42});
        let err = parse_variables(Some(&v)).unwrap_err();
        assert_eq!(err, "variables_not_string");
    }

    #[test]
    fn parse_variables_rejects_non_object() {
        let v = json!(["topic", "owner"]);
        let err = parse_variables(Some(&v)).unwrap_err();
        assert_eq!(err, "variables_not_object");
    }

    /// Transport-level spawn failure maps to `worker_spawn_failed` —
    /// the ledger has already been rolled back when this surfaces.
    #[test]
    fn spawn_failure_maps_to_worker_spawn_failed() {
        let api = tackle_exec_error_to_response(
            &TackleExecError::Spawn {
                id: Box::new(MoleculeId::new("task-20260905-0001").unwrap()),
                reason: "tmux new-session failed".to_owned(),
            },
            "req-spawn",
        );
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.label, "worker_spawn_failed");
    }

    /// A terminal molecule is a 409 with its own name — not the old
    /// stderr-derived `already_active`, which now means "live worker".
    #[test]
    fn terminal_molecule_maps_to_not_tackleable() {
        let api = tackle_exec_error_to_response(
            &TackleExecError::NotTackleable {
                id: Box::new(MoleculeId::new("task-20260905-0002").unwrap()),
                status: "completed".to_owned(),
            },
            "req-terminal",
        );
        assert_eq!(api.status, StatusCode::CONFLICT);
        assert_eq!(api.label, "not_tackleable");
    }

    /// The parity refusal is 501, and stays typed: the caller can tell
    /// "this adapter build does not cover this step kind" apart from
    /// every operational 503.
    #[test]
    fn unsupported_step_maps_to_501_typed_refusal() {
        let api = tackle_exec_error_to_response(
            &TackleExecError::UnsupportedStep {
                id: Box::new(MoleculeId::new("task-20260905-0003").unwrap()),
                step_id: "verify".to_owned(),
                kind: "gate",
            },
            "req-unsupported",
        );
        assert_eq!(api.status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(api.label, "tackle_unsupported_step");
    }

    /// Everything else (git fault, store fault, unknown adapter) stays
    /// the generic `tackle_unavailable` — the stable fallback, with the
    /// diagnosis in the server log rather than on the wire (turing G9).
    #[test]
    fn unclassified_dispatch_failure_stays_tackle_unavailable() {
        let api = tackle_exec_error_to_response(
            &TackleExecError::Git("not a git repository".to_owned()),
            "req-git",
        );
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.label, "tackle_unavailable");
    }
}
