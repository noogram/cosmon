// SPDX-License-Identifier: AGPL-3.0-only

//! What a browser can get this daemon to do, asserted on the router.
//!
//! These tests exist because the failure they guard is silent: a
//! wildcard `Access-Control-Allow-Origin` on a daemon whose
//! `POST /molecules/{id}/tackle` spawns a worker means any page the
//! operator opens can drive it, and nothing in the daemon's own
//! behaviour looks wrong while that is true.
//!
//! # Why there is no socket here any more
//!
//! Cross-origin refusal *sounds* like a wire property, and if this
//! crate mounted `tower_http::cors::CorsLayer` it would be one —
//! testing it would mean testing somebody else's middleware, and the
//! socket would be the only honest way to do it.
//!
//! It does not. `cosmon_api::cors::layer` is a hand-written middleware
//! in this repository, and the property that actually matters is a
//! statement about its control flow: the refusal is returned *before*
//! `next.run` is awaited, so the handler — and with it the `cs`
//! subprocess — is never entered. A `oneshot` through the same router
//! `main` builds observes that ordering exactly as well as a loopback
//! connection does, and observes it without depending on a free
//! ephemeral port or on the ambient environment. See
//! `docs/guides/port-or-wire.md` for the rule and the full
//! classification.
//!
//! The one claim that does need a listener — that `axum::serve` on a
//! bound port answers at all, and refuses a cross-origin write there
//! too — lives in `tests/wire.rs`.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use cosmon_api::{AppState, CorsPolicy};

#[path = "support/inproc.rs"]
mod inproc;

use inproc::Api;

/// Any path is fine — the daemon never executes it here; only the
/// header behaviour is under test.
fn dummy_cs_path() -> PathBuf {
    PathBuf::from("/nonexistent/cs")
}

fn allowing(origin: &str) -> CorsPolicy {
    CorsPolicy::from_allowed_origins([origin]).expect("policy")
}

/// A stand-in `cs` binary that records the fact it ran, and nothing
/// else. Its existence on disk is the observable we assert on: the
/// response header is the property next door, the side effect is the
/// property under test.
fn tattletale_cs(dir: &Path, marker: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("cs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\necho '{{}}'\n",
            marker.to_string_lossy()
        ),
    )
    .expect("write stand-in cs");
    let mut perms = std::fs::metadata(&script)
        .expect("stat stand-in cs")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod stand-in cs");
    script
}

/// Post to the worker-spawning route the way a hostile page can without
/// a preflight: a *simple* request. `text/plain` and no custom header
/// is precisely the combination a browser sends first and judges after.
async fn simple_post_tackle(api: &Api, origin: Option<&str>) -> StatusCode {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/molecules/task-20260727-0001/tackle")
        .header(header::CONTENT_TYPE, "text/plain");
    if let Some(origin) = origin {
        req = req.header(header::ORIGIN, origin);
    }
    api.send(req.body(Body::empty()).expect("build POST"))
        .await
        .status
}

/// The finding this whole module now exists for: withholding
/// `Access-Control-Allow-Origin` from an unlisted origin does not stop
/// the write, because the write already happened. Asserted on the side
/// effect — a `cs` subprocess that leaves a file behind — not on the
/// header.
///
/// The second half is the control. Without it the first half would
/// stay green if `/molecules/{id}/tackle` had simply stopped spawning
/// anything at all, which is the failure mode this test is meant to be
/// immune to.
///
/// Red proof (2026-08-06): with the `claims_an_origin && matched
/// .is_none()` early return deleted from `cors::layer`, this fails on
/// its first assertion — "the handler ran: an unlisted origin spawned
/// a `cs` subprocess" — exactly as it did over a socket.
#[tokio::test]
async fn a_cross_origin_simple_post_never_reaches_the_worker_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("the-worker-was-spawned");
    let ndjson = dir.path().join("engine-calls.ndjson");
    let state =
        AppState::new(tattletale_cs(dir.path(), &marker)).with_instrumentation_path(ndjson.clone());
    let api = Api::with_cors(state, allowing("http://localhost:5173"));

    let status = simple_post_tackle(&api, Some("https://evil.test")).await;
    // The side effect first, deliberately. If the guard regresses, the
    // failure this test reports should be "a subprocess ran", not "the
    // status was 200" — the status is the property next door.
    assert!(
        !marker.exists(),
        "the handler ran: an unlisted origin spawned a `cs` subprocess"
    );
    assert!(
        !ndjson.exists(),
        "the handler ran: an engine call was recorded for an unlisted origin"
    );
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an unlisted origin must be refused, not served"
    );

    // Control: the same request without an `Origin` — a native client —
    // does reach the handler, so the assertions above have teeth.
    let status = simple_post_tackle(&api, None).await;
    assert!(
        status.is_success(),
        "a native client (no Origin) must still be served, got {status}"
    );
    assert!(
        marker.exists(),
        "control failed: this route no longer spawns anything, so the \
         cross-origin assertion above proves nothing"
    );
}

/// The same refusal under the default policy, where there is no
/// allow-list to miss. This is the configuration every operator runs,
/// and the one that previously had no middleware in the stack at all.
///
/// Red proof (2026-08-06): with the early return deleted, fails on
/// "under Deny the handler still ran for a cross-origin write".
#[tokio::test]
async fn under_deny_a_cross_origin_simple_post_never_reaches_the_worker_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("the-worker-was-spawned");
    let state = AppState::new(tattletale_cs(dir.path(), &marker));
    let api = Api::with_cors(state, CorsPolicy::Deny);

    let status = simple_post_tackle(&api, Some("https://evil.test")).await;
    assert!(
        !marker.exists(),
        "under Deny the handler still ran for a cross-origin write"
    );
    assert_eq!(status, StatusCode::FORBIDDEN);

    let status = simple_post_tackle(&api, None).await;
    assert!(status.is_success(), "got {status}");
    assert!(marker.exists(), "control failed: the route spawns nothing");
}

/// `router` — the constructor `main` uses by default and that every
/// test uses — must emit nothing a browser can act on.
///
/// Red proof (2026-08-06): making `matching_origin` return the request's
/// own `Origin` under `Deny` turns this red on "default policy must not
/// grant any browser origin".
#[tokio::test]
async fn default_router_emits_no_allow_origin() {
    let api = Api::new(AppState::new(dummy_cs_path()));
    let resp = api.get_with_origin("/healthz", "https://evil.test").await;
    assert!(
        !resp.has_header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "default policy must not grant any browser origin"
    );
}

/// Red proof (2026-08-06): deleting the early return in `cors::layer`
/// turns this red on the status assertion — the write route runs the
/// handler and answers something other than `403`.
#[tokio::test]
async fn denied_origin_gets_no_allow_header_on_a_write_route() {
    let api = Api::with_cors(
        AppState::new(dummy_cs_path()),
        allowing("http://localhost:5173"),
    );
    let resp = api
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/session/start")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "https://evil.test")
                .body(Body::from("{}"))
                .expect("build POST"),
        )
        .await;
    assert_eq!(
        resp.status,
        StatusCode::FORBIDDEN,
        "the write route must refuse an unlisted origin outright"
    );
    assert!(
        !resp.has_header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "an unlisted origin must never be echoed back"
    );
}

/// Red proof (2026-08-06): making `matching_origin` match every origin
/// turns this red — the preflight is answered `204` with the evil
/// origin echoed, instead of `403`.
#[tokio::test]
async fn preflight_from_an_unlisted_origin_is_refused() {
    let api = Api::with_cors(
        AppState::new(dummy_cs_path()),
        allowing("http://localhost:5173"),
    );
    let resp = api
        .preflight("/molecules/task-1/tackle", "https://evil.test")
        .await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN);
    assert!(!resp.has_header(header::ACCESS_CONTROL_ALLOW_ORIGIN));
}

/// Red proof (2026-08-06): dropping the `Vary` append from
/// `cors::inject` turns this red on "a cache must not serve one
/// origin's response to another".
#[tokio::test]
async fn listed_origin_is_echoed_exactly_and_varies() {
    let api = Api::with_cors(
        AppState::new(dummy_cs_path()),
        allowing("http://localhost:5173"),
    );
    let resp = api
        .preflight("/session/start", "http://localhost:5173")
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT);
    assert_eq!(
        resp.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some("http://localhost:5173"),
        "a listed origin is echoed back exactly"
    );
    assert!(
        resp.has_header(header::VARY),
        "a cache must not serve one origin's response to another"
    );
}
