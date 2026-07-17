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
use cosmon_core::id::{FleetId, MoleculeId};
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
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

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::error::{ApiError, RppRejectReason};
use crate::events_bus::MoleculeEvent;
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::subprocess::{parse_cs_json, run_molecule_args, tackle_molecule_args, SystemInvoker};
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
// T9 remote-tackle V2 — tackle (`POST /v1/molecules/:id/tackle`)
// ---------------------------------------------------------------------------

/// `POST /v1/molecules/:id/tackle` — V1 dispatch cut (T9 remote-tackle V2).
///
/// Unlike the other §8p verbs, tackle is **not** library-direct: it
/// shells out to `cs tackle <id>` inside the per-tenant container,
/// which in turn launches Claude Code via tmux. The original §3.5
/// subprocess envelope is reinstated for this single verb (it was
/// dormant since T-RPP-LIB-DIRECT; the [`SystemInvoker`] machinery
/// is untouched and we reuse it here).
///
/// Pipeline:
///
/// 1. Extract + validate JWT.
/// 2. Require `cosmon:molecule:write` scope; emit
///    `AuthzDecisionEvaluated{verb=tackle, decision=Allow|Absent}`.
/// 3. Admission boundary (`http_request_to_spark`).
/// 4. Subprocess invocation: `cs --json tackle <id> --force` against the
///    per-tenant `cwd` (`<galaxies_root>/<noyau>`).
/// 5. Parse the `cs --json tackle` stdout into a wire-stable
///    `{ molecule_id, worker_session, spawned_at }` triple.
///
/// Errors mapped to:
/// - **404 `not_found`** — molecule id malformed or absent from store
///   (turing §8.2.3 — no existence oracle).
/// - **409 `already_active`** — `cs tackle` refused because a worker
///   session is already attached (idempotency check inside the CLI).
/// - **503 `tackle_unavailable`** — `cs` binary missing, subprocess
///   spawn failed, or Claude Code not installed in the container.
/// - **504 `subprocess_timeout`** — `cs tackle` exceeded the per-call
///   subprocess deadline (default 30s; tackle should return in <5s
///   after the tmux session is detached).
#[allow(clippy::too_many_lines)] // Authentication, admission, and spawn stay auditable in order.
pub async fn tackle_molecule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
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

    // Resolve the molecule through the same store path as GET before spawning
    // `cs`. A subprocess error may mention "not found" for unrelated reasons
    // (for example a Git worktree collision); once this lookup succeeds that
    // text cannot be used as the API's molecule-existence oracle.
    let _molecule = run_observe(&state, &spark, &jwt, &molecule_id)?;

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

    // 5. Subprocess invocation. The §3.5 envelope (`COSMON_API_REQUEST=1`,
    //    request-id correlation, resolved nucleon, per-tenant cwd,
    //    hard timeout) is set inside `SystemInvoker::invoke_owned`.
    //    `with_artifact_root` (e653 spec, task-20260522-ef4f)
    //    materialises `<artifact_root>/<noyau>/<molecule_id>/` and
    //    exports it as `COSMON_ARTIFACT_DIR` so the worker writes its
    //    outputs at the canonical path the GET `/artifacts` route
    //    later reads from.
    let invoker = SystemInvoker::new(
        state.cs_path.clone(),
        state.galaxies_root.clone(),
        state.subprocess_timeout,
    )
    .with_anthropic_key(state.anthropic_api_key.clone())
    .with_artifact_root(Some(state.artifact_root.clone()));
    let args = tackle_molecule_args(&molecule_id_str);
    let invocation = invoker.invoke_owned(&spark, &args).await.map_err(|e| {
        trace_tackle_subprocess_rejection(&spark, &molecule_id_str, &e);
        tackle_subprocess_error_to_api(&e, &spark.request_id)
    })?;

    // 6. Parse the `cs --json tackle` stdout. We accept either the
    //    canonical multi-line JSON object or NDJSON (last non-empty
    //    line wins) — the same shape acceptance applied by observe and
    //    nucleate.
    let parsed = parse_cs_json(&invocation.stdout).map_err(|e| {
        tracing::debug!(
            request_id = %spark.request_id,
            molecule_id = %molecule_id_str,
            reason = %e,
            "tackle subprocess returned unparsable output"
        );
        tackle_subprocess_error_to_api(&e, &spark.request_id)
    })?;

    let body = build_tackle_response(&molecule_id_str, &parsed);

    // tackle drives the molecule into `Running`. The previous status
    // is "pending" by §8j construction (the cs CLI rejects tackle on
    // already-running molecules with `already running`, mapped to 409
    // before we get here).
    state.events.publish(MoleculeEvent::state_changed(
        spark.noyau.as_str(),
        &molecule_id_str,
        "pending",
        "running",
    ));

    Ok(Json(json!({
        "request_id": spark.request_id,
        "tackle": body,
    })))
}

/// Project the `cs --json tackle` stdout into the wire-stable
/// `{ molecule_id, worker_session, spawned_at }` triple.
///
/// `cs --json tackle` today emits the worker session under the
/// `tmux_session` key (see `crates/cosmon-cli/src/cmd/tackle.rs`); the
/// projection reads that key first and keeps `session_name` / `session`
/// / `worker_id` as forward/backward-compat fallbacks for older or
/// future CLI shapes. `spawned_at` is read from the CLI output when
/// present, otherwise derived from the current wall clock. The
/// conservative projection keeps the wire body shallow rather than
/// forwarding every CLI-internal field (mirrors the
/// `ObserveJson::from_view` discipline on the read path).
fn build_tackle_response(molecule_id: &str, parsed: &Value) -> Value {
    let worker_session = parsed
        .get("tmux_session")
        .or_else(|| parsed.get("session_name"))
        .or_else(|| parsed.get("session"))
        .or_else(|| parsed.get("worker_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let spawned_at = parsed
        .get("spawned_at")
        .or_else(|| parsed.get("tackled_at"))
        .cloned()
        .unwrap_or_else(|| {
            let now = chrono::Utc::now().to_rfc3339();
            Value::String(now)
        });
    json!({
        "molecule_id": molecule_id,
        "worker_session": worker_session,
        "spawned_at": spawned_at,
    })
}

/// Map a subprocess-side rejection into the wire-stable
/// [`ApiError`] for the tackle route.
///
/// The mapping is deliberately conservative: any non-zero exit from
/// `cs tackle` whose stderr mentions the canonical idempotency string
/// is collapsed to 409 `already_active`, otherwise to 503
/// `tackle_unavailable`. The discipline mirrors the §3.6 reject
/// taxonomy without leaking the raw stderr to the wire (turing G9).
fn tackle_subprocess_error_to_api(reason: &RppRejectReason, request_id: &str) -> ApiError {
    match reason {
        RppRejectReason::SubprocessTimeout(_) => ApiError {
            status: StatusCode::GATEWAY_TIMEOUT,
            label: "subprocess_timeout",
            request_id: Some(request_id.to_owned()),
        },
        RppRejectReason::SubprocessExitNonZero { stderr_excerpt, .. } => {
            // The cs CLI signals "already tackled" with a distinct
            // message ("already running", "worker already attached",
            // …). The substring match is intentionally tolerant —
            // a tighter contract is a §10 amendment.
            let lower = stderr_excerpt.to_ascii_lowercase();
            if lower.contains("already running")
                || lower.contains("already tackled")
                || lower.contains("worker already")
            {
                ApiError {
                    status: StatusCode::CONFLICT,
                    label: "already_active",
                    request_id: Some(request_id.to_owned()),
                }
            } else {
                ApiError {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    label: "tackle_unavailable",
                    request_id: Some(request_id.to_owned()),
                }
            }
        }
        _ => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "tackle_unavailable",
            request_id: Some(request_id.to_owned()),
        },
    }
}

/// Record the child-process evidence needed to diagnose a tackle rejection
/// without leaking it into the HTTP response.
fn trace_tackle_subprocess_rejection(spark: &Spark, molecule_id: &str, reason: &RppRejectReason) {
    if let RppRejectReason::SubprocessExitNonZero { stderr_excerpt, .. } = reason {
        tracing::debug!(
            request_id = %spark.request_id,
            molecule_id,
            stderr_excerpt,
            "tackle subprocess rejected"
        );
    } else {
        tracing::debug!(
            request_id = %spark.request_id,
            molecule_id,
            reason = %reason,
            "tackle subprocess rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// B2 bounded drain — run (`POST /v1/molecules/:id/run`)
// ---------------------------------------------------------------------------

/// `POST /v1/molecules/:id/run` — start the resident drain loop on the
/// DAG rooted at `:id` (B2 bounded drain, ADR-124).
///
/// The client DEMANDS, the server DECIDES: the request
/// carries only the root molecule id; everything that governs the
/// drain — what gets tackled, when, under which bounds — is resolved
/// server-side. The B1/B2/B3 bounds come from the tenant's sealed
/// binding ([`crate::nucleon_map::DrainBounds`], operator-written,
/// readable via `GET /v1/quota`, never writable through any §8p
/// route) and are turned into `cs run` flags by
/// [`run_molecule_args`]. The loop runs INSIDE the tenant container,
/// co-located with the `StateStore` and `trunk.lock` (design (a) —
/// an advisory flock only binds holders on the same filesystem).
///
/// The route returns **202 Accepted** as soon as the loop is spawned:
/// a drain dispatches real Claude workers and is hours-shaped, so the
/// HTTP boundary stays a request door, not a progress cockpit. The
/// spawn publishes `drain.started` on the events bus; the detached
/// task publishes `drain.terminated` with the NAMED reason token
/// (I4) when the loop exits — `drained`, `budget_exhausted`,
/// `molecule_quota_exceeded`, `max_depth_exceeded`, `timeout`, or
/// `error` (see [`drain_exit_reason`]).
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
    let _molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
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

    // 5. Bounds — resolved from the same sealed binding admission used
    //    (the read face of `GET /v1/quota` and this enforcement face
    //    project from one `Resolved`, so they cannot disagree). An
    //    absent `[drain_bounds]` section resolves to the server
    //    defaults: a tenant drain is NEVER unbounded (godel Q3, B3
    //    obligatory).
    let bounds = state
        .nucleon_map
        .load()
        .resolve(&jwt.iss, &jwt.sub)
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

    // 7. Spawn the resident loop, detached. The invoker timeout gets
    //    a one-minute grace over the `--timeout` handed to `cs run`,
    //    so the loop's own NAMED deadline exit (124 → `timeout`)
    //    always wins over the envelope's anonymous kill.
    let drain_timeout_secs = crate::DEFAULT_DRAIN_TIMEOUT.as_secs();
    let invoker = SystemInvoker::new(
        state.cs_path.clone(),
        state.galaxies_root.clone(),
        crate::DEFAULT_DRAIN_TIMEOUT + std::time::Duration::from_secs(60),
    )
    .with_anthropic_key(state.anthropic_api_key.clone())
    .with_claude_model(state.claude_model.clone())
    .with_artifact_root(Some(state.artifact_root.clone()));
    let args = run_molecule_args(&molecule_id_str, &bounds, drain_timeout_secs);

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
    spawn_resident_drain(
        Arc::clone(&state),
        invoker,
        spark.clone(),
        args,
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

/// Detach the resident drain: run the `cs run` subprocess to its
/// named exit, publish `drain.terminated` with the stable reason
/// token, and release the noyau's drain slot. The route returns 202
/// while this lives on.
fn spawn_resident_drain(
    state: Arc<AppState>,
    invoker: SystemInvoker,
    spark: Spark,
    args: Vec<String>,
    noyau: String,
    root_id: String,
) {
    tokio::spawn(async move {
        let outcome = invoker.invoke_owned(&spark, &args).await;
        let reason = match &outcome {
            Ok(_) => drain_exit_reason(0),
            Err(RppRejectReason::SubprocessExitNonZero { code, .. }) => drain_exit_reason(*code),
            Err(RppRejectReason::SubprocessTimeout(_)) => "timeout",
            Err(_) => "error",
        };
        tracing::info!(
            target: "cosmon_rpp_adapter::drain",
            noyau = %noyau,
            root = %root_id,
            reason,
            "resident drain terminated"
        );
        state
            .events
            .publish(MoleculeEvent::drain_terminated(&noyau, &root_id, reason));
        state.drains.release(&noyau);
    });
}

/// Map a `cs run` exit code to the stable drain-termination reason
/// token published in `drain.terminated` events.
///
/// The non-zero tokens are the SAME strings as the
/// [`RppRejectReason::DrainBudgetExhausted`] /
/// [`RppRejectReason::DrainMoleculeQuotaExceeded`] /
/// [`RppRejectReason::DrainMaxDepthExceeded`] labels (B1 moussage)
/// — the client learns the bound by the
/// documented token without being able to lift it. The mirror is
/// pinned by `drain_exit_reason_mirrors_reject_labels` below.
#[must_use]
pub fn drain_exit_reason(code: i32) -> &'static str {
    match code {
        0 => "drained",
        90 => "budget_exhausted",
        91 => "molecule_quota_exceeded",
        92 => "max_depth_exceeded",
        124 => "timeout",
        _ => "error",
    }
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

    /// The drain-termination tokens published in `drain.terminated`
    /// events must be byte-identical to the B1 reject-reason labels
    /// — one vocabulary for the bound, whether
    /// the client meets it as an HTTP refusal or as an event token.
    #[test]
    fn drain_exit_reason_mirrors_reject_labels() {
        assert_eq!(
            drain_exit_reason(90),
            RppRejectReason::DrainBudgetExhausted.label()
        );
        assert_eq!(
            drain_exit_reason(91),
            RppRejectReason::DrainMoleculeQuotaExceeded.label()
        );
        assert_eq!(
            drain_exit_reason(92),
            RppRejectReason::DrainMaxDepthExceeded.label()
        );
        assert_eq!(drain_exit_reason(0), "drained");
        assert_eq!(drain_exit_reason(124), "timeout");
        assert_eq!(drain_exit_reason(1), "error");
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

    /// `build_tackle_response` must read the worker session from the
    /// real `cs --json tackle` shape, which emits it under
    /// `tmux_session` — not `session_name` / `session` / `worker_id`.
    /// Regression guard for Gap 6 (smithy V2 E2E, 2026-05-14): the
    /// projection used to return `worker_session: null` because none
    /// of the looked-up keys matched the actual CLI output. Mirrors
    /// the `parse_cs_json` pretty-multiline test added after a fixture
    /// masked a divergence.
    #[test]
    fn build_tackle_response_reads_tmux_session_from_real_cli_shape() {
        let parsed = json!({
            "command": "tackle",
            "molecule_id": "task-20260514-f02f",
            "status": "Running",
            "tmux_session": "cosmon-task-20260514-f02f",
            "worktree": "/srv/cosmon/cosmon/.worktrees/task-20260514-f02f",
            "branch": "feat/task-20260514-f02f",
            "attach": "tmux -L cosmon attach -t cosmon-task-20260514-f02f",
            "spawned_at": "2026-05-14T12:00:00+00:00",
        });
        let body = build_tackle_response("task-20260514-f02f", &parsed);
        assert_eq!(body["molecule_id"], "task-20260514-f02f");
        assert_eq!(body["worker_session"], "cosmon-task-20260514-f02f");
        assert!(
            !body["worker_session"].is_null(),
            "worker_session must not be null for the real CLI shape"
        );
        assert_eq!(body["spawned_at"], "2026-05-14T12:00:00+00:00");
    }

    /// The legacy fallback keys remain honoured: if a future or older
    /// CLI emits `session_name` instead of `tmux_session`, the
    /// projection still resolves it rather than returning null.
    #[test]
    fn build_tackle_response_falls_back_to_legacy_session_keys() {
        let parsed = json!({ "session_name": "legacy-session" });
        let body = build_tackle_response("mol-x", &parsed);
        assert_eq!(body["worker_session"], "legacy-session");
    }

    #[test]
    fn tackle_subprocess_not_found_text_is_not_an_existence_oracle() {
        let api = tackle_subprocess_error_to_api(
            &RppRejectReason::SubprocessExitNonZero {
                code: 1,
                stderr_excerpt: "fatal: worktree path not found".to_owned(),
            },
            "req-test",
        );
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.label, "tackle_unavailable");
    }
}
