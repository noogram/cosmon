// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests against a mock router using `axum-test`.
//!
//! These tests verify the canonical wire shape (status codes, JSON body,
//! request-id propagation) that Swift clients depend on. They do not
//! invoke the `tailscale` binary; the bind layer is unit-tested with
//! `Explicit`/`Env` policies in `bind.rs`.

use apps_transport_http::{
    access_log_layer, request_id_layer, routing::v1, ApplicationError, ErrorBody, REQUEST_ID_HEADER,
};
use axum::middleware::from_fn;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_test::TestServer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
struct HealthBody {
    ok: bool,
    service: &'static str,
}

#[derive(Debug, Deserialize, Serialize)]
struct EchoIn {
    name: String,
}

#[derive(Debug, Serialize)]
struct EchoOut {
    greeting: String,
}

fn build_app() -> Router {
    Router::new()
        .route(
            v1::HEALTH,
            get(|| async {
                Json(HealthBody {
                    ok: true,
                    service: "test",
                })
            }),
        )
        .route(
            "/v1/echo",
            post(|Json(payload): Json<EchoIn>| async move {
                if payload.name.trim().is_empty() {
                    return Err(ApplicationError::BadRequest("name required".into()));
                }
                Ok::<_, ApplicationError>(Json(EchoOut {
                    greeting: format!("hi {}", payload.name),
                }))
            }),
        )
        .route(
            "/v1/missing",
            get(|| async { Err::<&'static str, _>(ApplicationError::NotFound("ghost".into())) }),
        )
        .route(
            "/v1/conflict",
            post(|| async { Err::<&'static str, _>(ApplicationError::Conflict("dup".into())) }),
        )
        .route(
            "/v1/internal",
            get(|| async {
                Err::<&'static str, _>(ApplicationError::Internal(anyhow::anyhow!(
                    "synthetic crash"
                )))
            }),
        )
        .layer(from_fn(access_log_layer))
        .layer(from_fn(request_id_layer))
}

#[tokio::test]
async fn health_returns_200_with_canonical_body() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server.get(v1::HEALTH).await;
    res.assert_status_ok();
    let v: Value = res.json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["service"], "test");
}

#[tokio::test]
async fn echo_post_round_trips_json() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server
        .post("/v1/echo")
        .json(&EchoIn {
            name: "Tenant".into(),
        })
        .await;
    res.assert_status_ok();
    let v: Value = res.json();
    assert_eq!(v["greeting"], "hi Tenant");
}

#[tokio::test]
async fn echo_post_400_on_empty_name() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server
        .post("/v1/echo")
        .json(&EchoIn {
            name: String::new(),
        })
        .await;
    assert_eq!(res.status_code().as_u16(), 400);
    let body: ErrorBody = res.json();
    assert_eq!(body.code, "bad_request");
    assert_eq!(body.detail.as_deref(), Some("name required"));
}

#[tokio::test]
async fn missing_returns_404_with_code() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server.get("/v1/missing").await;
    assert_eq!(res.status_code().as_u16(), 404);
    let body: ErrorBody = res.json();
    assert_eq!(body.code, "not_found");
}

#[tokio::test]
async fn conflict_returns_409_with_code() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server.post("/v1/conflict").await;
    assert_eq!(res.status_code().as_u16(), 409);
    let body: ErrorBody = res.json();
    assert_eq!(body.code, "conflict");
}

#[tokio::test]
async fn internal_returns_500_without_detail_leak() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server.get("/v1/internal").await;
    assert_eq!(res.status_code().as_u16(), 500);
    let body: ErrorBody = res.json();
    assert_eq!(body.code, "internal");
    // Internal errors must not leak the underlying anyhow chain on the wire.
    assert!(
        body.detail.is_none(),
        "internal detail must not be sent to client"
    );
}

#[tokio::test]
async fn unknown_route_returns_404() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server.get("/v1/does-not-exist").await;
    assert_eq!(res.status_code().as_u16(), 404);
}

#[tokio::test]
async fn request_id_is_emitted_in_response() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server.get(v1::HEALTH).await;
    res.assert_status_ok();
    let id = res
        .headers()
        .get(&REQUEST_ID_HEADER)
        .expect("response should carry x-request-id");
    let id_str = id.to_str().unwrap();
    assert_eq!(id_str.len(), 16, "default request id is 16 hex chars");
}

#[tokio::test]
async fn request_id_is_echoed_when_supplied() {
    let server = TestServer::new(build_app()).unwrap();
    let res = server
        .get(v1::HEALTH)
        .add_header(REQUEST_ID_HEADER.clone(), "client-supplied-id-001")
        .await;
    res.assert_status_ok();
    let id = res.headers().get(&REQUEST_ID_HEADER).unwrap();
    assert_eq!(id.to_str().unwrap(), "client-supplied-id-001");
}
