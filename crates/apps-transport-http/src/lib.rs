// SPDX-License-Identifier: Apache-2.0

//! `apps-transport-http` — HTTP-on-Tailscale transport primitives.
//!
//! Decision (2026-04-26 operator): HTTP-over-Tailscale is the standard
//! transport for native apps in the local cluster of galaxies (Verdict,
//! Mur du Matin, Cosmon, future). Tailscale is the trust boundary
//! (Noogram-compliant); HTTP is the wire (axum on Rust, URLSession+Codable
//! on Swift). This crate is the canonical Rust foundation: every cluster
//! daemon should bind through [`serve_http_on_tailscale`] so binding
//! semantics, error mapping, access logs and URL versioning stay the same
//! across galaxies.
//!
//! ## Surface
//!
//! - [`TailscaleBind`] — choose a deterministic bind address: auto-discover
//!   via `tailscale ip --4`, fall back to `COCKPIT_HTTP_BIND` env, or use
//!   an explicit address. **Never `0.0.0.0`** — that would defeat the
//!   trust boundary.
//! - [`serve_http_on_tailscale`] — bind a [`axum::Router`] on the resolved
//!   address and serve until shutdown.
//! - [`access_log_layer`] — tracing middleware emitting one structured line
//!   per request (`request_id`, `method`, `path`, `status`, `latency_ms`,
//!   `peer_ip`, `peer_hostname`).
//! - [`ApplicationError`] + [`error_response`] — canonical error type with
//!   400/404/409/422/500 mappings and a stable JSON shape
//!   `{"error", "code", "detail"}`.
//! - [`v1`] — URL versioning helper (`/v1/<resource>`).
//!
//! ## Out of scope (other molecules)
//!
//! - Migrating existing apps (Verdict/Mur du Matin) onto this crate.
//! - HMAC or pinned-cert auth — Tailscale is the trust boundary in v0.1.
//! - Push channel (Server-Sent Events). See `analysis.md` §1.5 for the
//!   v1.1 sketch.

#![forbid(unsafe_code)]

pub mod bind;
pub mod error;
pub mod middleware;
pub mod routing;

pub use bind::{resolve_bind, BindError, BindOutcome, TailscaleBind};
pub use error::{ApplicationError, ErrorBody};
pub use middleware::{access_log_layer, request_id_layer, RequestId, REQUEST_ID_HEADER};
pub use routing::v1;

use std::net::SocketAddr;

use axum::Router;
use tracing::info;

/// Bind a router on the resolved Tailscale address and serve until the
/// future returned by `shutdown` resolves.
///
/// The address is resolved by [`resolve_bind`] from `bind`. Bind failures,
/// including `0.0.0.0` rejection, propagate as [`BindError`] wrapped in
/// `anyhow::Error`.
///
/// # Errors
///
/// Returns an error if address resolution fails (see [`BindError`]),
/// if the TCP listener cannot be opened, or if axum's `serve` returns
/// a runtime error.
///
/// # Example
///
/// ```no_run
/// # async fn ex() -> anyhow::Result<()> {
/// use apps_transport_http::{serve_http_on_tailscale, TailscaleBind};
/// use axum::{routing::get, Router};
///
/// let app = Router::new().route("/v1/health", get(|| async { "ok" }));
/// serve_http_on_tailscale(
///     TailscaleBind::auto_with_port(8789),
///     app,
///     std::future::pending::<()>(),
/// )
/// .await?;
/// # Ok(())
/// # }
/// ```
pub async fn serve_http_on_tailscale<F>(
    bind: TailscaleBind,
    router: Router,
    shutdown: F,
) -> anyhow::Result<BindOutcome>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let outcome = resolve_bind(&bind)?;
    let addr: SocketAddr = outcome.addr;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind {addr}: {e}"))?;
    info!(
        bind = %addr,
        source = outcome.source.as_str(),
        hostname = outcome.hostname.as_deref().unwrap_or(""),
        "apps-transport-http: listening"
    );
    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e}"))?;
    Ok(outcome)
}
