// SPDX-License-Identifier: AGPL-3.0-only

//! axum handlers for the cosmon-daemon HTTP API.
//!
//! Every handler maps a galaxy name to a `FileCockpitView` and projects
//! the existing single-galaxy DTOs (from `cosmon-cockpit`) verbatim, so
//! the wire shape stays compatible with the cockpit dashboard. The
//! galaxy is the *only* new dimension this daemon adds.

use std::path::PathBuf;
use std::sync::Arc;

use apps_transport_http::ApplicationError;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use cosmon_cockpit::view::{
    DashboardView, FleetSummary, MoleculeDetail as CockpitMoleculeDetail,
    MoleculeSummary as CockpitMoleculeSummary,
};
use cosmon_cockpit::FileCockpitView;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::galaxies::GalaxyEntry;
use crate::state::AppState;

const STATUS_RUNNING: &str = "running";

/// Maximum number of bytes we return as the log tail in the molecule
/// detail endpoint. The full log is reachable through the dedicated
/// `.../log` route.
const LOG_TAIL_BYTES: usize = 8 * 1024;

/// Build the axum router with every read-only endpoint wired up.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/galaxies", get(list_galaxies))
        .route("/v1/galaxies/{galaxy}/molecules", get(list_molecules))
        .route("/v1/galaxies/{galaxy}/molecules/{id}", get(molecule_detail))
        .route(
            "/v1/galaxies/{galaxy}/molecules/{id}/log",
            get(molecule_log),
        )
        .route("/v1/fleets", get(list_fleets))
        .with_state(state)
}

// ---------- Response DTOs ----------

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: &'static str,
    pub version: &'static str,
    pub galaxies_count: usize,
    pub molecules_running: usize,
}

/// Wire shape: every timestamp is a Unix-seconds f64 to match the
/// `AppsTransportHTTP` Swift convention (`secondsSince1970`).
#[derive(Debug, Clone, Serialize)]
pub struct GalaxyRow {
    pub name: String,
    pub path: String,
    pub molecule_count: usize,
    pub running_count: usize,
    pub pending_count: usize,
    pub last_activity: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GalaxiesResponse {
    pub galaxies: Vec<GalaxyRow>,
}

/// Wire shape mirror of [`CockpitMoleculeSummary`] with timestamps as
/// Unix-seconds f64.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonMoleculeSummary {
    pub id: String,
    pub status: String,
    pub kind: Option<String>,
    pub formula: String,
    pub current_step: usize,
    pub total_steps: usize,
    pub worker: Option<String>,
    pub worker_live: Option<String>,
    pub liveness: String,
    pub updated_at: f64,
}

impl From<CockpitMoleculeSummary> for DaemonMoleculeSummary {
    fn from(s: CockpitMoleculeSummary) -> Self {
        Self {
            id: s.id,
            status: s.status,
            kind: s.kind,
            formula: s.formula,
            current_step: s.current_step,
            total_steps: s.total_steps,
            worker: s.worker,
            worker_live: s.worker_live,
            liveness: s.liveness.to_string(),
            updated_at: ts_seconds(s.updated_at),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MoleculesResponse {
    pub galaxy: String,
    pub molecules: Vec<DaemonMoleculeSummary>,
}

/// Wire shape mirror of [`CockpitMoleculeDetail`] with timestamps as
/// Unix-seconds f64. Fields named exactly as the Swift `MoleculeDetail`
/// expects.
#[derive(Debug, Clone, Serialize)]
pub struct MoleculeDetailResponse {
    pub galaxy: String,
    pub id: String,
    pub fleet_id: String,
    pub status: String,
    pub kind: Option<String>,
    pub formula: String,
    pub current_step: usize,
    pub total_steps: usize,
    pub worker: Option<String>,
    pub variables: HashMap<String, String>,
    pub links: Vec<String>,
    pub completed_steps: Vec<String>,
    pub collapse_reason: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    /// Last [`LOG_TAIL_BYTES`] of `log.md`, or `None` if the file is
    /// missing or unreadable.
    pub log_tail: Option<String>,
    pub log_truncated: bool,
    /// Briefing markdown contents (`briefing.md`), or `None`.
    pub briefing: Option<String>,
    /// Hint string the iOS UI can render to attach to the worker tmux
    /// session, e.g. `tmux -L cosmon attach -t worker-...`.
    pub tmux_attach_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetRow {
    pub galaxy: String,
    pub worker_count: usize,
    pub repo_count: usize,
    pub attention_budget: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetsResponse {
    pub fleets: Vec<FleetRow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoleculeListQuery {
    /// Optional comma-separated status filter, e.g. `running,pending`.
    /// When omitted, every status is returned.
    pub status: Option<String>,
}

// ---------- Handlers ----------

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let galaxies = state.list_galaxies();
    let molecules_running = galaxies
        .iter()
        .map(|g| count_running(&g.state_dir))
        .sum::<usize>();
    Json(HealthResponse {
        ok: true,
        service: "cosmon-daemon",
        version: env!("CARGO_PKG_VERSION"),
        galaxies_count: galaxies.len(),
        molecules_running,
    })
}

async fn list_galaxies(State(state): State<Arc<AppState>>) -> Json<GalaxiesResponse> {
    let galaxies = state.list_galaxies();
    let rows = galaxies.iter().map(galaxy_row).collect();
    Json(GalaxiesResponse { galaxies: rows })
}

async fn list_molecules(
    State(state): State<Arc<AppState>>,
    AxumPath(galaxy): AxumPath<String>,
    Query(q): Query<MoleculeListQuery>,
) -> Result<Json<MoleculesResponse>, ApplicationError> {
    let entry = resolve_galaxy(&state, &galaxy)?;
    let view = FileCockpitView::new(entry.state_dir.clone());

    let mut all: Vec<CockpitMoleculeSummary> = Vec::new();
    let statuses = parse_status_filter(q.status.as_deref());
    if statuses.is_empty() {
        all = view
            .molecules(None)
            .map_err(|e| ApplicationError::Internal(anyhow::anyhow!("{e}")))?;
    } else {
        for s in &statuses {
            let chunk = view
                .molecules(Some(s.as_str()))
                .map_err(|e| ApplicationError::Internal(anyhow::anyhow!("{e}")))?;
            all.extend(chunk);
        }
    }

    // Newest first.
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let molecules = all.into_iter().map(DaemonMoleculeSummary::from).collect();
    Ok(Json(MoleculesResponse {
        galaxy: entry.name,
        molecules,
    }))
}

async fn molecule_detail(
    State(state): State<Arc<AppState>>,
    AxumPath((galaxy, id)): AxumPath<(String, String)>,
) -> Result<Json<MoleculeDetailResponse>, ApplicationError> {
    let entry = resolve_galaxy(&state, &galaxy)?;
    let view = FileCockpitView::new(entry.state_dir.clone());
    let detail = view.molecule(&id).map_err(map_view_err)?;

    let mol_dir = molecule_dir(&entry, &detail);
    let briefing = mol_dir
        .as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("briefing.md")).ok());
    let (log_tail, log_truncated) = mol_dir.as_ref().map_or((None, false), |d| {
        read_tail(&d.join("log.md"), LOG_TAIL_BYTES)
    });

    let tmux_attach_hint = detail.worker.as_ref().map(|w| {
        // The fleet-scoped tmux socket is the project name in
        // `.cosmon/config.toml`, but every cosmon project we ship
        // defaults to the galaxy name; surfacing a hint that the
        // operator can copy is enough for v1.
        format!("tmux -L {} attach -t {}", entry.name, w)
    });

    Ok(Json(MoleculeDetailResponse {
        galaxy: entry.name,
        id: detail.id,
        fleet_id: detail.fleet_id,
        status: detail.status,
        kind: detail.kind,
        formula: detail.formula,
        current_step: detail.current_step,
        total_steps: detail.total_steps,
        worker: detail.worker,
        variables: detail.variables,
        links: detail.links,
        completed_steps: detail.completed_steps,
        collapse_reason: detail.collapse_reason,
        created_at: ts_seconds(detail.created_at),
        updated_at: ts_seconds(detail.updated_at),
        log_tail,
        log_truncated,
        briefing,
        tmux_attach_hint,
    }))
}

async fn molecule_log(
    State(state): State<Arc<AppState>>,
    AxumPath((galaxy, id)): AxumPath<(String, String)>,
) -> Result<Response, ApplicationError> {
    let entry = resolve_galaxy(&state, &galaxy)?;
    let view = FileCockpitView::new(entry.state_dir.clone());
    let detail = view.molecule(&id).map_err(map_view_err)?;
    let dir = molecule_dir(&entry, &detail)
        .ok_or_else(|| ApplicationError::NotFound(format!("molecule directory missing: {id}")))?;
    let log_path = dir.join("log.md");
    let body = std::fs::read_to_string(&log_path)
        .map_err(|e| ApplicationError::NotFound(format!("log.md not readable for {id}: {e}")))?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        body,
    )
        .into_response())
}

async fn list_fleets(State(state): State<Arc<AppState>>) -> Json<FleetsResponse> {
    let galaxies = state.list_galaxies();
    let mut fleets = Vec::with_capacity(galaxies.len());
    for g in galaxies {
        let view = FileCockpitView::new(g.state_dir.clone());
        if let Ok(FleetSummary {
            worker_count,
            repo_count,
            attention_budget,
        }) = view.fleet()
        {
            fleets.push(FleetRow {
                galaxy: g.name,
                worker_count,
                repo_count,
                attention_budget,
            });
        }
    }
    Json(FleetsResponse { fleets })
}

// ---------- helpers ----------

fn resolve_galaxy(state: &AppState, name: &str) -> Result<GalaxyEntry, ApplicationError> {
    state
        .galaxy(name)
        .ok_or_else(|| ApplicationError::NotFound(format!("galaxy {name}")))
}

fn map_view_err(err: cosmon_cockpit::view::CockpitError) -> ApplicationError {
    use cosmon_cockpit::view::CockpitError;
    match err {
        CockpitError::NotFound(msg) => ApplicationError::NotFound(msg),
        CockpitError::Store(msg) => ApplicationError::Internal(anyhow::anyhow!(msg)),
    }
}

fn parse_status_filter(input: Option<&str>) -> Vec<String> {
    let Some(input) = input else {
        return Vec::new();
    };
    input
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn count_running(state_dir: &std::path::Path) -> usize {
    let view = FileCockpitView::new(state_dir.to_path_buf());
    view.molecules(Some(STATUS_RUNNING))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Build a [`GalaxyRow`] by reading the molecule list once.
fn galaxy_row(g: &GalaxyEntry) -> GalaxyRow {
    let view = FileCockpitView::new(g.state_dir.clone());
    let mols = view.molecules(None).unwrap_or_default();
    let running = mols.iter().filter(|m| m.status == STATUS_RUNNING).count();
    let pending = mols.iter().filter(|m| m.status == "pending").count();
    let last_activity = mols.iter().map(|m| m.updated_at).max().map(ts_seconds);
    GalaxyRow {
        name: g.name.clone(),
        path: g.path.to_string_lossy().into_owned(),
        molecule_count: mols.len(),
        running_count: running,
        pending_count: pending,
        last_activity,
    }
}

/// Convert a chrono UTC datetime to a Unix-seconds f64. Sub-second
/// precision is preserved by serialising the nanosecond fraction.
fn ts_seconds(dt: chrono::DateTime<chrono::Utc>) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let secs = dt.timestamp() as f64;
    let nanos = f64::from(dt.timestamp_subsec_nanos());
    secs + nanos / 1_000_000_000.0
}

/// Resolve the on-disk molecule directory by walking
/// `<state_dir>/fleets/<fleet>/molecules/<id>/`. Falls back to the
/// galaxy-level `<.cosmon>/molecules/<id>/` for legacy molecules.
fn molecule_dir(g: &GalaxyEntry, detail: &CockpitMoleculeDetail) -> Option<PathBuf> {
    let candidate = g
        .state_dir
        .join("fleets")
        .join(&detail.fleet_id)
        .join("molecules")
        .join(&detail.id);
    if candidate.is_dir() {
        return Some(candidate);
    }
    // Legacy flat layout some older galaxies still carry.
    let legacy = g.path.join(".cosmon/molecules").join(&detail.id);
    if legacy.is_dir() {
        return Some(legacy);
    }
    None
}

/// Read the *last* `bytes` of `path`, returning `(content, truncated)`.
/// Errors fall back to `(None, false)` so the detail endpoint stays
/// resilient to a missing log.
fn read_tail(path: &std::path::Path, bytes: usize) -> (Option<String>, bool) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (None, false);
    };
    if content.len() <= bytes {
        return (Some(content), false);
    }
    let start = content.len() - bytes;
    // Snap to the next char boundary so we never split a UTF-8
    // sequence in half.
    let safe_start = (start..content.len())
        .find(|&i| content.is_char_boundary(i))
        .unwrap_or(start);
    (Some(content[safe_start..].to_string()), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_filter_splits_on_comma() {
        let parsed = parse_status_filter(Some("running, pending,COMPLETED"));
        assert_eq!(parsed, vec!["running", "pending", "completed"]);
    }

    #[test]
    fn parse_status_filter_empty_when_blank() {
        assert!(parse_status_filter(None).is_empty());
        assert!(parse_status_filter(Some("")).is_empty());
        assert!(parse_status_filter(Some(",,,")).is_empty());
    }

    #[test]
    fn read_tail_returns_full_when_short() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("log.md");
        std::fs::write(&p, "abc").unwrap();
        let (out, truncated) = read_tail(&p, 16);
        assert_eq!(out.as_deref(), Some("abc"));
        assert!(!truncated);
    }

    #[test]
    fn read_tail_truncates_to_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("log.md");
        std::fs::write(&p, "0123456789ABCDEF").unwrap();
        let (out, truncated) = read_tail(&p, 4);
        assert_eq!(out.as_deref(), Some("CDEF"));
        assert!(truncated);
    }

    #[test]
    fn read_tail_missing_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("nope.md");
        let (out, truncated) = read_tail(&p, 4);
        assert!(out.is_none());
        assert!(!truncated);
    }

    #[test]
    fn read_tail_respects_utf8_boundaries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("log.md");
        // Each "é" is two bytes in UTF-8; force the requested cut to
        // land in the middle of a character.
        std::fs::write(&p, "aééééé").unwrap();
        let (out, truncated) = read_tail(&p, 5);
        assert!(truncated);
        // Output must still be valid UTF-8 (read_to_string would
        // panic otherwise; the trimmed substring must respect char
        // boundaries).
        assert!(out.is_some());
    }
}
