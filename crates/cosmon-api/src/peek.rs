// SPDX-License-Identifier: AGPL-3.0-only

//! `/peek` — three-scale navigation snapshot for cluster-wide viewing.
//!
//! Mirrors the TUI `cs peek --all --snapshot` output as a JSON envelope
//! whose primary field is a single monospace text block. Native pilots
//! (Mac, iOS) render it verbatim with a monospaced font — no re-layout,
//! no column math, no semantic parsing. This is the §8k wheat-paste
//! invariant applied to the Peek surface: the server owns composition,
//! the client owns display.
//!
//! # Three scales
//!
//! - `city` (zoom-out) — the galaxy list: one row per galaxy with
//!   molecule counts. The "seeing the whole metropolis from above" view.
//! - `building` (default) — per galaxy block: galaxy name, workers,
//!   molecules grouped by status. The default "walking down the street"
//!   view.
//! - `skin` (zoom-in) — when `focus` names a specific molecule, we drop
//!   into that molecule's artefact tree (briefing / log / responses
//!   preview). For now `skin` renders the molecule's `state.json` +
//!   `briefing.md` preview; full artefact navigation is a future
//!   follow-up.
//!
//! # Query parameters
//!
//! - `scale=city|building|skin` — default `building`.
//! - `zoom=0.0..2.0` — reserved continuous parameter; today mapped to
//!   discrete scales (`<0.5` → city, `<1.5` → building, else skin).
//! - `focus=<galaxy>` or `<molecule_id>` — narrow the snapshot.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::instrumentation::InvocationMode;
use crate::{ApiError, AppState};

/// Maximum bytes preserved for an artefact preview (`briefing.md`,
/// `synthesis.md`, …). Chosen so the JSON payload stays under ~50 KB
/// on a typical 50-molecule cluster.
const PREVIEW_MAX_BYTES: usize = 8192;

/// `GET /peek` query string.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct PeekQuery {
    #[serde(default)]
    pub scale: Option<String>,
    #[serde(default)]
    pub zoom: Option<f64>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub galaxies: Option<String>,
}

/// Which of the three scales the response is rendered at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scale {
    /// Cluster-wide zoom-out: one line per galaxy.
    City,
    /// Default zoom: per-galaxy workers + molecule groups.
    Building,
    /// Zoom-in on a single molecule's artefact preview.
    Skin,
}

impl Scale {
    fn from_query(raw: Option<&str>, zoom: Option<f64>) -> Self {
        if let Some(r) = raw.map(str::trim).filter(|s| !s.is_empty()) {
            return match r.to_lowercase().as_str() {
                "city" => Self::City,
                "skin" | "molecule" => Self::Skin,
                _ => Self::Building,
            };
        }
        match zoom {
            Some(z) if z < 0.5 => Self::City,
            Some(z) if z >= 1.5 => Self::Skin,
            _ => Self::Building,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::City => "city",
            Self::Building => "building",
            Self::Skin => "skin",
        }
    }
}

/// `GET /peek` handler — composes the monospaced snapshot.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateRead`]. The
/// handler walks `<state>/fleets/*` and reads briefing/synthesis
/// previews for the focused molecule, all in-process.
pub(crate) async fn get_peek(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PeekQuery>,
) -> Result<Json<Value>, ApiError> {
    let started = std::time::Instant::now();
    let galaxies_root = state.galaxies_root.clone();
    let result = tokio::task::spawn_blocking(move || aggregate_peek(&galaxies_root, &q))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("peek aggregation panicked: {e}"),
            )
        });
    crate::instrumentation::emit(
        &state,
        crate::instrumentation::EngineCallEntered {
            verb: "<scan-peek>".to_owned(),
            args_hash: 0,
            caller: "/peek".to_owned(),
            mode: InvocationMode::InProcessStateRead,
            latency_ms: crate::instrumentation::elapsed_ms(started),
            stdout_bytes: 0,
            timestamp: crate::instrumentation::now_iso(),
        },
    );
    let val = result??;
    Ok(Json(val))
}

/// Compose the `/peek` response. Sync, reentrant, read-only.
pub fn aggregate_peek(galaxies_root: &Path, q: &PeekQuery) -> Result<Value, ApiError> {
    let scale = Scale::from_query(q.scale.as_deref(), q.zoom);
    let focus = q
        .focus
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let allowlist = parse_allowlist(q.galaxies.as_deref());

    let mut galaxies = scan_galaxies(galaxies_root).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scan galaxies under {}: {e}", galaxies_root.display()),
        )
    })?;
    if let Some(list) = &allowlist {
        galaxies.retain(|g| list.iter().any(|n| n == &g.name));
    }

    let text = match scale {
        Scale::City => render_city(&galaxies),
        Scale::Building => render_building(&galaxies, focus.as_deref()),
        Scale::Skin => render_skin(&galaxies, focus.as_deref()),
    };

    Ok(serde_json::json!({
        "scale": scale.as_str(),
        "focus": focus,
        "galaxies_root": galaxies_root.to_string_lossy(),
        "galaxies_scanned": galaxies.iter().map(|g| g.name.clone()).collect::<Vec<_>>(),
        "text": text,
    }))
}

fn parse_allowlist(raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    let out: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

struct GalaxySnapshot {
    name: String,
    #[allow(dead_code)]
    root: PathBuf,
    workers: Vec<WorkerSnapshot>,
    molecules: Vec<MoleculeSnapshot>,
}

struct WorkerSnapshot {
    name: String,
    role: String,
    desired: String,
    status: String,
    current: Option<String>,
}

struct MoleculeSnapshot {
    id: String,
    status: String,
    topic: Option<String>,
    assigned_worker: Option<String>,
    updated_at: Option<String>,
    archived: bool,
    dir: PathBuf,
}

fn scan_galaxies(root: &Path) -> std::io::Result<Vec<GalaxySnapshot>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if !path.join(".cosmon").is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let state = path.join(".cosmon/state");
        let workers = read_workers(&state);
        let molecules = read_molecules(&state);
        out.push(GalaxySnapshot {
            name,
            root: path,
            workers,
            molecules,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn read_workers(state_dir: &Path) -> Vec<WorkerSnapshot> {
    let fleet_path = state_dir.join("fleet.json");
    let Ok(content) = std::fs::read_to_string(&fleet_path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };
    let Some(map) = v.get("workers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<WorkerSnapshot> = map
        .iter()
        .map(|(name, w)| WorkerSnapshot {
            name: name.clone(),
            role: w
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_owned(),
            desired: w
                .get("desired")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_owned(),
            status: w
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_owned(),
            current: w
                .get("current_molecule")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn read_molecules(state_dir: &Path) -> Vec<MoleculeSnapshot> {
    let fleets = state_dir.join("fleets");
    let mut out = Vec::new();
    let Ok(fleet_iter) = std::fs::read_dir(&fleets) else {
        return out;
    };
    for fleet_entry in fleet_iter.flatten() {
        let Ok(ft) = fleet_entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let mol_dir = fleet_entry.path().join("molecules");
        let Ok(mol_iter) = std::fs::read_dir(&mol_dir) else {
            continue;
        };
        for mol_entry in mol_iter.flatten() {
            let dir = mol_entry.path();
            let state_file = dir.join("state.json");
            if !state_file.is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&state_file) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            let Some(id) = v.get("id").and_then(Value::as_str) else {
                continue;
            };
            let status = v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_lowercase();
            let topic = v
                .get("variables")
                .and_then(|vars| vars.get("topic"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let assigned_worker = v
                .get("assigned_worker")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let updated_at = v
                .get("updated_at")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let archived = v.get("archived").and_then(Value::as_bool).unwrap_or(false);
            out.push(MoleculeSnapshot {
                id: id.to_owned(),
                status,
                topic,
                assigned_worker,
                updated_at,
                archived,
                dir,
            });
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

/// City scale — one line per galaxy with molecule counts.
fn render_city(galaxies: &[GalaxySnapshot]) -> String {
    let mut out = String::new();
    out.push_str("COSMON CLUSTER — CITY VIEW\n");
    out.push_str("──────────────────────────\n");
    if galaxies.is_empty() {
        out.push_str("(no galaxies found)\n");
        return out;
    }
    let name_width = galaxies
        .iter()
        .map(|g| g.name.len())
        .max()
        .unwrap_or(8)
        .max(8);
    out.push_str(&format!(
        "  {:<name_width$}  {:>4}  {:>7}  {:>7}  {:>9}  {:>9}\n",
        "GALAXY", "WKRS", "PEND", "RUN", "COMPLETED", "COLLAPSED"
    ));
    out.push_str(&format!("  {}\n", "─".repeat(name_width + 50)));
    for g in galaxies {
        let mut pending = 0usize;
        let mut running = 0usize;
        let mut completed = 0usize;
        let mut collapsed = 0usize;
        for m in &g.molecules {
            if m.archived {
                continue;
            }
            match m.status.as_str() {
                "pending" | "queued" => pending += 1,
                "running" => running += 1,
                "completed" => completed += 1,
                "collapsed" => collapsed += 1,
                _ => {}
            }
        }
        out.push_str(&format!(
            "  {:<name_width$}  {:>4}  {:>7}  {:>7}  {:>9}  {:>9}\n",
            g.name,
            g.workers.len(),
            pending,
            running,
            completed,
            collapsed
        ));
    }
    out
}

/// Building scale — per-galaxy block with workers + molecule status groups.
fn render_building(galaxies: &[GalaxySnapshot], focus: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("COSMON CLUSTER — BUILDING VIEW\n");
    out.push_str("──────────────────────────────\n");
    let filtered: Vec<&GalaxySnapshot> = match focus {
        Some(f) => galaxies.iter().filter(|g| g.name == f).collect(),
        None => galaxies.iter().collect(),
    };
    if filtered.is_empty() {
        out.push_str("(no galaxies match focus)\n");
        return out;
    }
    for (i, g) in filtered.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_galaxy_block(g, &mut out);
    }
    out
}

fn render_galaxy_block(g: &GalaxySnapshot, out: &mut String) {
    out.push_str(&format!(
        "▸ {}  ({} workers, {} molecules)\n",
        g.name,
        g.workers.len(),
        g.molecules.iter().filter(|m| !m.archived).count()
    ));
    if !g.workers.is_empty() {
        out.push_str("  Workers:\n");
        for w in &g.workers {
            let mol = w.current.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "    · {:<12} {:<14} desired={} status={} mol={}\n",
                w.name, w.role, w.desired, w.status, mol
            ));
        }
    }
    let mut by_status: std::collections::BTreeMap<&str, Vec<&MoleculeSnapshot>> =
        std::collections::BTreeMap::new();
    for m in &g.molecules {
        if m.archived {
            continue;
        }
        by_status.entry(m.status.as_str()).or_default().push(m);
    }
    if by_status.is_empty() {
        return;
    }
    let ordering = [
        "running",
        "queued",
        "pending",
        "frozen",
        "completed",
        "collapsed",
    ];
    let mut seen = std::collections::HashSet::new();
    for status in ordering {
        if let Some(rows) = by_status.get(status) {
            write_molecule_group(status, rows, out);
            seen.insert(status);
        }
    }
    for (status, rows) in by_status.iter() {
        if seen.contains(status) {
            continue;
        }
        write_molecule_group(status, rows, out);
    }
}

fn write_molecule_group(status: &str, rows: &[&MoleculeSnapshot], out: &mut String) {
    out.push_str(&format!(
        "  {:<10} ({} molecules)\n",
        status.to_uppercase(),
        rows.len()
    ));
    for m in rows.iter().take(20) {
        let topic = m
            .topic
            .as_deref()
            .map(|t| truncate(t, 50))
            .unwrap_or_else(|| "-".to_owned());
        let worker = m.assigned_worker.as_deref().unwrap_or("-");
        out.push_str(&format!("    · {:<22} {:<12} {}\n", m.id, worker, topic));
    }
    if rows.len() > 20 {
        out.push_str(&format!("    … {} more\n", rows.len() - 20));
    }
}

/// Skin scale — drop into the focus molecule's artefact tree.
fn render_skin(galaxies: &[GalaxySnapshot], focus: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("COSMON CLUSTER — SKIN VIEW\n");
    out.push_str("──────────────────────────\n");
    let Some(id) = focus else {
        out.push_str("(skin scale requires ?focus=<molecule_id>)\n");
        return out;
    };
    for g in galaxies {
        if let Some(m) = g.molecules.iter().find(|m| m.id == id) {
            out.push_str(&format!("{}  ({})\n", m.id, g.name));
            out.push_str(&format!(
                "status={}  worker={}  updated_at={}\n\n",
                m.status,
                m.assigned_worker.as_deref().unwrap_or("-"),
                m.updated_at.as_deref().unwrap_or("-"),
            ));
            if let Some(topic) = &m.topic {
                out.push_str("TOPIC\n─────\n");
                out.push_str(topic);
                out.push_str("\n\n");
            }
            push_preview(&m.dir.join("briefing.md"), "BRIEFING.md", &mut out);
            push_preview(&m.dir.join("synthesis.md"), "SYNTHESIS.md", &mut out);
            return out;
        }
    }
    out.push_str(&format!(
        "(molecule `{id}` not found in any scanned galaxy)\n"
    ));
    out
}

fn push_preview(path: &Path, heading: &str, out: &mut String) {
    let Ok(mut content) = std::fs::read_to_string(path) else {
        return;
    };
    if content.len() > PREVIEW_MAX_BYTES {
        content.truncate(PREVIEW_MAX_BYTES);
        content.push_str("\n…[truncated]");
    }
    out.push_str(heading);
    out.push('\n');
    out.push_str(&"─".repeat(heading.chars().count()));
    out.push('\n');
    out.push_str(&content);
    out.push_str("\n\n");
}

fn truncate(s: &str, max: usize) -> String {
    let flat = s.replace('\n', " ");
    let flat = flat.trim();
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        return flat.to_owned();
    }
    let cut: String = chars.into_iter().take(max).collect();
    format!("{cut}…")
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_mol(
        root: &Path,
        galaxy: &str,
        id: &str,
        status: &str,
        updated: &str,
        briefing: Option<&str>,
    ) {
        let dir = root
            .join(galaxy)
            .join(".cosmon/state/fleets/default/molecules")
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let j = serde_json::json!({
            "id": id,
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": status,
            "variables": {"topic": format!("{id} topic line")},
            "created_at": updated,
            "updated_at": updated,
            "assigned_worker": null,
            "archived": false,
        });
        std::fs::write(dir.join("state.json"), j.to_string()).unwrap();
        if let Some(b) = briefing {
            std::fs::write(dir.join("briefing.md"), b).unwrap();
        }
    }

    fn write_fleet(root: &Path, galaxy: &str) {
        let state = root.join(galaxy).join(".cosmon/state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("fleet.json"),
            serde_json::json!({
                "workers": {
                    "ruby": {
                        "role": "implementation",
                        "desired": "running",
                        "status": "active",
                        "current_molecule": "task-a",
                        "updated_at": "2026-04-23T10:00:00Z",
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn scale_from_query_prefers_explicit_string() {
        assert_eq!(Scale::from_query(Some("city"), None), Scale::City);
        assert_eq!(Scale::from_query(Some("building"), None), Scale::Building);
        assert_eq!(Scale::from_query(Some("skin"), None), Scale::Skin);
        assert_eq!(Scale::from_query(Some("bogus"), None), Scale::Building);
    }

    #[test]
    fn scale_from_zoom_falls_into_discrete_levels() {
        assert_eq!(Scale::from_query(None, Some(0.0)), Scale::City);
        assert_eq!(Scale::from_query(None, Some(1.0)), Scale::Building);
        assert_eq!(Scale::from_query(None, Some(1.8)), Scale::Skin);
        assert_eq!(Scale::from_query(None, None), Scale::Building);
    }

    #[test]
    fn city_render_lists_every_galaxy() {
        let tmp = TempDir::new().unwrap();
        write_fleet(tmp.path(), "cosmon");
        write_mol(
            tmp.path(),
            "cosmon",
            "task-a",
            "running",
            "2026-04-22T12:00:00Z",
            None,
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "task-b",
            "pending",
            "2026-04-22T10:00:00Z",
            None,
        );
        write_mol(
            tmp.path(),
            "mailroom",
            "task-c",
            "completed",
            "2026-04-22T09:00:00Z",
            None,
        );
        std::fs::create_dir_all(tmp.path().join("mailroom/.cosmon")).unwrap();

        let mut q = PeekQuery::default();
        q.scale = Some("city".into());
        let out = aggregate_peek(tmp.path(), &q).unwrap();
        assert_eq!(out["scale"], "city");
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("COSMON CLUSTER — CITY VIEW"));
        assert!(text.contains("cosmon"));
        assert!(text.contains("mailroom"));
    }

    #[test]
    fn building_render_includes_workers_and_groups() {
        let tmp = TempDir::new().unwrap();
        write_fleet(tmp.path(), "cosmon");
        write_mol(
            tmp.path(),
            "cosmon",
            "task-running",
            "running",
            "2026-04-22T12:00:00Z",
            None,
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "task-pending",
            "pending",
            "2026-04-22T11:00:00Z",
            None,
        );

        let out = aggregate_peek(tmp.path(), &PeekQuery::default()).unwrap();
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("▸ cosmon"));
        assert!(text.contains("ruby"));
        assert!(text.contains("RUNNING"));
        assert!(text.contains("PENDING"));
        assert!(text.contains("task-running"));
    }

    #[test]
    fn skin_render_requires_focus_and_finds_molecule() {
        let tmp = TempDir::new().unwrap();
        write_fleet(tmp.path(), "cosmon");
        write_mol(
            tmp.path(),
            "cosmon",
            "task-focus",
            "running",
            "2026-04-22T12:00:00Z",
            Some("### briefing body\n\nstep one\n"),
        );

        let mut q = PeekQuery::default();
        q.scale = Some("skin".into());
        let out_no_focus = aggregate_peek(tmp.path(), &q).unwrap();
        assert!(out_no_focus["text"]
            .as_str()
            .unwrap()
            .contains("requires ?focus="));

        q.focus = Some("task-focus".into());
        let out = aggregate_peek(tmp.path(), &q).unwrap();
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("task-focus"));
        assert!(text.contains("BRIEFING.md"));
        assert!(text.contains("step one"));
    }

    #[test]
    fn peek_respects_galaxies_allowlist() {
        let tmp = TempDir::new().unwrap();
        for name in ["cosmon", "mailroom"] {
            std::fs::create_dir_all(tmp.path().join(name).join(".cosmon")).unwrap();
        }
        let mut q = PeekQuery::default();
        q.scale = Some("city".into());
        q.galaxies = Some("cosmon".into());
        let out = aggregate_peek(tmp.path(), &q).unwrap();
        let names: Vec<&str> = out["galaxies_scanned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["cosmon"]);
    }
}
