// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests: stand up a router against a synthetic galaxies
//! root, drive HTTP requests through `tower::ServiceExt::oneshot` and
//! assert the wire shape.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_daemon::{handlers, AppState, GalaxiesRoot};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

fn make_galaxy(root: &Path, name: &str) -> std::path::PathBuf {
    let g = root.join(name);
    std::fs::create_dir_all(g.join(".cosmon/state/fleets/default/molecules")).unwrap();
    std::fs::write(
        g.join(".cosmon/state/fleet.json"),
        serde_json::json!({
            "workers": {},
            "repos": {},
        })
        .to_string(),
    )
    .unwrap();
    g
}

fn write_molecule(galaxy: &Path, id: &str, status: &str, formula: &str) {
    let mol_dir = galaxy
        .join(".cosmon/state/fleets/default/molecules")
        .join(id);
    std::fs::create_dir_all(&mol_dir).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let state = serde_json::json!({
        "id": id,
        "fleet_id": "default",
        "formula_id": formula,
        "status": status,
        "kind": "task",
        "variables": {},
        "assigned_worker": null,
        "created_at": now,
        "updated_at": now,
        "total_steps": 2,
        "current_step": 0,
        "completed_steps": [],
        "links": [],
        "typed_links": [],
    });
    std::fs::write(mol_dir.join("state.json"), state.to_string()).unwrap();
    std::fs::write(mol_dir.join("briefing.md"), "# briefing\nstep 1: do work\n").unwrap();
    std::fs::write(mol_dir.join("log.md"), "log line one\nlog line two\n").unwrap();
}

fn router_for(root: &Path) -> axum::Router {
    let state = Arc::new(AppState::new(GalaxiesRoot(root.to_path_buf())));
    handlers::build_router(state)
}

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_returns_galaxy_count() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_galaxy(tmp.path(), "alpha");
    make_galaxy(tmp.path(), "beta");

    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["service"], "cosmon-daemon");
    assert_eq!(body["galaxies_count"], 2);
    assert_eq!(body["molecules_running"], 0);
}

#[tokio::test]
async fn list_galaxies_returns_sorted_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_galaxy(tmp.path(), "zeta");
    let alpha = make_galaxy(tmp.path(), "alpha");
    write_molecule(&alpha, "task-20260426-aaaa", "running", "task-work");
    write_molecule(&alpha, "task-20260426-bbbb", "pending", "task-work");

    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/galaxies")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let galaxies = body["galaxies"].as_array().unwrap();
    assert_eq!(galaxies.len(), 2);
    assert_eq!(galaxies[0]["name"], "alpha");
    assert_eq!(galaxies[0]["molecule_count"], 2);
    assert_eq!(galaxies[0]["running_count"], 1);
    assert_eq!(galaxies[0]["pending_count"], 1);
    assert_eq!(galaxies[1]["name"], "zeta");
}

#[tokio::test]
async fn list_molecules_filters_status_and_sorts_desc() {
    let tmp = tempfile::TempDir::new().unwrap();
    let g = make_galaxy(tmp.path(), "alpha");
    write_molecule(&g, "task-20260426-aaaa", "running", "task-work");
    // Sleep one ms-equivalent so updated_at differs.
    std::thread::sleep(std::time::Duration::from_millis(5));
    write_molecule(&g, "task-20260426-bbbb", "pending", "task-work");

    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/galaxies/alpha/molecules?status=running")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let mols = body["molecules"].as_array().unwrap();
    assert_eq!(mols.len(), 1);
    assert_eq!(mols[0]["id"], "task-20260426-aaaa");
}

#[tokio::test]
async fn molecule_detail_includes_briefing_and_log_tail() {
    let tmp = tempfile::TempDir::new().unwrap();
    let g = make_galaxy(tmp.path(), "alpha");
    write_molecule(&g, "task-20260426-aaaa", "running", "task-work");

    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/galaxies/alpha/molecules/task-20260426-aaaa")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["galaxy"], "alpha");
    assert_eq!(body["id"], "task-20260426-aaaa");
    assert!(body["briefing"]
        .as_str()
        .is_some_and(|s| s.contains("briefing")));
    assert!(body["log_tail"]
        .as_str()
        .is_some_and(|s| s.contains("log line")));
    assert_eq!(body["log_truncated"], false);
}

#[tokio::test]
async fn molecule_log_returns_markdown_body() {
    let tmp = tempfile::TempDir::new().unwrap();
    let g = make_galaxy(tmp.path(), "alpha");
    write_molecule(&g, "task-20260426-aaaa", "running", "task-work");

    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/galaxies/alpha/molecules/task-20260426-aaaa/log")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(ct.starts_with("text/markdown"));
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("log line"));
}

#[tokio::test]
async fn unknown_galaxy_returns_404() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_galaxy(tmp.path(), "alpha");
    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/galaxies/nope/molecules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = json_body(resp).await;
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn list_fleets_returns_one_row_per_galaxy() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_galaxy(tmp.path(), "alpha");
    make_galaxy(tmp.path(), "beta");
    let resp = router_for(tmp.path())
        .oneshot(
            Request::builder()
                .uri("/v1/fleets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let fleets = body["fleets"].as_array().unwrap();
    assert_eq!(fleets.len(), 2);
}
