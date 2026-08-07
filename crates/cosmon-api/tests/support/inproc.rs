// SPDX-License-Identifier: AGPL-3.0-only

//! Drive the `cs-api` router in-process, with no TCP listener.
//!
//! # Why this exists
//!
//! Every test in this crate used to bind a loopback socket and speak to
//! it through `reqwest`, whatever it was actually asserting. The
//! classification in `docs/guides/port-or-wire.md` found that all
//! twenty-seven of them were making claims about *our* code — which
//! route was chosen, what got serialised, which request was refused,
//! what landed on disk — and none about the wire. For such a claim the
//! socket adds no evidence: the bytes on it are produced by hyper,
//! which we neither wrote nor test here. What it does add is a
//! dependency on the environment the test runs in, which is the
//! failure mode `task-20260806-4823` was opened for.
//!
//! So the port is the [`axum::Router`] itself, driven through
//! [`tower::ServiceExt::oneshot`]. The middleware stack, the route
//! table, the extractors, the handlers and the error mapping are all
//! still real — only the listener is gone. `router` is built by the
//! same `cosmon_api::router_with_cors` that `main` calls, so the thing
//! under test is the thing that ships.
//!
//! The single claim this cannot make — that a bound listener served by
//! `axum::serve` actually answers — lives in `tests/wire.rs`, which is
//! the only suite in this crate that binds a port.

#![allow(dead_code)] // Each test target uses a different subset.

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::Router;
use cosmon_api::{router, router_with_cors, AppState, CorsPolicy};
use tower::ServiceExt;

/// A response body is read whole into memory; no route here returns
/// anything remotely near this, and a limit is required by `to_bytes`.
const BODY_LIMIT: usize = 4 * 1024 * 1024;

/// The router under test, callable request by request.
///
/// Cloned per call because `oneshot` consumes the service; `Router` is
/// cheap to clone (it is `Arc`-backed internally).
pub struct Api {
    router: Router,
}

impl Api {
    /// The router `main` builds by default: [`CorsPolicy::Deny`].
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self {
            router: router(state),
        }
    }

    /// The router under an explicit [`CorsPolicy`], matching
    /// `--allow-web-origin`.
    #[must_use]
    pub fn with_cors(state: AppState, cors: CorsPolicy) -> Self {
        Self {
            router: router_with_cors(state, cors),
        }
    }

    /// Send an already-built request. The escape hatch for anything
    /// the typed helpers below do not cover — an `OPTIONS` preflight,
    /// a deliberately odd `Content-Type`.
    pub async fn send(&self, req: Request<Body>) -> Resp {
        let resp = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("the router is infallible; a panic in a handler aborts instead");
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = to_bytes(resp.into_body(), BODY_LIMIT)
            .await
            .expect("read response body")
            .to_vec();
        Resp {
            status,
            headers,
            body,
        }
    }

    /// `GET <uri>`.
    pub async fn get(&self, uri: &str) -> Resp {
        self.send(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .expect("build GET"),
        )
        .await
    }

    /// `GET <uri>` with an `Origin`, as a browser would send it.
    pub async fn get_with_origin(&self, uri: &str, origin: &str) -> Resp {
        self.send(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header(header::ORIGIN, origin)
                .body(Body::empty())
                .expect("build GET"),
        )
        .await
    }

    /// `POST <uri>` with a JSON body.
    pub async fn post_json(&self, uri: &str, body: &serde_json::Value) -> Resp {
        self.post_raw(uri, "application/json", body.to_string())
            .await
    }

    /// `POST <uri>` with no body at all — several routes take none.
    pub async fn post_empty(&self, uri: &str) -> Resp {
        self.send(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .body(Body::empty())
                .expect("build POST"),
        )
        .await
    }

    /// `POST <uri>` with a literal body and content type, for the
    /// cases where the exact bytes or the exact type is the point.
    pub async fn post_raw(&self, uri: &str, content_type: &str, body: impl Into<String>) -> Resp {
        self.send(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body.into()))
                .expect("build POST"),
        )
        .await
    }

    /// `OPTIONS <uri>` carrying an `Origin` — a browser preflight.
    pub async fn preflight(&self, uri: &str, origin: &str) -> Resp {
        self.send(
            Request::builder()
                .method(Method::OPTIONS)
                .uri(uri)
                .header(header::ORIGIN, origin)
                .body(Body::empty())
                .expect("build OPTIONS"),
        )
        .await
    }
}

/// A response, read whole.
///
/// Status, headers and body are all kept: a CORS assertion needs the
/// headers, a refusal assertion needs the status, and most handlers
/// are judged on their JSON. Nothing here asserts on the caller's
/// behalf — a helper that quietly swallowed a non-2xx status is
/// exactly how a converted test loses its teeth.
pub struct Resp {
    /// HTTP status the router produced.
    pub status: StatusCode,
    /// Response headers, including anything the CORS middleware wrote.
    pub headers: HeaderMap,
    /// Response body bytes.
    pub body: Vec<u8>,
}

impl Resp {
    /// Parse the body as JSON, reporting the raw body on failure so a
    /// broken response reads as a broken response and not as a parse
    /// error with no context.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "body is not JSON ({e}); status {}, body: {}",
                self.status,
                self.text()
            )
        })
    }

    /// The body as UTF-8, lossily — used in failure messages.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Assert a 2xx and return `self`, quoting the body when it is not.
    ///
    /// The equivalent of `reqwest`'s `error_for_status`, kept explicit
    /// at each call site rather than folded into the request helpers.
    ///
    /// Deliberately **not** `#[must_use]`: the assertion *is* the point, so
    /// `resp.ok();` on its own line is the normal call and not a mistake. The
    /// pure accessors above keep the attribute, where discarding the value
    /// really would mean the call did nothing.
    pub fn ok(self) -> Self {
        assert!(
            self.status.is_success(),
            "expected 2xx, got {}: {}",
            self.status,
            self.text()
        );
        self
    }

    /// One header value as a string, or `None` when absent.
    #[must_use]
    pub fn header(&self, name: HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Whether a header is present at all — for the CORS assertions,
    /// where absence is the property.
    #[must_use]
    pub fn has_header(&self, name: HeaderName) -> bool {
        self.headers.contains_key(name)
    }
}

/// A `HeaderValue` from a `&str`, panicking on an illegal one. Test
/// inputs are literals; a failure here is a typo, not a condition.
#[must_use]
pub fn hv(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).expect("test header value is legal")
}
