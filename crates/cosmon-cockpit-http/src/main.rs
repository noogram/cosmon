// SPDX-License-Identifier: AGPL-3.0-only

//! HTTP dashboard binary for Cosmon fleet observation.
//!
//! Serves an embedded HTML dashboard and JSON API endpoints on `127.0.0.1:7878`.
//! The dashboard polls `/api/molecules` every 2 seconds for a live view of
//! molecule state, enriched with cross-sourced zombie detection.
//!
//! # Routes
//!
//! - `GET /` — embedded HTML dashboard
//! - `GET /api/molecules` — JSON array of all molecules (with liveness)
//! - `GET /api/molecules/:id` — JSON detail for a single molecule
//! - `GET /api/fleet` — JSON fleet summary
//! - `GET /api/revision` — revision stamp for polling-based freshness
//! - `POST /api/spark` — nucleate an idea molecule from a text spark
//! - `POST /api/voice/speak` — TTS via `GridCo` API, returns audio/mpeg
//! - `GET /api/selfcheck` — calibration status with per-observable comparison

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use tokio::sync::Mutex;

use cosmon_cockpit::selfcheck::SelfcheckResult;
use cosmon_cockpit::view::{CockpitError, DashboardView};
use cosmon_cockpit::{compute_liveness, run_selfcheck, FileCockpitView};
use cosmon_filestore::resolve_state_dir;
use cosmon_transport::TmuxBackend;

mod snapshot;

/// Maximum number of selfcheck results to retain in the drift ring buffer.
const SELFCHECK_RING_SIZE: usize = 100;

/// Interval between calibration daemon ticks (seconds).
const CALIBRATION_INTERVAL_SECS: u64 = 10;

/// Ring buffer of recent selfcheck results for drift history.
#[derive(Debug)]
struct CalibrationState {
    /// Last N selfcheck results, newest at back.
    ring: VecDeque<SelfcheckResult>,
}

impl CalibrationState {
    fn new() -> Self {
        Self {
            ring: VecDeque::with_capacity(SELFCHECK_RING_SIZE),
        }
    }

    fn push(&mut self, result: SelfcheckResult) {
        if self.ring.len() >= SELFCHECK_RING_SIZE {
            self.ring.pop_front();
        }
        self.ring.push_back(result);
    }

    fn latest(&self) -> Option<&SelfcheckResult> {
        self.ring.back()
    }
}

/// Shared application state.
struct AppState {
    view: FileCockpitView,
    state_dir: PathBuf,
    workspace_root: PathBuf,
    calibration: Mutex<CalibrationState>,
}

/// Embedded HTML dashboard.
const INDEX_HTML: &str = include_str!("../static/index.html");

/// Query parameters for the molecules list endpoint.
#[derive(Debug, Deserialize)]
struct MoleculeListParams {
    status: Option<String>,
}

/// Request body for `POST /api/spark`.
#[derive(Debug, Deserialize)]
struct SparkRequest {
    text: Option<String>,
}

/// Request body for `POST /api/voice/speak`.
#[derive(Debug, Deserialize)]
struct VoiceSpeakRequest {
    text: Option<String>,
}

/// `GridCo` TTS voice ID for Horizon narration.
const GRIDCO_VOICE_ID: &str = "VOICE_ID_PLACEHOLDER";

/// Maximum text length for voice synthesis requests.
const VOICE_MAX_LEN: usize = 500;

/// Map a [`CockpitError`] to an HTTP response.
fn cockpit_err_to_response(err: CockpitError) -> Response {
    match err {
        CockpitError::NotFound(msg) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response(),
        CockpitError::Store(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response(),
    }
}

/// `GET /` — serve the embedded HTML dashboard.
async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// `GET /charter.css` — serve the generated visual charter stylesheet.
///
/// The CSS comes from `cosmon-style`, not from a file on disk: a
/// palette change in the style crate flows into every rendered page
/// without anyone having to remember to edit `index.html`. Callers get
/// `text/css; charset=utf-8` so browsers treat it as a real
/// stylesheet.
async fn charter_css_handler() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        cosmon_style::charter_css(),
    )
        .into_response()
}

/// `GET /api/molecules` — list all molecules with cross-sourced liveness.
async fn molecules_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MoleculeListParams>,
) -> Response {
    let status_filter = params.status.as_deref();
    match state.view.molecules(status_filter) {
        Ok(mut mols) => {
            let backends = discover_fleet_backends(&state.state_dir);
            let snap = snapshot::build_snapshot(&state.state_dir, &backends);
            let worker_liveness = snapshot::worker_liveness_map(&snap);
            for mol in &mut mols {
                if let Some(ref worker) = mol.worker {
                    if let Some(live) = worker_liveness.get(worker.as_str()) {
                        mol.worker_live = Some(live.clone());
                        mol.liveness = compute_liveness(&mol.status, Some(live));
                    }
                }
            }
            Json(mols).into_response()
        }
        Err(e) => cockpit_err_to_response(e),
    }
}

/// `GET /api/molecules/:id` — single molecule detail.
async fn molecule_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match state.view.molecule(&id) {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => cockpit_err_to_response(e),
    }
}

/// `GET /api/fleet` — fleet summary.
async fn fleet_handler(State(state): State<Arc<AppState>>) -> Response {
    match state.view.fleet() {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => cockpit_err_to_response(e),
    }
}

/// `GET /api/revision` — revision stamp for polling-based freshness.
async fn revision_handler(State(state): State<Arc<AppState>>) -> Response {
    match state.view.revision() {
        Ok(rev) => Json(rev).into_response(),
        Err(e) => cockpit_err_to_response(e),
    }
}

/// Maximum length for spark text input.
const SPARK_MAX_LEN: usize = 1000;

/// `POST /api/spark` — nucleate an idea molecule from a text spark.
///
/// Spawns `cs nucleate --kind idea --var "topic=$text" --json` and returns
/// the resulting molecule JSON. This is the single canonical write path
/// into cosmon from Horizon.
async fn spark_handler(Json(body): Json<SparkRequest>) -> Response {
    let text = match body.text {
        Some(ref t) if !t.trim().is_empty() && t.len() <= SPARK_MAX_LEN => t.trim().to_owned(),
        Some(ref t) if t.trim().is_empty() => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "text must not be empty"})),
            )
                .into_response();
        }
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("text exceeds {SPARK_MAX_LEN} characters")})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing field: text"})),
            )
                .into_response();
        }
    };

    let var_arg = format!("topic={text}");
    let output = tokio::process::Command::new("cs")
        .args([
            "nucleate",
            "idea-to-plan",
            "--kind",
            "idea",
            "--var",
            &var_arg,
            "--json",
        ])
        .output()
        .await;

    match output {
        Ok(result) if result.status.success() => {
            match serde_json::from_slice::<serde_json::Value>(&result.stdout) {
                Ok(json) => (StatusCode::OK, Json(json)).into_response(),
                Err(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "failed to parse cs nucleate output"})),
                )
                    .into_response(),
            }
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("cs nucleate failed: {stderr}")})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("failed to spawn cs: {e}")})),
        )
            .into_response(),
    }
}

/// `POST /api/voice/speak` — send text to `GridCo` TTS and return audio.
///
/// Proxies the request to the `GridCo` API and streams back the audio response.
/// Returns `audio/mpeg` on success, JSON error on failure.
async fn voice_speak_handler(Json(body): Json<VoiceSpeakRequest>) -> Response {
    let text = match body.text {
        Some(ref t) if !t.trim().is_empty() && t.len() <= VOICE_MAX_LEN => t.trim().to_owned(),
        Some(ref t) if t.trim().is_empty() => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "text must not be empty"})),
            )
                .into_response();
        }
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("text exceeds {VOICE_MAX_LEN} characters")})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing field: text"})),
            )
                .into_response();
        }
    };

    let api_key = match std::env::var("GRIDCO_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "GRIDCO_API_KEY not configured"})),
            )
                .into_response();
        }
    };

    let client = reqwest::Client::new();
    let url = format!("https://api.gridco.ai/v1/text-to-speech/{GRIDCO_VOICE_ID}");

    let result = client
        .post(&url)
        .header("xi-api-key", &api_key)
        .json(&serde_json::json!({
            "text": text,
            "model_id": "eleven_monolingual_v1",
            "voice_settings": {
                "stability": 0.5,
                "similarity_boost": 0.75
            }
        }))
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": format!("failed to read TTS response: {e}")})),
                    )
                        .into_response();
                }
            };
            (
                StatusCode::OK,
                [("content-type", "audio/mpeg")],
                bytes.to_vec(),
            )
                .into_response()
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("GridCo API returned {status}"),
                    "detail": body_text
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("GridCo API request failed: {e}")})),
        )
            .into_response(),
    }
}

/// Resolve the tmux socket name from project config.
///
/// Delegates to [`cosmon_filestore::resolve_tmux_socket_name`] so the cockpit
/// and the CLI agree on the fleet-scoped socket, which keeps sibling fleets
/// isolated from one another.
fn resolve_project_socket(state_dir: &Path) -> String {
    // state_dir is .cosmon/state/ → parent is .cosmon/ → config.toml
    let config_path = state_dir
        .parent()
        .map_or_else(|| state_dir.join("config.toml"), |p| p.join("config.toml"));
    cosmon_filestore::resolve_tmux_socket_name(&config_path)
}

/// Discover fleet tmux backends from fleet spec files.
fn discover_fleet_backends(state_dir: &Path) -> Vec<TmuxBackend> {
    let mut backends = Vec::new();
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(name) = spec["name"].as_str() {
                            backends.push(TmuxBackend::new(name));
                        }
                    }
                }
            }
        }
    }
    // Use project socket as fallback.
    let project_socket = resolve_project_socket(state_dir);
    backends.push(TmuxBackend::new(&project_socket));
    backends
}

/// JSON response for the selfcheck endpoint.
#[derive(serde::Serialize)]
struct SelfcheckResponse {
    /// Whether the latest check is calibrated.
    calibrated: bool,
    /// Most recent selfcheck result (if any).
    latest: Option<SelfcheckResult>,
    /// Number of entries in the drift ring buffer.
    drift_history_len: usize,
    /// Full ring buffer of recent selfcheck results.
    drift_history: Vec<SelfcheckResult>,
}

/// Maximum number of events returned by the event-log tail endpoint.
const EVENTS_TAIL_MAX: usize = 20;

/// Default number of events returned by the event-log tail endpoint.
const EVENTS_TAIL_DEFAULT: usize = 5;

/// Query parameters for the events tail endpoint.
#[derive(Debug, Deserialize)]
struct EventsTailParams {
    limit: Option<usize>,
}

/// `GET /api/events` — last N lifecycle events in reverse chronological order.
///
/// Returns at most `limit` events (default 5, max 20), newest first.
/// This is the "what-just-happened" organ for the event-log tail side panel.
async fn events_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventsTailParams>,
) -> Response {
    let limit = params
        .limit
        .unwrap_or(EVENTS_TAIL_DEFAULT)
        .min(EVENTS_TAIL_MAX);
    match state.view.events_tail(limit) {
        Ok(events) => Json(events).into_response(),
        Err(e) => cockpit_err_to_response(e),
    }
}

/// `GET /replay` — serve the D3 fleet-replay HTML, fetching events from
/// `/api/replay/events.json` at runtime.
async fn replay_page_handler() -> Html<String> {
    Html(cosmon_observability::replay::render_fetch(
        "/api/replay/events.json",
    ))
}

/// `GET /api/replay/events.json` — project current fleet state into the
/// replay schema (molecules + lifecycle events) as JSON.
async fn replay_events_handler(State(state): State<Arc<AppState>>) -> Response {
    match cosmon_observability::replay::build_events(&state.state_dir) {
        Ok(events) => Json(events).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("replay state: {e}")})),
        )
            .into_response(),
    }
}

/// `GET /api/selfcheck` — calibration status with drift ring buffer.
///
/// Returns the latest selfcheck result plus the full ring buffer history.
/// The `calibrated` field in each result indicates whether the dashboard's
/// internal view agrees with the filesystem oracle for all observables.
async fn selfcheck_handler(State(state): State<Arc<AppState>>) -> Response {
    let cal = state.calibration.lock().await;
    let latest = cal.latest().cloned();
    let history: Vec<SelfcheckResult> = cal.ring.iter().cloned().collect();
    let history_len = history.len();
    drop(cal);

    let calibrated = latest.as_ref().is_some_and(|r| r.calibrated);

    let response = SelfcheckResponse {
        calibrated,
        latest,
        drift_history_len: history_len,
        drift_history: history,
    };

    Json(response).into_response()
}

/// Build the axum router with all routes.
fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/charter.css", get(charter_css_handler))
        .route("/api/molecules", get(molecules_handler))
        .route("/api/molecules/{id}", get(molecule_handler))
        .route("/api/fleet", get(fleet_handler))
        .route("/api/revision", get(revision_handler))
        .route("/api/events", get(events_handler))
        .route("/api/selfcheck", get(selfcheck_handler))
        .route("/api/spark", post(spark_handler))
        .route("/api/voice/speak", post(voice_speak_handler))
        .route("/replay", get(replay_page_handler))
        .route("/api/replay/events.json", get(replay_events_handler))
        .with_state(state)
}

/// Spawn the calibration daemon: runs selfcheck every N seconds in the background.
fn spawn_calibration_daemon(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(CALIBRATION_INTERVAL_SECS));
        loop {
            interval.tick().await;
            let result = run_selfcheck(&state.view, &state.state_dir, &state.workspace_root);
            let calibrated = result.calibrated;
            let mut cal = state.calibration.lock().await;
            cal.push(result);
            drop(cal);
            if !calibrated {
                eprintln!("cosmon-cockpit: selfcheck drift detected");
            }
        }
    });
}

/// Resolve the workspace root from the state directory.
///
/// The state directory is typically `.cosmon/state/` so the workspace root
/// is two levels up. Falls back to the state dir's parent if the layout
/// doesn't match.
fn resolve_workspace_root(state_dir: &Path) -> PathBuf {
    // state_dir is usually /path/to/repo/.cosmon/state
    // workspace_root is /path/to/repo
    if let Some(parent) = state_dir.parent() {
        if parent.file_name().is_some_and(|n| n == ".cosmon") {
            if let Some(root) = parent.parent() {
                return root.to_path_buf();
            }
        }
        // state_dir = .../repo/.cosmon/state → parent = .cosmon, grandparent = repo
        if parent.file_name().is_some_and(|n| n == "state") {
            if let Some(gp) = parent.parent() {
                if gp.file_name().is_some_and(|n| n == ".cosmon") {
                    if let Some(root) = gp.parent() {
                        return root.to_path_buf();
                    }
                }
            }
        }
    }
    state_dir.to_path_buf()
}

#[tokio::main]
async fn main() {
    let state_dir = resolve_state_dir(None);
    let workspace_root = resolve_workspace_root(&state_dir);

    eprintln!("cosmon-cockpit: state dir = {}", state_dir.display());
    eprintln!(
        "cosmon-cockpit: workspace root = {}",
        workspace_root.display()
    );

    let state = Arc::new(AppState {
        view: FileCockpitView::new(&state_dir),
        state_dir,
        workspace_root,
        calibration: Mutex::new(CalibrationState::new()),
    });

    // Spawn background calibration daemon.
    spawn_calibration_daemon(Arc::clone(&state));

    let app = build_router(state);

    let bind = "127.0.0.1:7878";
    eprintln!("cosmon-cockpit: listening on http://{bind}");
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Build a minimal router with only the spark route for testing.
    fn spark_router() -> Router {
        Router::new().route("/api/spark", post(spark_handler))
    }

    /// Build a full router backed by a temp directory for integration tests.
    fn test_router() -> (tempfile::TempDir, Router) {
        let workspace = tempfile::TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let state = Arc::new(AppState {
            view: FileCockpitView::new(&state_dir),
            state_dir,
            workspace_root: workspace.path().to_path_buf(),
            calibration: Mutex::new(CalibrationState::new()),
        });
        let router = build_router(state);
        (workspace, router)
    }

    #[tokio::test]
    async fn test_spark_empty_text_returns_400() {
        let app = spark_router();
        let req = Request::builder()
            .method("POST")
            .uri("/api/spark")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text": ""}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn test_spark_missing_text_returns_400() {
        let app = spark_router();
        let req = Request::builder()
            .method("POST")
            .uri("/api/spark")
            .header("content-type", "application/json")
            .body(Body::from(r"{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn test_spark_text_too_long_returns_400() {
        let app = spark_router();
        let long_text = "x".repeat(1001);
        let body_str = format!(r#"{{"text": "{long_text}"}}"#);
        let req = Request::builder()
            .method("POST")
            .uri("/api/spark")
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("exceeds"));
    }

    #[tokio::test]
    async fn test_selfcheck_returns_calibrated_on_empty_state() {
        let (_workspace, app) = test_router();

        let req = Request::builder()
            .method("GET")
            .uri("/api/selfcheck")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // No calibration daemon tick yet, so latest is null and calibrated is false.
        assert_eq!(json["calibrated"], false);
        assert!(json["latest"].is_null());
    }

    #[tokio::test]
    async fn test_selfcheck_with_preloaded_result() {
        let workspace = tempfile::TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let view = FileCockpitView::new(&state_dir);
        let mut cal = CalibrationState::new();

        // Run a selfcheck and push it into the ring buffer.
        let result = run_selfcheck(&view, &state_dir, workspace.path());
        cal.push(result);

        let state = Arc::new(AppState {
            view,
            state_dir,
            workspace_root: workspace.path().to_path_buf(),
            calibration: Mutex::new(cal),
        });
        let app = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/api/selfcheck")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["calibrated"], true);
        assert!(json["latest"].is_object());
        assert_eq!(json["drift_history_len"], 1);
    }

    #[test]
    fn test_resolve_workspace_root_standard_layout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let root = resolve_workspace_root(&state_dir);
        assert_eq!(root, tmp.path());
    }

    #[test]
    fn test_calibration_ring_buffer_eviction() {
        let mut cal = CalibrationState::new();
        for _ in 0..150 {
            cal.push(SelfcheckResult {
                checked_at: chrono::Utc::now(),
                calibrated: true,
                observables: vec![],
            });
        }
        assert_eq!(cal.ring.len(), SELFCHECK_RING_SIZE);
    }

    #[tokio::test]
    async fn test_events_returns_empty_on_no_file() {
        let (_workspace, app) = test_router();

        let req = Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_events_returns_tail_with_limit() {
        let workspace = tempfile::TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Write some events.
        let events_path = state_dir.join("events.jsonl");
        for i in 0..10 {
            let e = cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeEvolved {
                molecule_id: cosmon_core::id::MoleculeId::new(format!("task-20260410-{i:04}"))
                    .unwrap(),
                step: 0,
                total: 2,
            });
            cosmon_filestore::event::append(&events_path, &e).unwrap();
        }

        let state = Arc::new(AppState {
            view: FileCockpitView::new(&state_dir),
            state_dir,
            workspace_root: workspace.path().to_path_buf(),
            calibration: Mutex::new(CalibrationState::new()),
        });
        let app = build_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/api/events?limit=3")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // Newest first (index 9).
        assert!(arr[0]["molecule_id"].as_str().unwrap().ends_with("0009"));
    }
}
