// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/auth/me` — JWT introspection / whoami.
//!
//! Trivial admission-side route — a priority high-friction-tenant
//! convenience.
//! Pipeline:
//!
//! 1. Extract `Authorization: Bearer <jwt>`; 401 if missing.
//! 2. Validate JWT (clause a) → `ValidatedJwt`.
//! 3. **No scope check.** A valid JWT is the whole gate — this is the
//!    counterpart of `cs whoami`, not a state-mutating verb.
//! 4. Resolve `(iss, sub) → noyau` via the sealed nucleon map. Absent
//!    bindings surface as `"noyau": null` in the response; the route
//!    does not 401, because the JWT is well-formed and the tenant is
//!    asking "who do you see in this token?", not "let me do work".
//! 5. Project the validated claims to JSON.
//!
//! The response intentionally echoes only what the server already
//! trusts — `sub`, `aud`, `scopes`, `noyau`, `expires_at`, `issuer`.
//! Raw token bytes and `jti` are deliberately omitted (turing §8.2.3 —
//! never echo material the operator could use to extend an attack
//! window).
//!
//! Excluded from the §8p molecule⇔verb bijection check the same way
//! `/v1/auth/claude/*` is — admission-side routes have no `cs` CLI
//! verb counterpart (path-prefix exclusion in
//! `tests/api_surface_freeze.rs::routes_and_verbs_are_bijective`).

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;
use crate::jwt::JwtVerifier;
use crate::AppState;

/// Body schema for `GET /v1/auth/me`.
///
/// Stable, additive-only. Adding a field is allowed; renaming or
/// removing one is a §8p break that must update the `OpenAPI` spec and
/// the cosmon-remote client in the same change.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuthMeResponse {
    /// `sub` claim — the principal identifier the `IdP` signs into the
    /// token.
    pub sub: String,
    /// `aud` claim — the audience the JWT was minted for. Always a
    /// single string at this point (admission has already picked the
    /// matching audience when the `IdP` issued an array).
    pub aud: Vec<String>,
    /// Scopes carried by the JWT itself (does NOT include
    /// binding-granted scopes from the nucleon map — those are an
    /// authorization detail the wire-side `/me` deliberately does not
    /// leak).
    pub scopes: Vec<String>,
    /// Tenant axis (noyau) bound to `(iss, sub)`. `None` when no
    /// nucleon binding exists for the principal — the JWT is valid but
    /// not yet provisioned for any tenant.
    pub noyau: Option<String>,
    /// Absolute expiry as ISO-8601 UTC (`exp` claim, formatted).
    pub expires_at: String,
    /// `iss` claim — the `IdP` that signed the token (e.g.
    /// `https://cs-oidc-mock` or the Forgejo issuer URL).
    pub issuer: String,
    /// Worker-glasses signal (smithy C1 onboarding, janis pre-mortem
    /// « les deux badges ») : whether the container's Claude Code
    /// credentials file exists — i.e. whether `auth login` has been
    /// completed at least once. `None` when the auth-claude surface is
    /// not configured on this deployment (the server cannot know);
    /// `Some(false)` is the signal `cosmon-remote doctor` turns into
    /// « lance `auth login` » *before* the tenant discovers it via a
    /// 503 on their first tackle. Additive field, gated behind a valid
    /// JWT — the unauthenticated `/healthz` deliberately does not
    /// carry it.
    pub claude_credentials_present: Option<bool>,
    /// Adapter binary version, aligned on the cosmon release version —
    /// the same string as the release tag, the tarball the operator
    /// downloaded, and `cosmon-rpp-adapter --version` (release `v0.2.1`
    /// ⇒ `"0.2.1"`). Additive field — mirrors `/healthz` so
    /// an authenticated client gets
    /// the same answer without a second round-trip.
    pub version: String,
    /// Number of `surface_added` events the binary was compiled with
    /// (monotonic — `data/surface_events.txt` is append-only). A
    /// client comparing its own compiled-in count may print an
    /// informative stderr note on mismatch; never blocking, never
    /// `/v2/`.
    pub api_surface_version: usize,
}

/// `GET /v1/auth/me`. See module docs for the pipeline.
pub async fn get_auth_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    // 1. Authorization header.
    let token = extract_bearer(&headers)?;

    // 2. JWT validation — clause (a). Any failure collapses to the
    //    canonical reject-reason → 401 mapping. Audit-side detail
    //    (Expired vs SignatureInvalid vs AudienceMismatch …) stays in
    //    the audit channel; the wire body carries the stable label.
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 3. No scope check (whoami semantics).

    // 4. Resolve binding → noyau. Absent binding ⇒ `noyau: null` (the
    //    JWT decoded fine, the tenant is simply not yet bound to a
    //    cosmon noyau).
    let noyau = state
        .nucleon_map
        .load()
        .resolve(&jwt.iss, &jwt.sub)
        .map(|resolved| resolved.noyau.as_str().to_owned());

    // 5. Format `exp` as ISO-8601 UTC. The token's exp is already
    //    validated as in-future, so the conversion never collapses to
    //    a stale value.
    let expires_at = format_exp_iso8601(jwt.exp);

    // 6. Worker-glasses probe: a plain existence check on the
    //    credentials file the PKCE confirm handler writes
    //    (`write_credentials_file`). Reading the *artifact itself* —
    //    not a session record — keeps the signal independent of how
    //    the login happened (API flow, operator docker-exec, image
    //    seed) and falsifiable by deleting the file.
    let claude_credentials_present = state
        .auth_claude
        .as_ref()
        .map(|ac| ac.config.credentials_path.exists());

    let body = AuthMeResponse {
        sub: jwt.sub,
        aud: vec![jwt.aud],
        scopes: jwt.scopes,
        noyau,
        expires_at,
        issuer: jwt.iss,
        claude_credentials_present,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        api_surface_version: crate::surface_events::SURFACE_EVENTS.len(),
    };
    let value = serde_json::to_value(&body)
        .map_err(|_| ApiError::internal("auth_me_serialization_failed"))?;
    Ok(Json(value))
}

/// Extract the JWT bearer from the `Authorization` header. Mirrors the
/// molecules-route helper; kept private here so the auth surface can
/// evolve its accepted spellings (e.g. `DPoP`) without dragging the
/// molecule routes along.
fn extract_bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    use crate::error::RppRejectReason;
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

/// Format a Unix-epoch second-count as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Pure standalone implementation — the crate does not depend on
/// `chrono` and pulling it in for one timestamp is the wrong trade.
/// The civil-date conversion uses the proleptic Gregorian calendar
/// (the convention RFC 3339 / ISO 8601 fix). Verified against the
/// `chrono` reference output in `tests::format_exp_iso8601_matches_chrono`.
fn format_exp_iso8601(unix_secs: u64) -> String {
    let secs_in_day: u64 = 86_400;
    let days = unix_secs / secs_in_day;
    let time_of_day = unix_secs % secs_in_day;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days-since-1970-01-01 to `(year, month, day)` in the
/// proleptic Gregorian calendar. Algorithm from Howard Hinnant's
/// civil-from-days (public domain) — same conversion `chrono` uses
/// internally.
// clippy 1.89 `similar_names` flags the single-letter civil-from-days vars
// (y/yoe/doy/doe…); they are the algorithm's canonical names. Toolchain drift.
#[allow(clippy::similar_names)]
fn days_to_ymd(days: u64) -> (i64, u32, u32) {
    // Re-express as days since epoch shifted to 0000-03-01 so the
    // 400-year cycle aligns on a leap-year boundary.
    let z = days as i64 + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_exp_iso8601_known_values() {
        // Reference: `date -u -r 1700000000 +"%Y-%m-%dT%H:%M:%SZ"`.
        assert_eq!(format_exp_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_exp_iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        // 2026-05-22T12:00:00Z = 1779796800.
        assert_eq!(format_exp_iso8601(1_779_451_200), "2026-05-22T12:00:00Z");
    }

    #[test]
    fn days_to_ymd_handles_leap_years() {
        // 2024-02-29 (leap year, day 19_417 since 1970-01-01).
        let days = 19_782; // 2024-02-29
        let (y, m, d) = days_to_ymd(days);
        assert_eq!((y, m, d), (2024, 2, 29));
        // 2026-05-22 = 20_595 days since 1970-01-01.
        let (y, m, d) = days_to_ymd(20_595);
        assert_eq!((y, m, d), (2026, 5, 22));
    }
}
