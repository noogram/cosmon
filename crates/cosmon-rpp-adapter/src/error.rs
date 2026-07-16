// SPDX-License-Identifier: AGPL-3.0-only

//! Typed reject reasons and HTTP error responses.
//!
//! [`RppRejectReason`] mirrors the §3.6 enum from ADR-080 — its
//! ordering documents the §8j HTTPS+JWT clause sequence. [`ApiError`]
//! is the wire-level error: it converts every rejection into an HTTP
//! status + JSON body without leaking tenant identity (turing G9).
//!
//! # Anonymity discipline
//!
//! The HTTP body never contains:
//!
//! - the rejected JWT `sub` (or its hash),
//! - the rejected JWT `jti`,
//! - the resolved (or attempted-resolved) `nucleon_id`,
//! - the `noyau` of a cross-tenant pivot.
//!
//! These details land in the audit log only. The wire body carries
//! the human-readable status reason and the `request_id` so the
//! operator can cross-reference logs.

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::nucleon_map::Noyau;

/// Typed reject reasons — one variant per §8j HTTPS+JWT clause path.
///
/// The taxonomy is canonical for the RPP. Future ingress ports
/// (gRPC, QUIC) re-instantiate §8j with their *own* substrate-specific
/// extensions; they do **not** add reasons to this enum.
#[derive(Clone, Debug, thiserror::Error)]
pub enum RppRejectReason {
    // ---- Shape (clause b — every payload must materialise) ----
    /// Body did not parse as JSON or violated the route schema.
    #[error("invalid json body")]
    InvalidJsonBody,
    /// The route path resolved to no known cosmon verb.
    #[error("unknown verb")]
    UnknownVerb,
    /// The request maps to a verb that is operator-only (§5).
    #[error("operator-only verb")]
    OperatorOnlyVerb(&'static str),

    // ---- Identity (clause a) ----
    /// `Authorization` header absent.
    #[error("missing authorization")]
    MissingAuthorization,
    /// `Authorization` header present but did not parse as a JWT.
    #[error("malformed jwt")]
    MalformedJwt,
    /// Algorithm not in the RS256 / ES256 whitelist.
    #[error("unsupported alg")]
    UnsupportedAlg(String),
    /// Cryptographic verification failed.
    #[error("signature invalid")]
    SignatureInvalid,
    /// Token past its `exp`.
    #[error("expired")]
    Expired,
    /// Token before its `nbf`.
    #[error("not yet valid")]
    NotYetValid,
    /// `aud` did not match the pinned audience for this RPP instance.
    #[error("audience mismatch")]
    AudienceMismatch,
    /// The token carried multiple audiences and **more than one** of
    /// them is pinned for this issuer. Fail-closed: an ambiguous
    /// audience leaves the `(iss, sub, aud)` isolation key under the
    /// token minter's control (pick-the-first would let ordering choose
    /// which pinned audience the RS resolves to). Reject rather than
    /// silently select. (Finding Rp F3, delib-20260710-33b7.)
    #[error("ambiguous audience")]
    AmbiguousAudience,
    /// `iss` not in the boot-time pinned issuers.
    #[error("issuer not pinned")]
    IssuerNotPinned,
    /// `sub` did not resolve to any sealed `nucleon_id`.
    #[error("unknown sub")]
    UnknownSub,
    /// The `oidc-identity.toml` BLAKE3 seal diverged from the on-disk
    /// content — retroactive edit detected.
    #[error("seal broken")]
    SealBroken,
    /// JWT `sub` resolved to a `nucleon_id` outside the request's
    /// tenant routing.
    #[error("cross-tenant pivot")]
    CrossTenantPivot {
        /// Expected `noyau` per the request routing.
        expected_noyau: Noyau,
        /// Resolved `noyau` from the sealed mapping.
        found_noyau: Noyau,
    },

    // ---- Rate (clause c) ----
    /// Per-`sub` leaky bucket exceeded.
    #[error("rate limited")]
    RateLimited {
        /// Earliest moment a new admission would succeed.
        retry_after: Duration,
    },
    /// Per-`noyau` global budget exhausted.
    #[error("noyau budget exhausted")]
    NoyauBudgetExhausted(Noyau),

    // ---- Drain bounds (clause c extension — B1/B2/B3 moussage,
    //      task-20260610-e5f6, godel Q3). The bounds live in the
    //      binding (operator-written, never client-writable); the
    //      refusal codes are stable so the client *learns* the bound
    //      by the documented failure without being able to lift it. ----
    /// B3 — the binding's drain budget is exhausted (429
    /// `budget_exhausted`). Mirrors `cs run` exit code 90.
    #[error("drain budget exhausted")]
    DrainBudgetExhausted,
    /// B1 — the DAG is deeper than the binding allows (409
    /// `max_depth_exceeded`). Mirrors `cs run` exit code 92.
    #[error("drain max depth exceeded")]
    DrainMaxDepthExceeded,
    /// B2 — the fleet is wider than the binding allows (429
    /// `molecule_quota_exceeded`). Mirrors `cs run` exit code 91.
    #[error("drain molecule quota exceeded")]
    DrainMoleculeQuotaExceeded,

    // ---- Kill-switch (clauses a + c — operator override) ----
    /// `sub` is in the deny-list.
    #[error("sub revoked")]
    SubKilled,
    /// `jti` is in the deny-list.
    #[error("jti revoked")]
    JtiKilled,
    /// `noyau` is in the deny-list.
    #[error("noyau paused")]
    NoyauKilled(Noyau),
    /// Global blast-door is closed.
    #[error("global kill switch engaged")]
    GlobalKill,

    // ---- Topology (clause d) ----
    /// Bidirectional design forbidden in V0/V1.
    #[error("bidirectional forbidden")]
    BidirectionalForbidden,

    // ---- Subprocess envelope (clause e) ----
    /// `cs` could not be spawned.
    #[error("subprocess spawn failed: {0}")]
    SubprocessSpawnFailed(String),
    /// `cs` exceeded the configured timeout.
    #[error("subprocess timeout after {0:?}")]
    SubprocessTimeout(Duration),
    /// `cs` exited non-zero.
    #[error("subprocess exit non-zero (code={code})")]
    SubprocessExitNonZero {
        /// Process exit code as reported by the OS.
        code: i32,
        /// Short stderr excerpt for operator logs (never echoed in
        /// wire response).
        stderr_excerpt: String,
    },

    // ---- Substrate (clause b) ----
    /// Materialisation of the inbox file failed.
    #[error("inbox materialisation failed: {0}")]
    InboxMaterializationFailed(String),
}

impl RppRejectReason {
    /// Stable machine-readable label, suitable for NDJSON / metrics.
    /// Kept in sync with `tests/admission_test.rs`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::InvalidJsonBody => "invalid_json_body",
            Self::UnknownVerb => "unknown_verb",
            Self::OperatorOnlyVerb(_) => "operator_only_verb",
            Self::MissingAuthorization => "missing_authorization",
            Self::MalformedJwt => "malformed_jwt",
            Self::UnsupportedAlg(_) => "unsupported_alg",
            Self::SignatureInvalid => "signature_invalid",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::AudienceMismatch => "audience_mismatch",
            Self::AmbiguousAudience => "ambiguous_audience",
            Self::IssuerNotPinned => "issuer_not_pinned",
            Self::UnknownSub => "unknown_sub",
            Self::SealBroken => "seal_broken",
            Self::CrossTenantPivot { .. } => "cross_tenant_pivot",
            Self::RateLimited { .. } => "rate_limited",
            Self::NoyauBudgetExhausted(_) => "noyau_budget_exhausted",
            Self::DrainBudgetExhausted => "budget_exhausted",
            Self::DrainMaxDepthExceeded => "max_depth_exceeded",
            Self::DrainMoleculeQuotaExceeded => "molecule_quota_exceeded",
            Self::SubKilled => "sub_killed",
            Self::JtiKilled => "jti_killed",
            Self::NoyauKilled(_) => "noyau_killed",
            Self::GlobalKill => "global_kill",
            Self::BidirectionalForbidden => "bidirectional_forbidden",
            Self::SubprocessSpawnFailed(_) => "subprocess_spawn_failed",
            Self::SubprocessTimeout(_) => "subprocess_timeout",
            Self::SubprocessExitNonZero { .. } => "subprocess_exit_non_zero",
            Self::InboxMaterializationFailed(_) => "inbox_materialization_failed",
        }
    }

    /// HTTP status this rejection maps to. Cross-tenant pivot,
    /// unknown-sub, and seal-broken intentionally collapse to 401
    /// (or 404 for the molecule lookup) so that no oracle leaks
    /// existence (turing §8.2.3, ADR-080 §10.1).
    #[must_use]
    pub fn http_status(&self) -> StatusCode {
        match self {
            Self::InvalidJsonBody | Self::UnknownVerb => StatusCode::BAD_REQUEST,
            Self::OperatorOnlyVerb(_) => StatusCode::FORBIDDEN,
            Self::MissingAuthorization
            | Self::MalformedJwt
            | Self::UnsupportedAlg(_)
            | Self::SignatureInvalid
            | Self::Expired
            | Self::NotYetValid
            | Self::AudienceMismatch
            | Self::AmbiguousAudience
            | Self::IssuerNotPinned
            | Self::UnknownSub
            | Self::SealBroken
            | Self::CrossTenantPivot { .. }
            | Self::SubKilled
            | Self::JtiKilled => StatusCode::UNAUTHORIZED,
            Self::NoyauKilled(_) | Self::GlobalKill => StatusCode::SERVICE_UNAVAILABLE,
            Self::RateLimited { .. }
            | Self::NoyauBudgetExhausted(_)
            | Self::DrainBudgetExhausted
            | Self::DrainMoleculeQuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
            Self::DrainMaxDepthExceeded => StatusCode::CONFLICT,
            Self::BidirectionalForbidden => StatusCode::METHOD_NOT_ALLOWED,
            Self::SubprocessSpawnFailed(_)
            | Self::SubprocessExitNonZero { .. }
            | Self::InboxMaterializationFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::SubprocessTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
        }
    }
}

/// Wire-level error: status + JSON body. Constructed from a
/// [`RppRejectReason`] (which carries the audit-side detail) and an
/// optional `request_id` so the operator can cross-reference logs.
#[derive(Debug)]
pub struct ApiError {
    /// HTTP status emitted to the client.
    pub status: StatusCode,
    /// Stable label for the reject reason — copies into JSON body.
    pub label: &'static str,
    /// Optional `request_id` (clause (b) — the inbox file name).
    pub request_id: Option<String>,
}

impl ApiError {
    /// Lift a [`RppRejectReason`] into an [`ApiError`] for HTTP
    /// emission. The detailed reason stays in the audit channel; the
    /// client sees only the stable label and the `request_id`.
    #[must_use]
    pub fn from_reject(reason: &RppRejectReason, request_id: Option<String>) -> Self {
        Self {
            status: reason.http_status(),
            label: reason.label(),
            request_id,
        }
    }

    /// Internal-server-error helper for non-reject failures (e.g.
    /// JSON serialisation in the response path).
    #[must_use]
    pub fn internal(label: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            label,
            request_id: None,
        }
    }

    /// Construct an error with an explicit status + stable label, for
    /// surfaces whose failure taxonomy is NOT a §8j admission
    /// [`RppRejectReason`] (e.g. the operator-sealed admin provisioning
    /// routes). The label is the only string the
    /// client sees; never embed a secret or tenant identity in it.
    #[must_use]
    pub fn with_status(status: StatusCode, label: &'static str) -> Self {
        Self {
            status,
            label,
            request_id: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.request_id {
            Some(id) => json!({"error": self.label, "request_id": id}),
            None => json!({"error": self.label}),
        };
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_is_stable() {
        assert_eq!(RppRejectReason::Expired.label(), "expired");
        assert_eq!(
            RppRejectReason::OperatorOnlyVerb("done").label(),
            "operator_only_verb"
        );
        assert_eq!(
            RppRejectReason::SubprocessTimeout(Duration::from_secs(30)).label(),
            "subprocess_timeout",
        );
    }

    #[test]
    fn unknown_sub_collapses_to_401() {
        // Critical: 401 not 403 — no oracle on existence (turing).
        assert_eq!(
            RppRejectReason::UnknownSub.http_status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn drain_bound_codes_are_stable() {
        // The stable alphabet of the moussage refusals
        // (task-20260610-e5f6): the client learns the bound by the
        // documented failure — label and status are wire contract.
        assert_eq!(
            RppRejectReason::DrainBudgetExhausted.label(),
            "budget_exhausted"
        );
        assert_eq!(
            RppRejectReason::DrainBudgetExhausted.http_status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            RppRejectReason::DrainMaxDepthExceeded.label(),
            "max_depth_exceeded"
        );
        assert_eq!(
            RppRejectReason::DrainMaxDepthExceeded.http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            RppRejectReason::DrainMoleculeQuotaExceeded.label(),
            "molecule_quota_exceeded"
        );
        assert_eq!(
            RppRejectReason::DrainMoleculeQuotaExceeded.http_status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn rate_limit_is_429() {
        assert_eq!(
            RppRejectReason::RateLimited {
                retry_after: Duration::from_secs(5)
            }
            .http_status(),
            StatusCode::TOO_MANY_REQUESTS,
        );
    }

    #[test]
    fn body_does_not_leak_sub_or_nucleon() {
        let err = ApiError::from_reject(&RppRejectReason::UnknownSub, Some("req-abc".into()));
        let resp = err.into_response();
        // The label is the only error string on the wire — sub/jti/nucleon
        // never appear.
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
