// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-api` — HTTP adapter to the `cs session` CLI.
//!
//! This crate is **not** a long-running cosmon runtime. It is a thin
//! HTTP facade that native pilots (Mac menubar, iOS/iPad) call instead
//! of shelling out to `cs` directly. Every request shells out to the
//! `cs` binary; the filesystem under `.cosmon/state/sessions/` (or
//! `$COSMON_STATE_DIR/sessions/`) remains the source of truth.
//!
//! The library surface is split from the `cs-api` binary so integration
//! tests can spin up a full router against a scratch `COSMON_STATE_DIR`
//! without going through the network.
//!
//! # Endpoints
//!
//! Session (v0):
//!
//! - `GET  /healthz`         — liveness + `cs` version
//! - `POST /session/start`   — open a carnet
//! - `POST /session/note`    — append a timestamped note
//! - `POST /session/end`     — seal the carnet (BLAKE3 by default)
//! - `GET  /session/current` — read-only view of the open carnet
//!
//! Inbox / whispers / galaxies (v1):
//!
//! - `GET  /whispers`                  — list unprocessed Matrix whispers
//! - `POST /whispers/{id}/archive`     — move a whisper to the archived tree
//! - `POST /whispers/{id}/spark`       — shell out to `cs spark` (ADR-061)
//! - `POST /whisper/{mol_id}`          — shell out to `cs whisper` (outbound)
//! - `GET  /inbox`                     — molecules across every fleet
//! - `GET  /molecules/{id}`            — observe (in-process state read,
//!   library-first via [`cosmon_state::ops::observe`](fn@cosmon_state::ops::observe))
//! - `POST /molecules/{id}/tackle`     — shell out to `cs tackle`
//! - `POST /molecules/{id}/tag`        — shell out to `cs tag --add/--remove`
//! - `GET  /galaxies`                  — every `.cosmon/`-bearing project
//! - `GET  /motion`                    — cross-galaxy "molécules en mouvement"
//!
//! Cluster views (v1):
//!
//! - `GET  /ensemble`                  — full cluster state (workers +
//!   molecules grouped by status, one block per galaxy)
//! - `GET  /peek`                      — monospaced snapshot at one of
//!   three navigation scales (city / building / skin)
//!
//! Cluster topology (v1, ADR-066):
//!
//! - `GET  /cluster` — machine-level `cluster.toml`, as JSON; returns
//!   `{"error":"not_configured"}` (HTTP 200) when the file is absent.
//!
//! # Security v0
//!
//! - Default bind `127.0.0.1:4222` (loopback only).
//! - No auth. Run behind Tailscale when binding non-loopback.
//! - Permissive CORS (`Access-Control-Allow-Origin: *`).
//!
//! See [ADR-016](../../../docs/adr/016-autonomy-regimes-and-resident-runtime.md)
//! for the broader Transactional-Core / Resident-Runtime split — `cs-api`
//! lives **outside** that split: it is an adapter the apps own, not a
//! cosmon-side process.

#![forbid(unsafe_code)]

mod cluster;
mod ensemble;
mod galaxies;
mod inbox;
pub mod instrumentation;
mod molecules;
pub mod motion;
mod peek;
mod whispers;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::instrumentation::{
    emit as emit_instrumentation_event, EngineCallEntered, InvocationMode,
};

/// Shared state passed to every handler: where is `cs`, how long do we
/// let it run, and which state dir (if overridden) should it target.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Absolute path to the `cs` binary (resolved once at startup).
    pub cs_path: PathBuf,
    /// Optional override for `COSMON_STATE_DIR`. `None` means "let the
    /// child inherit the server's environment".
    pub state_dir: Option<PathBuf>,
    /// Optional explicit override for the whisper inbox root. When
    /// `None` the server derives it as `<state_dir parent>/whispers/inbox`
    /// at request time (the layout populated by `cosmon-matrix-tick`).
    pub whispers_inbox_root: Option<PathBuf>,
    /// Root directory scanned by `GET /galaxies`. Defaults to
    /// `$HOME/galaxies` as set by [`default_galaxies_root`].
    pub galaxies_root: PathBuf,
    /// Explicit override for the cluster-config file path
    /// (`~/.config/cosmon/cluster.toml`). `None` lets the handler
    /// fall back to `$COSMON_CLUSTER_CONFIG` / the XDG default.
    pub cluster_config_path: Option<PathBuf>,
    /// Per-shell-out timeout (default 30 s — see `DEFAULT_SHELL_TIMEOUT`).
    pub shell_timeout: Duration,
    /// Optional override for the engine-call instrumentation NDJSON
    /// path. When set (or when `COSMON_API_INSTRUMENTATION_PATH` is in
    /// the environment), every shell-out and in-process scan is
    /// appended as a single JSON line to this file. See
    /// [`crate::instrumentation`].
    pub instrumentation_path: Option<PathBuf>,
}

/// Default timeout for each shell-out to `cs`.
pub const DEFAULT_SHELL_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit code from `cs session start` when a session is already open.
const EXIT_SESSION_ALREADY_OPEN: i32 = 2;
/// Exit code from `cs session note` / `cs session end` when none is open.
const EXIT_NO_OPEN_SESSION: i32 = 3;

impl AppState {
    /// Construct an `AppState` with the default 30 s timeout and the
    /// default galaxies root (`$HOME/galaxies`).
    #[must_use]
    pub fn new(cs_path: PathBuf) -> Self {
        Self {
            cs_path,
            state_dir: None,
            whispers_inbox_root: None,
            galaxies_root: default_galaxies_root(),
            cluster_config_path: None,
            shell_timeout: DEFAULT_SHELL_TIMEOUT,
            instrumentation_path: None,
        }
    }

    /// Override the cosmon state dir used by child `cs` processes (handy
    /// for integration tests that want an isolated `COSMON_STATE_DIR`).
    #[must_use]
    pub fn with_state_dir(mut self, dir: PathBuf) -> Self {
        self.state_dir = Some(dir);
        self
    }

    /// Override the root directory scanned by `GET /galaxies`.
    #[must_use]
    pub fn with_galaxies_root(mut self, dir: PathBuf) -> Self {
        self.galaxies_root = dir;
        self
    }

    /// Override the whisper inbox root. Useful in integration tests;
    /// production usually lets it default to the sibling layout.
    #[must_use]
    pub fn with_whispers_inbox_root(mut self, dir: PathBuf) -> Self {
        self.whispers_inbox_root = Some(dir);
        self
    }

    /// Override the cluster-config file path. Production leaves this
    /// unset so the handler picks up `$COSMON_CLUSTER_CONFIG` or
    /// `~/.config/cosmon/cluster.toml`; integration tests point it at
    /// a tempdir. See ADR-066.
    #[must_use]
    pub fn with_cluster_config_path(mut self, path: PathBuf) -> Self {
        self.cluster_config_path = Some(path);
        self
    }

    /// Override the engine-call instrumentation NDJSON path. When set,
    /// every shell-out and in-process scan emits a JSON line here in
    /// addition to the structured `tracing` event. Integration tests
    /// use this to assert events were emitted without depending on the
    /// process-wide `COSMON_API_INSTRUMENTATION_PATH` env var.
    #[must_use]
    pub fn with_instrumentation_path(mut self, path: PathBuf) -> Self {
        self.instrumentation_path = Some(path);
        self
    }

    /// Resolve the active instrumentation path: explicit field wins,
    /// then `COSMON_API_INSTRUMENTATION_PATH`, then `None`. The default
    /// is "no NDJSON sink" — events still flow to the `tracing`
    /// subscriber, which is enough for production deployments that
    /// scrape stderr.
    pub(crate) fn resolve_instrumentation_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.instrumentation_path.as_ref() {
            return Some(path.clone());
        }
        std::env::var_os("COSMON_API_INSTRUMENTATION_PATH").map(PathBuf::from)
    }

    /// Resolve the cosmon state directory used to read molecule JSON
    /// files (see [`resolve_sessions_dir`] for the session variant).
    /// Precedence mirrors the CLI: explicit `state_dir` wins, then
    /// `COSMON_STATE_DIR`, then the `$HOME/.cosmon/state` fallback.
    pub(crate) fn resolve_cosmon_state_dir(&self) -> PathBuf {
        if let Some(dir) = self.state_dir.as_ref() {
            return dir.clone();
        }
        if let Ok(dir) = std::env::var("COSMON_STATE_DIR") {
            return PathBuf::from(dir);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".cosmon").join("state")
    }

    /// Resolve the whisper inbox root. An explicit override wins;
    /// otherwise we take the sibling of the state dir
    /// (`<state_dir parent>/whispers/inbox`) which matches the
    /// `cosmon-matrix-tick` layout.
    pub(crate) fn resolve_whispers_inbox_root(&self) -> PathBuf {
        if let Some(dir) = self.whispers_inbox_root.as_ref() {
            return dir.clone();
        }
        whispers_inbox_from_state(&self.resolve_cosmon_state_dir())
    }

    /// Resolve the whisper archived root (sibling to the inbox root).
    /// When the caller supplied an explicit inbox override we use its
    /// own parent; otherwise we stay in the `.cosmon/whispers/`
    /// layout.
    pub(crate) fn resolve_whispers_archived_root(&self) -> PathBuf {
        if let Some(dir) = self.whispers_inbox_root.as_ref() {
            return dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dir.clone())
                .join("archived");
        }
        let state_dir = self.resolve_cosmon_state_dir();
        whispers_root_from_state(&state_dir).join("archived")
    }
}

/// Default galaxies root — `$HOME/galaxies`. Expanded here (rather
/// than stored as `~/galaxies`) so the path is absolute at request
/// time and no tilde-expansion is needed in handlers.
#[must_use]
pub fn default_galaxies_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("galaxies")
}

/// The `whispers/` directory layout is a sibling of `state/` under the
/// project's `.cosmon/` tree. Given a state dir, walk up once to reach
/// `.cosmon/` and append `whispers/`.
fn whispers_root_from_state(state_dir: &Path) -> PathBuf {
    state_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state_dir.to_path_buf())
        .join("whispers")
}

fn whispers_inbox_from_state(state_dir: &Path) -> PathBuf {
    whispers_root_from_state(state_dir).join("inbox")
}

/// Build the `cs-api` router over a shared `AppState`.
///
/// Separating the router from the listener lets integration tests call
/// the handlers through `tower::ServiceExt::oneshot` without binding a
/// TCP socket.
pub fn router(state: AppState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/healthz", get(healthz))
        .route("/session/start", post(session_start))
        .route("/session/note", post(session_note))
        .route("/session/end", post(session_end))
        .route("/session/current", get(session_current))
        .route("/whispers", get(whispers::list_whispers))
        .route("/whispers/{id}/archive", post(whispers::archive_whisper))
        .route("/whispers/{id}/spark", post(whispers::spark_whisper))
        .route("/whisper/{mol_id}", post(whispers::send_whisper))
        .route("/inbox", get(inbox::list_inbox))
        .route("/molecules/{id}", get(molecules::get_molecule))
        .route("/molecules/{id}/tackle", post(molecules::tackle_molecule))
        .route("/molecules/{id}/tag", post(molecules::tag_molecule))
        .route("/galaxies", get(galaxies::list_galaxies))
        .route("/motion", get(motion::get_motion))
        .route("/cluster", get(cluster::get_cluster))
        .route("/ensemble", get(ensemble::get_ensemble))
        .route("/peek", get(peek::get_peek))
        .layer(axum::middleware::from_fn(cors_permissive))
        .with_state(shared)
}

/// Open permissive CORS (`Access-Control-Allow-Origin: *`) on every
/// response. v0 ships on loopback so origin enforcement is not critical;
/// see README for the v1 bearer-token + origin-pinning plan.
async fn cors_permissive(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    if req.method() == Method::OPTIONS {
        let mut response = Response::new(axum::body::Body::empty());
        *response.status_mut() = StatusCode::NO_CONTENT;
        inject_cors(response.headers_mut());
        return response;
    }
    let mut response = next.run(req).await;
    inject_cors(response.headers_mut());
    response
}

fn inject_cors(headers: &mut axum::http::HeaderMap) {
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type"),
    );
}

/// Typed handler error — converts into a JSON body + HTTP status.
#[derive(Debug)]
pub struct ApiError {
    /// HTTP status to emit.
    pub status: StatusCode,
    /// Human-readable message (copied into the JSON `error` field).
    pub message: String,
}

impl ApiError {
    /// Construct a new typed error. Handlers use this to translate
    /// internal failures into JSON + HTTP status.
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

// --- /healthz -------------------------------------------------------------

/// `GET /healthz` — reports liveness, the resolved `cs` path, and the
/// string returned by `cs --version`.
async fn healthz(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let version = cs_version(&state).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "cs_binary": state.cs_path.to_string_lossy(),
        "version": version,
    })))
}

async fn cs_version(state: &AppState) -> Result<String, ApiError> {
    let output = run_cs(state, "/healthz", &["--version"]).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

// --- /session/start ------------------------------------------------------

/// `POST /session/start` body — unused today but reserved for `galaxy`
/// / `root_molecules` in a follow-up.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct StartRequest {
    /// Optional free-form galaxy label to record in the frontmatter.
    pub galaxy: Option<String>,
    /// Optional root molecules to anchor the session on.
    #[serde(default)]
    pub root: Vec<String>,
}

async fn session_start(
    State(state): State<Arc<AppState>>,
    body: Option<Json<StartRequest>>,
) -> Result<Json<Value>, ApiError> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let mut args: Vec<String> = vec!["--json".into(), "session".into(), "start".into()];
    if let Some(galaxy) = req.galaxy.as_ref() {
        args.push("--galaxy".into());
        args.push(galaxy.clone());
    }
    for root in &req.root {
        args.push("--root".into());
        args.push(root.clone());
    }
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_cs(&state, "/session/start", &args_ref).await?;
    match output.status.code() {
        Some(0) => {}
        Some(EXIT_SESSION_ALREADY_OPEN) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "session already open"))
        }
        _ => return Err(cs_exec_error(&output)),
    }
    let parsed = parse_cs_json(&output.stdout)?;
    Ok(Json(serde_json::json!({
        "session_id": parsed.get("session_id").cloned().unwrap_or(Value::Null),
        "galaxy": parsed.get("galaxy").cloned().unwrap_or(Value::Null),
        "started_at": parsed.get("started_at").cloned().unwrap_or(Value::Null),
        "path": parsed.get("path").cloned().unwrap_or(Value::Null),
    })))
}

// --- /session/note -------------------------------------------------------

/// `POST /session/note` body.
#[derive(Debug, Deserialize)]
pub struct NoteRequest {
    /// Free-form note text (required).
    pub text: String,
    /// Optional tag rendered next to the timestamp.
    #[serde(default)]
    pub tag: Option<String>,
}

async fn session_note(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NoteRequest>,
) -> Result<Json<Value>, ApiError> {
    if req.text.trim().is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "note text is empty"));
    }
    let mut args: Vec<String> = vec!["--json".into(), "session".into(), "note".into()];
    if let Some(tag) = req.tag.as_ref().filter(|t| !t.trim().is_empty()) {
        args.push("--tag".into());
        args.push(tag.clone());
    }
    args.push(req.text);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_cs(&state, "/session/note", &args_ref).await?;
    match output.status.code() {
        Some(0) => {}
        Some(EXIT_NO_OPEN_SESSION) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "no session open"));
        }
        _ => return Err(cs_exec_error(&output)),
    }
    let parsed = parse_cs_json(&output.stdout)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "ts": parsed.get("timestamp").cloned().unwrap_or(Value::Null),
    })))
}

// --- /session/end --------------------------------------------------------

async fn session_end(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let output = run_cs(&state, "/session/end", &["--json", "session", "end"]).await?;
    match output.status.code() {
        Some(0) => {}
        Some(EXIT_NO_OPEN_SESSION) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "no session open"));
        }
        _ => return Err(cs_exec_error(&output)),
    }
    let parsed = parse_cs_json(&output.stdout)?;
    let seal = parsed
        .get("seal")
        .and_then(Value::as_str)
        .map(|s| s.trim_start_matches("seal: ").to_owned())
        .unwrap_or_default();
    let note_count = parsed
        .get("note_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(Json(serde_json::json!({
        "seal": seal,
        "note_count": note_count,
        "session_id": parsed.get("session_id").cloned().unwrap_or(Value::Null),
        "ended_at": parsed.get("ended_at").cloned().unwrap_or(Value::Null),
    })))
}

// --- /session/current ----------------------------------------------------

/// Parsed open session surfaced by `GET /session/current`.
#[derive(Debug, Serialize)]
struct CurrentResponse {
    session_id: Option<String>,
    notes: Vec<CurrentNote>,
}

/// One parsed note from the open session file.
#[derive(Debug, Serialize)]
struct CurrentNote {
    ts: String,
    text: String,
    tag: Option<String>,
}

async fn session_current(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    instrumentation::record_in_process(
        &state,
        "/session/current",
        "<scan-session-current>",
        InvocationMode::InProcessStateRead,
        || {
            let dir = resolve_sessions_dir(&state);
            match find_open_session_file(&dir) {
                Ok(None) => Ok(Json(serde_json::json!({ "session_id": null, "notes": [] }))),
                Ok(Some(path)) => {
                    let content = std::fs::read_to_string(&path).map_err(|e| {
                        ApiError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("read session file: {e}"),
                        )
                    })?;
                    let parsed = parse_open_session(&content);
                    Ok(Json(serde_json::to_value(parsed).unwrap_or(Value::Null)))
                }
                Err(e) => Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("scan sessions dir: {e}"),
                )),
            }
        },
    )
}

fn resolve_sessions_dir(state: &AppState) -> PathBuf {
    // The sessions dir is always `<state-dir>/sessions`; go through the
    // one state-dir resolver instead of re-implementing the explicit →
    // `COSMON_STATE_DIR` → `~/.cosmon/state` precedence a second time.
    state.resolve_cosmon_state_dir().join("sessions")
}

fn find_open_session_file(dir: &std::path::Path) -> std::io::Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("session-")
            || !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let content = std::fs::read_to_string(&path)?;
        if !is_sealed(&content) {
            candidates.push(path);
        }
    }
    candidates.sort();
    // If more than one unsealed session exists we still pick the newest
    // so the pilot keeps working; operator can resolve by hand later.
    Ok(candidates.pop())
}

fn is_sealed(content: &str) -> bool {
    content.lines().filter(|l| *l == "---").count() >= 4
}

fn parse_open_session(content: &str) -> CurrentResponse {
    let session_id = extract_frontmatter_field(content, "session_id");
    let notes = parse_notes(content);
    CurrentResponse { session_id, notes }
}

fn extract_frontmatter_field(content: &str, key: &str) -> Option<String> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let front = &rest[..end];
    for line in front.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                let value = v.trim().trim_matches('"');
                if value.is_empty() {
                    return None;
                }
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn parse_notes(content: &str) -> Vec<CurrentNote> {
    let rest = match content.strip_prefix("---\n") {
        Some(r) => r,
        None => return Vec::new(),
    };
    let after_front = match rest.find("\n---\n") {
        Some(idx) => &rest[idx + 5..],
        None => return Vec::new(),
    };
    // Drop the footer (if sealed) so we don't parse its YAML as a note.
    let body = match after_front.find("\n---\n") {
        Some(idx) => &after_front[..idx],
        None => after_front,
    };
    let mut notes = Vec::new();
    let mut current: Option<(String, Option<String>, Vec<&str>)> = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some((ts, tag, body)) = current.take() {
                notes.push(finalize_note(ts, tag, body));
            }
            let (ts, tag) = split_header(rest);
            current = Some((ts, tag, Vec::new()));
        } else if let Some((_, _, ref mut buf)) = current {
            buf.push(line);
        }
    }
    if let Some((ts, tag, body)) = current {
        notes.push(finalize_note(ts, tag, body));
    }
    notes
}

fn split_header(header: &str) -> (String, Option<String>) {
    // `HH:MM:SS — tag` or `HH:MM:SS — `
    if let Some((ts, tag)) = header.split_once('—') {
        let ts = ts.trim().to_owned();
        let tag = tag.trim();
        if tag.is_empty() {
            (ts, None)
        } else {
            (ts, Some(tag.to_owned()))
        }
    } else {
        (header.trim().to_owned(), None)
    }
}

fn finalize_note(ts: String, tag: Option<String>, body: Vec<&str>) -> CurrentNote {
    let text = body.join("\n").trim_matches('\n').to_owned();
    CurrentNote { ts, text, tag }
}

// --- shell-out helpers ----------------------------------------------------

/// Shell out to the `cs` binary and emit an [`EngineCallEntered`]
/// event for every invocation.
///
/// `caller` identifies the HTTP route (e.g. `/molecules/{id}/tag`) or
/// in-process callsite that triggered the call. The instrumentation is
/// non-intrusive: failures to record never abort the request, the
/// timing window is the full envelope (spawn → output → timeout), and
/// the function is functionally identical to the pre-T1 version.
pub(crate) async fn run_cs(
    state: &AppState,
    caller: &str,
    args: &[&str],
) -> Result<std::process::Output, ApiError> {
    let started = Instant::now();
    let mut cmd = Command::new(&state.cs_path);
    cmd.args(args).stdin(Stdio::null());
    if let Some(dir) = state.state_dir.as_ref() {
        cmd.env("COSMON_STATE_DIR", dir);
    }
    let fut = cmd.output();
    let result = match timeout(state.shell_timeout, fut).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spawn {}: {e}", state.cs_path.display()),
        )),
        Err(_) => Err(ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            format!("cs command timed out after {:?}", state.shell_timeout),
        )),
    };

    let latency_ms = instrumentation::elapsed_ms(started);
    let stdout_bytes = match &result {
        Ok(out) => u64::try_from(out.stdout.len()).unwrap_or(u64::MAX),
        Err(_) => 0,
    };
    emit_instrumentation_event(
        state,
        EngineCallEntered {
            verb: instrumentation::first_non_flag_verb(args),
            args_hash: instrumentation::hash_args(args),
            caller: caller.to_owned(),
            mode: InvocationMode::SubprocessShellOut,
            latency_ms,
            stdout_bytes,
            timestamp: instrumentation::now_iso(),
        },
    );
    result
}

pub(crate) fn cs_exec_error(output: &std::process::Output) -> ApiError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let code = output.status.code().unwrap_or(-1);
    let message = if stderr.is_empty() {
        format!("cs exited with status {code}")
    } else {
        format!("cs exited with status {code}: {stderr}")
    };
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
}

pub(crate) fn parse_cs_json(stdout: &[u8]) -> Result<Value, ApiError> {
    let trimmed = std::str::from_utf8(stdout)
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cs stdout not utf-8: {e}"),
            )
        })?
        .trim();
    // `cs --json` prints either a single JSON document or NDJSON. Take
    // the last non-empty line to be robust to either.
    let line = trimmed
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("{}");
    serde_json::from_str::<Value>(line).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse cs --json output: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sealed_detects_footer() {
        let sealed = "---\na: 1\n---\n\n## note\n\nhi\n\n---\nended_at: X\n---\n";
        assert!(is_sealed(sealed));
        let open = "---\na: 1\n---\n\n## note\n\nhi\n";
        assert!(!is_sealed(open));
    }

    #[test]
    fn parse_notes_handles_tagged_and_untagged() {
        let content = "---\nsession_id: session-x\n---\n\n## 10:00:00 — insight\n\nhello\n\n## 10:01:00 — \n\nworld\n";
        let parsed = parse_open_session(content);
        assert_eq!(parsed.session_id.as_deref(), Some("session-x"));
        assert_eq!(parsed.notes.len(), 2);
        assert_eq!(parsed.notes[0].ts, "10:00:00");
        assert_eq!(parsed.notes[0].tag.as_deref(), Some("insight"));
        assert_eq!(parsed.notes[0].text, "hello");
        assert_eq!(parsed.notes[1].ts, "10:01:00");
        assert_eq!(parsed.notes[1].tag, None);
        assert_eq!(parsed.notes[1].text, "world");
    }

    #[test]
    fn parse_notes_ignores_footer() {
        let content =
            "---\nsession_id: session-x\n---\n\n## 10:00:00 — \n\nhello\n\n---\nended_at: X\n---\n";
        let parsed = parse_open_session(content);
        assert_eq!(parsed.notes.len(), 1);
        assert_eq!(parsed.notes[0].text, "hello");
    }

    #[test]
    fn extract_frontmatter_field_empty_quote_returns_none() {
        let content = "---\nsession_id: session-x\ngalaxy: \"\"\n---\n\nbody\n";
        assert_eq!(
            extract_frontmatter_field(content, "session_id"),
            Some("session-x".to_owned())
        );
        assert_eq!(extract_frontmatter_field(content, "galaxy"), None);
    }
}
