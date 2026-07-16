// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /.well-known/cosmon-oauth-clients` — the OAuth client-id
//! reverse-discovery endpoint (delib-20260710-33b7 §C8, `task-20260710-909a`).
//!
//! Operational-class route: outside `/v1/`, **no JWT gate** (the document
//! is public — `client_id` needs integrity, not confidentiality), and
//! excluded from the §8p frozen API surface (same class as `/healthz`,
//! `/install.sh`, `/mcp`). No `cs` CLI verb counterpart — publishing the
//! provisioned `client_id`s is an HTTP-ingress discovery concern.
//!
//! The handler is a thin projection of on-disk state
//! ([`crate::oauth_discovery::load_registry`]); it holds no in-RAM
//! business state, so it stays a pure read of the tenant filesystem like
//! every other adapter surface.
//!
//! **No application-layer throttle (recorded decision, `task-20260710-4364`
//! / review df19 F3).** Being unauthenticated, this route is intentionally
//! outside the §8j clause-(c) leaky bucket (which is keyed on the JWT
//! `sub` and no-ops without one). `DoS` control for the operational class is
//! delegated to the network edge — see `crate::router` and
//! `docs/architectural-invariants.md` §8j. The residual read amplification
//! reaches exact `/healthz` parity once `task-20260710-a575` (F2) drops the
//! `exists()` pre-check in `load_registry`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::error::ApiError;
use crate::oauth_discovery::load_registry;
use crate::AppState;

/// Serve the reverse-discovery document.
///
/// - `200` + the JSON [`crate::oauth_discovery::ClientRegistry`] when a
///   registry is configured (explicit `oauth-clients.toml` or derived
///   from the trusted-issuers allowlist). `Cache-Control: no-store` so a
///   `client_id` rotation (Forgejo re-provision) is seen on the next
///   fetch — the client's rotation-detection (purge + re-login) depends
///   on freshness.
/// - `404 discovery_unconfigured` when nothing is configured — the
///   surface is discoverable-but-inert, never a fabricated document.
/// - `500 discovery_error` on a malformed / bad-schema registry file
///   (fail-closed: a mis-provisioned server refuses rather than serving a
///   document it cannot vouch for).
pub async fn get_oauth_clients(State(state): State<Arc<AppState>>) -> Response {
    match load_registry(&state.state_dir) {
        Ok(Some(doc)) => {
            let mut resp = Json(doc).into_response();
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            resp
        }
        Ok(None) => {
            ApiError::with_status(StatusCode::NOT_FOUND, "discovery_unconfigured").into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "oauth-clients reverse-discovery refused fail-closed");
            ApiError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "discovery_error")
                .into_response()
        }
    }
}
