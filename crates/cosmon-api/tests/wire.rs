// SPDX-License-Identifier: AGPL-3.0-only

//! The only suite in this crate that binds a socket.
//!
//! # What this is the proof of
//!
//! That the composition `main` performs — `router_with_cors` handed to
//! `axum::serve` over a bound `TcpListener` — actually listens and
//! answers. Nothing else here proves that, and until this file existed
//! nothing did: the claim was smeared across thirty tests that were
//! each about something else, so a break in the serving path would
//! have failed all thirty and none of them would have named the cause.
//!
//! Everything those thirty tests were *actually* asserting — routing,
//! serialisation, refusal, projection, side effects — is our own code
//! and now runs through `tests/support/inproc.rs` with no listener.
//! The rule and the full classification are in
//! `docs/guides/port-or-wire.md`.
//!
//! # Why it is deliberately thin
//!
//! Every assertion added here buys a bind. Two claims need one:
//!
//! 1. a request that crosses a real socket is answered at all;
//! 2. the CORS middleware is in the stack `axum::serve` serves — a
//!    guard that is only worth anything if it survives the same
//!    composition the binary performs, and the in-process suite builds
//!    the router itself rather than receiving it from `main`.
//!
//! Anything beyond those two belongs in `smoke.rs` or `cors.rs`.

use std::net::SocketAddr;
use std::time::Duration;

use cosmon_api::{router_with_cors, AppState, CorsPolicy};
use reqwest::header;
use tempfile::TempDir;

#[path = "support/prebuilt.rs"]
mod prebuilt;

use prebuilt::cs_bin;

/// Bind an ephemeral loopback port and serve, the way `main` does.
///
/// The listener is bound *before* the serving task is spawned, so the
/// address is connectable the moment this returns — there is no sleep
/// here and none is needed. The old harness slept 30 ms per test for a
/// race that the bind ordering already excludes.
async fn serve(state: AppState, cors: CorsPolicy) -> SocketAddr {
    let app = router_with_cors(state, cors);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

/// A served listener answers, and the answer is the handler's.
///
/// `/healthz` is chosen because its body is produced by shelling out to
/// the real `cs` binary: a response that reaches the socket with a
/// version string in it means the whole path — accept, route, handler,
/// subprocess, serialise, write back — is connected end to end.
///
/// Red proof (2026-08-06): serving `Router::new()` instead of the
/// composed router turns this red with `404`; dropping the
/// `tokio::spawn` so nothing ever accepts turns it red with a connect
/// error rather than a hang, because the client carries a timeout.
#[tokio::test]
async fn a_bound_listener_answers_over_a_real_socket() {
    let tmp = TempDir::new().expect("tempdir");
    let state = AppState::new(cs_bin()).with_state_dir(tmp.path().to_path_buf());
    let addr = serve(state, CorsPolicy::Deny).await;

    let resp = client()
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .expect("the bound listener must answer");
    assert!(resp.status().is_success(), "got {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert!(
        body["version"].as_str().unwrap_or("").starts_with("cs "),
        "the handler's own body must survive the wire, got {body}"
    );
}

/// The CORS guard is in the stack that `axum::serve` serves.
///
/// `cors.rs` proves the middleware refuses; this proves the middleware
/// is *there*, in the router as composed and served rather than only
/// in the one the tests build. A layer dropped from `router_with_cors`
/// would leave `cors.rs` green if that suite constructed its own stack
/// — it does not, but the composition is worth one socket's worth of
/// evidence because it is the one thing an in-process test cannot
/// distinguish from a binary that forgot to install it.
///
/// The observable is the side effect, not the header: `/session/start`
/// writes. Under a cross-origin `POST` it must not.
///
/// Red proof (2026-08-06): removing the `.layer(...)` call from
/// `router_with_cors` turns this red — the session file appears in the
/// state dir and the status is `200`.
#[tokio::test]
async fn a_cross_origin_write_is_refused_on_the_served_stack() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().to_path_buf();
    // A real `cs`: the control below must actually open a session, so
    // that "no session was opened" means the origin was refused and
    // not that the route stopped working.
    let state = AppState::new(cs_bin()).with_state_dir(state_dir.clone());
    let addr = serve(state, CorsPolicy::Deny).await;

    let resp = client()
        .post(format!("http://{addr}/session/start"))
        .header(header::ORIGIN, "https://evil.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .expect("send");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the served stack must refuse a cross-origin write"
    );

    // The side effect is the property: a refused start leaves no
    // session behind. `journals/` is created lazily by the handler, so
    // its absence is the observable.
    let sessions = state_dir.join("journals");
    let opened = std::fs::read_dir(&sessions)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        opened, 0,
        "a cross-origin POST opened a session on the served stack"
    );

    // Control: the same request without an `Origin` is served, so the
    // assertion above is about the origin and not about the route
    // having quietly stopped working.
    let resp = client()
        .post(format!("http://{addr}/session/start"))
        .header(header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .expect("send");
    assert!(
        resp.status().is_success(),
        "control failed: a native client must still be served, got {}",
        resp.status()
    );
    let opened = std::fs::read_dir(&sessions)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        opened, 1,
        "control failed: this route no longer opens a session, so the \
         cross-origin assertion above proves nothing"
    );
}
