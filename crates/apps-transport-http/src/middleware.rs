// SPDX-License-Identifier: Apache-2.0

//! Tracing/access-log middleware.
//!
//! [`request_id_layer`] tags every request with a fresh `request_id`
//! header (UUID-v4 surrogate; we use a 64-bit random hex to avoid pulling
//! the `uuid` crate into the dependency tree of every cluster daemon).
//! [`access_log_layer`] emits one structured `tracing::info!` line per
//! request:
//!
//! ```text
//! request_id=… method=GET path=/v1/health status=200 latency_ms=2 peer_ip=192.0.2.10 peer_hostname=host.example
//! ```
//!
//! No external dependencies on `tower-http`. The layer is hand-rolled
//! with `axum::middleware::from_fn` so the crate stays small and the
//! semantics are obvious to read.

// clippy 1.89 strengthened `similar_names` to flag the conventional axum
// `req` / `res` binding pair used by both layers; the names are idiomatic and
// intentional. File-level allow (toolchain drift, unrelated to
// task-20260617-4847's release-membrane work).
#![allow(clippy::similar_names)]

use std::time::Instant;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;

use std::net::SocketAddr;

/// Header used to propagate the request id across the request/response.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Newtype that handlers can extract via `Extension<RequestId>` to embed
/// the same id in their own logs.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// Generate a new request id (16-hex-char random token).
fn fresh_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    #[allow(clippy::cast_possible_truncation)]
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    let mix = nanos
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(pid)
        .wrapping_add(1_442_695_040_888_963_407);
    format!("{mix:016x}")
}

/// Middleware: ensure every request carries an `x-request-id` header,
/// reuse the inbound one if the client supplied a non-empty value.
pub async fn request_id_layer(mut req: Request<Body>, next: Next) -> Response {
    let id = req
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map_or_else(fresh_request_id, str::to_owned);
    req.extensions_mut().insert(RequestId(id.clone()));
    if let Ok(hv) = HeaderValue::from_str(&id) {
        req.headers_mut().insert(&REQUEST_ID_HEADER, hv.clone());
    }
    let mut res = next.run(req).await;
    if let Ok(hv) = HeaderValue::from_str(&id) {
        res.headers_mut().insert(&REQUEST_ID_HEADER, hv);
    }
    res
}

/// Middleware: emit one structured log line per request. Includes peer
/// IP if axum exposes a [`ConnectInfo<SocketAddr>`] extension; falls
/// back to `unknown` otherwise.
pub async fn access_log_layer(req: Request<Body>, next: Next) -> Response {
    let started = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map_or_else(|| "-".to_string(), |r| r.0.clone());
    let peer_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(
            || "unknown".to_string(),
            |ConnectInfo(a)| a.ip().to_string(),
        );

    let res = next.run(req).await;

    let latency_ms = started.elapsed().as_millis();
    let status = res.status().as_u16();
    let level = if status >= 500 {
        tracing::Level::ERROR
    } else if status >= 400 {
        tracing::Level::WARN
    } else {
        tracing::Level::INFO
    };
    match level {
        tracing::Level::ERROR => tracing::error!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status,
            latency_ms = %latency_ms,
            peer_ip = %peer_ip,
            "http access (error)"
        ),
        tracing::Level::WARN => tracing::warn!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status,
            latency_ms = %latency_ms,
            peer_ip = %peer_ip,
            "http access (4xx)"
        ),
        _ => tracing::info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status,
            latency_ms = %latency_ms,
            peer_ip = %peer_ip,
            "http access"
        ),
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_request_id_is_16_hex_chars() {
        let id = fresh_request_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fresh_request_id_changes_per_call() {
        let a = fresh_request_id();
        std::thread::sleep(std::time::Duration::from_nanos(1));
        let b = fresh_request_id();
        assert_ne!(a, b);
    }
}
