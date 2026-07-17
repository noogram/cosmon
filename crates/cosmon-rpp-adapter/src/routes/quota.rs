// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/quota` and the shared `X-RateLimit-*` header helper.
//!
//! The route lets a tenant inspect *where it stands* in the leaky
//! bucket before being told *no* by a 429 — it is the symmetric read
//! face of [`crate::rate_limit::IngressRateLimiter::check_and_consume`].
//! The headers carry the same snapshot on every JWT-gated response so
//! a programmatic client can throttle itself without a second
//! round-trip.
//!
//! # Why a snapshot endpoint and not a counter
//!
//! The §8j ingress rate-limit is a **leaky bucket**, not a per-minute /
//! per-hour counter. The honest exposure is therefore burst capacity +
//! current level + the wall-clock at which the bucket fully drains
//! back to zero. Pretending otherwise (e.g. emitting a fake
//! `requests_per_minute` field) would invite tenant code to reason
//! about a model the server does not actually implement.
//!
//! # Anti-amplification
//!
//! `IngressRateLimiter::current_state` is a pure read — calling
//! `/v1/quota` does **not** consume a token. The route is, however,
//! still gated by the standard admission boundary (clauses a–d) so a
//! tenant who is globally killed or denied still gets the expected
//! rejection rather than an oracle into their own quota.

use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::Value;

use crate::admission::{http_request_to_spark, AdmissionRig, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{MOLECULE_READ, MOLECULE_WRITE};
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::rate_limit::{hash_sub, RateState};
use crate::AppState;

/// Header name for the bucket capacity (max tokens). Pinned as a const
/// so the surface-freeze test catches accidental renames.
pub const HEADER_RATE_LIMIT_LIMIT: HeaderName = HeaderName::from_static("x-ratelimit-limit");
/// Header name for the remaining tokens (floor of `capacity - level`).
pub const HEADER_RATE_LIMIT_REMAINING: HeaderName =
    HeaderName::from_static("x-ratelimit-remaining");
/// Header name for the wall-clock ISO-8601 reset time (bucket drains
/// back to zero).
pub const HEADER_RATE_LIMIT_RESET: HeaderName = HeaderName::from_static("x-ratelimit-reset");

/// Wire shape for the `GET /v1/quota` response body.
#[derive(Debug, Serialize)]
pub struct QuotaResponse {
    /// Echoed back so the operator can cross-reference the audit log.
    pub request_id: String,
    /// Bucket configuration.
    pub limits: QuotaLimits,
    /// Tenant-specific state, snapshotted to the request's `now`.
    pub current: QuotaCurrent,
    /// Burst tokens still available (floor of `capacity - level`).
    pub remaining: i64,
    /// ISO-8601 wall-clock at which the bucket will be fully drained
    /// (equal to `now` when no leak is configured).
    pub reset_at: String,
    /// Effective drain bounds for this tenant's binding (B1/B2/B3
    /// moussage). Additive field — the symmetric
    /// READ face of the bounds the drain enforces: the client can ask
    /// "what is my depth/budget bound?" but no §8p route writes it
    /// (operator gesture on the sealed binding only).
    pub drain_bounds: QuotaDrainBounds,
}

/// Drain-bounds block of [`QuotaResponse`] (read-only exposure).
#[derive(Debug, Serialize)]
pub struct QuotaDrainBounds {
    /// B3 — max runtime actions per drain (the decreasing budget that
    /// forces termination; `cs run` exit 90 / 429 `budget_exhausted`).
    pub budget: u64,
    /// B1 — max DAG depth (`cs run` exit 92 / 409 `max_depth_exceeded`).
    pub max_depth: u32,
    /// B2 — max molecules while draining (`cs run` exit 91 /
    /// 429 `molecule_quota_exceeded`).
    pub max_molecules: u64,
}

/// Bucket configuration block of [`QuotaResponse`].
#[derive(Debug, Serialize)]
pub struct QuotaLimits {
    /// Maximum tokens the bucket can hold (V0: 30).
    pub burst_capacity: i64,
    /// Tokens drained per minute (V0: 10).
    pub leak_per_minute: f64,
    /// Tokens drained per hour (V0: 600).
    pub leak_per_hour: f64,
}

/// Tenant-specific snapshot block of [`QuotaResponse`].
#[derive(Debug, Serialize)]
pub struct QuotaCurrent {
    /// Current bucket level (after the wall-clock has drained pre-existing
    /// tokens since the last persisted write).
    pub bucket_level: f64,
    /// Floor of `bucket_level`, useful for table display.
    pub bucket_level_floor: i64,
}

/// `GET /v1/quota` — return the JWT tenant's current rate-limit
/// snapshot. JWT-gated and scope-gated (either `cosmon:molecule:read`
/// or `cosmon:molecule:write` — same as `GET /v1/molecules`).
///
/// The route also emits the standard `X-RateLimit-*` headers so a
/// caller can read them from any JWT-gated route — `/v1/quota` is the
/// dedicated read face but not the *only* surface.
pub async fn get_quota(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // Scope check — same accept-list as GET /v1/molecules: either read
    // or write admits.
    crate::routes::molecules::authorise_scope_public(
        &state,
        &jwt,
        "quota",
        &[MOLECULE_READ, MOLECULE_WRITE],
        MOLECULE_READ,
    )?;

    // Admission boundary — we want clauses a (audience pin), c (rate
    // limit consumption is part of the symmetric contract), d
    // (deny-list). The /quota call DOES consume a token like any other
    // /v1/ call — the route is informational, not free. That keeps
    // the budget honest: a tenant cannot poll /quota in a tight loop
    // to amplify denial elsewhere.
    let spark = crate::routes::molecules::build_spark_public(
        &state,
        &jwt,
        Verb::ObserveMolecule, // /quota is conceptually an observe
        None,
    )?;

    let now_ms = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX);

    let sub_hash = hash_sub(&jwt.sub);
    let snapshot = state
        .rate_limiter
        .current_state(&sub_hash, now_ms)
        .map_err(|_| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            label: "rate_limit_read_failed",
            request_id: Some(spark.request_id.clone()),
        })?;

    // Effective drain bounds from the tenant's sealed binding — the
    // same map admission resolved against, so the read face can never
    // disagree with the enforcement face.
    let drain = state
        .nucleon_map
        .load()
        .resolve(&jwt.iss, &jwt.sub)
        .map_or_else(crate::nucleon_map::DrainBounds::default, |r| r.drain_bounds);

    let body = QuotaResponse {
        request_id: spark.request_id.clone(),
        limits: QuotaLimits {
            burst_capacity: state.rate_limiter.capacity().floor() as i64,
            leak_per_minute: state.rate_limiter.leak_per_minute(),
            leak_per_hour: state.rate_limiter.leak_per_hour(),
        },
        current: QuotaCurrent {
            bucket_level: snapshot.level,
            bucket_level_floor: snapshot.level.floor().max(0.0) as i64,
        },
        remaining: snapshot.remaining_floor(),
        reset_at: format_reset_iso(snapshot.reset_at_ms),
        drain_bounds: QuotaDrainBounds {
            budget: drain.budget,
            max_depth: drain.max_depth,
            max_molecules: drain.max_molecules,
        },
    };

    let mut response = (
        StatusCode::OK,
        Json(serde_json::to_value(&body).unwrap_or(Value::Null)),
    )
        .into_response();
    apply_rate_limit_headers(response.headers_mut(), &snapshot);
    Ok(response)
}

/// Inject the `X-RateLimit-*` triplet into a response's header map.
/// Shared by [`get_quota`] and the molecule route handlers so the
/// surface is uniform across `/v1/`.
pub fn apply_rate_limit_headers(headers: &mut HeaderMap, snap: &RateState) {
    if let Ok(v) = HeaderValue::from_str(&snap.capacity_floor().to_string()) {
        headers.insert(HEADER_RATE_LIMIT_LIMIT, v);
    }
    if let Ok(v) = HeaderValue::from_str(&snap.remaining_floor().to_string()) {
        headers.insert(HEADER_RATE_LIMIT_REMAINING, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format_reset_iso(snap.reset_at_ms)) {
        headers.insert(HEADER_RATE_LIMIT_RESET, v);
    }
}

/// Build the snapshot for a JWT'd tenant after the admission boundary
/// has fired (so the bucket has been bumped). Used by every /v1/ route
/// to attach the headers without a second JWT decode.
#[must_use]
pub fn snapshot_for_jwt(state: &Arc<AppState>, jwt: &ValidatedJwt) -> Option<RateState> {
    let now_ms = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX);
    let sub_hash = hash_sub(&jwt.sub);
    state.rate_limiter.current_state(&sub_hash, now_ms).ok()
}

/// Format a Unix-ms timestamp as an ISO-8601 second-precision string
/// (`YYYY-MM-DDTHH:MM:SSZ`). Pure, panic-free. Used both in the JSON
/// body and the `X-RateLimit-Reset` header so they always agree.
fn format_reset_iso(ms: i64) -> String {
    use chrono::{DateTime, Utc};
    let secs = ms.div_euclid(1000);
    let nsec = (ms.rem_euclid(1000)) as u32 * 1_000_000;
    match DateTime::<Utc>::from_timestamp(secs, nsec) {
        Some(dt) => dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        None => "1970-01-01T00:00:00Z".to_string(),
    }
}

/// Extract the JWT bearer from the `Authorization` header. Local
/// duplicate of the molecules-module helper kept module-private so the
/// scope-check helper stays in `molecules.rs` (only that module knows
/// the full scope catalog signatures).
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

// Suppress a benign import (kept for symmetry with the molecules
// module which uses the same admission rig pattern).
#[allow(dead_code)]
fn _unused() {
    let _ = http_request_to_spark;
    let _: Option<AdmissionRig> = None;
    let _ = new_request_id;
}

/// Axum middleware that injects the `X-RateLimit-*` triplet on every
/// JWT-bearing response (gap report ae3d workflow §h, task
/// `20260522-2f91`).
///
/// Runs *after* the inner handler so the snapshot reflects the post-
/// admission bucket level (admission consumes a token; the headers
/// then show `Remaining = capacity − level` against the same snapshot
/// the caller paid for).
///
/// The middleware:
///
/// - extracts the `Authorization: Bearer <jwt>` header,
/// - re-validates the JWT through [`JwtVerifier::validate`] — the cost
///   is a signature verify against an in-RAM JWKS, well under a
///   millisecond,
/// - on success, queries `IngressRateLimiter::current_state`
///   (a pure read — see the no-amplification invariant pinned by
///   `current_state_does_not_consume`),
/// - injects the three headers into the response.
///
/// Failure modes (missing header, invalid JWT, IO error reading the
/// bucket) silently drop the headers. The request's authoritative
/// response status is unchanged — we never let a header concern
/// override the route's own outcome. That keeps the middleware
/// strictly additive: removing it cannot break a passing request.
pub async fn rate_limit_headers_layer(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Snapshot the bearer up front — `req` is consumed by `next.run`.
    let token: Option<String> = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
        })
        .map(|t| t.trim().to_owned());

    let mut response = next.run(req).await;

    if let Some(t) = token {
        if let Ok(jwt) = JwtVerifier::validate(&state.jwks.load(), &t, state.posture) {
            if let Some(snap) = snapshot_for_jwt(&state, &jwt) {
                apply_rate_limit_headers(response.headers_mut(), &snap);
            }
        }
    }
    response
}
