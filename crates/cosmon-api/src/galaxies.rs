// SPDX-License-Identifier: AGPL-3.0-only

//! `/galaxies` — enumerate every cosmon project under a parent root.
//!
//! A "galaxy" is any directory under [`AppState::galaxies_root`] that
//! carries a `.cosmon/` metadata directory. The iOS pilot uses this to
//! build a home screen listing — one tile per galaxy — and to show an
//! at-a-glance pending/running count.

use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use serde_json::Value;

use crate::instrumentation::{record_in_process, InvocationMode};
use crate::{ApiError, AppState};

/// Per-galaxy summary row.
#[derive(Debug, Serialize)]
struct Galaxy {
    /// Directory basename (e.g. `cosmon`, `mailroom`).
    name: String,
    /// Absolute path to the galaxy root (the directory that contains `.cosmon/`).
    path: String,
    /// Count of molecules with `status == "pending"` across all fleets.
    pending_count: usize,
    /// Count of molecules with `status == "running"` across all fleets.
    running_count: usize,
    /// Most recent `updated_at` seen among molecules, as an ISO-8601 string.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity: Option<String>,
}

/// `GET /galaxies` — scan `galaxies_root` for `.cosmon/`-bearing directories.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateRead`]. The
/// scan tallies pending/running counts per galaxy directly from disk;
/// no `cs` subprocess is spawned.
pub(crate) async fn list_galaxies(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, ApiError> {
    record_in_process(
        &state,
        "/galaxies",
        "<scan-galaxies>",
        InvocationMode::InProcessStateRead,
        || {
            let root = state.galaxies_root.clone();
            let galaxies = scan_galaxies(&root).map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("scan galaxies root {}: {e}", root.display()),
                )
            })?;
            Ok(Json(serde_json::json!({
                "galaxies_root": root.to_string_lossy(),
                "galaxies": galaxies,
            })))
        },
    )
}

/// Walk one level of `galaxies_root`; a child is a galaxy iff it is a
/// directory AND carries a `.cosmon/` subdirectory. Non-cosmon folders
/// are silently skipped so the operator can drop arbitrary working
/// directories next to their galaxies without polluting this view.
fn scan_galaxies(root: &Path) -> std::io::Result<Vec<Galaxy>> {
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
        let state_dir = path.join(".cosmon").join("state");
        let (pending, running, last) = count_molecules(&state_dir);
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push(Galaxy {
            name,
            path: path.to_string_lossy().into_owned(),
            pending_count: pending,
            running_count: running,
            last_activity: last,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Walk `<state>/fleets/*/molecules/*/state.json` and tally
/// pending/running counts + the lexicographically-max `updated_at` (ISO
/// strings sort chronologically). Returns zeros when the state dir is
/// absent — a brand-new galaxy reports 0/0 rather than an error.
fn count_molecules(state_dir: &Path) -> (usize, usize, Option<String>) {
    let fleets = state_dir.join("fleets");
    let mut pending = 0;
    let mut running = 0;
    let mut last_activity: Option<String> = None;
    let Ok(fleets_iter) = std::fs::read_dir(&fleets) else {
        return (0, 0, None);
    };
    for fleet_entry in fleets_iter.flatten() {
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
            let state_file = mol_entry.path().join("state.json");
            let Ok(content) = std::fs::read_to_string(&state_file) else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            let status = val
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_lowercase();
            match status.as_str() {
                "pending" => pending += 1,
                "running" => running += 1,
                _ => {}
            }
            if let Some(u) = val.get("updated_at").and_then(|s| s.as_str()) {
                match &last_activity {
                    Some(cur) if cur.as_str() >= u => {}
                    _ => last_activity = Some(u.to_owned()),
                }
            }
        }
    }
    (pending, running, last_activity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_mol(root: &Path, galaxy: &str, id: &str, status: &str, updated_at: &str) {
        let dir = root
            .join(galaxy)
            .join(".cosmon/state/fleets/default/molecules")
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let j = serde_json::json!({
            "id": id,
            "status": status,
            "updated_at": updated_at,
        });
        std::fs::write(dir.join("state.json"), j.to_string()).unwrap();
    }

    #[test]
    fn scan_galaxies_finds_only_cosmon_dirs() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("not-a-galaxy/src")).unwrap();
        write_mol(
            tmp.path(),
            "cosmon",
            "task-1",
            "pending",
            "2026-04-22T10:00:00Z",
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "task-2",
            "running",
            "2026-04-22T12:00:00Z",
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "task-3",
            "completed",
            "2026-04-22T11:00:00Z",
        );
        write_mol(
            tmp.path(),
            "workshop",
            "task-1",
            "pending",
            "2026-04-22T09:00:00Z",
        );
        let galaxies = scan_galaxies(tmp.path()).unwrap();
        let names: Vec<&str> = galaxies.iter().map(|g| g.name.as_str()).collect();
        assert!(names.contains(&"cosmon"));
        assert!(names.contains(&"workshop"));
        assert!(!names.contains(&"not-a-galaxy"));
        let cosmon = galaxies.iter().find(|g| g.name == "cosmon").unwrap();
        assert_eq!(cosmon.pending_count, 1);
        assert_eq!(cosmon.running_count, 1);
        assert_eq!(
            cosmon.last_activity.as_deref(),
            Some("2026-04-22T12:00:00Z")
        );
    }

    #[test]
    fn scan_galaxies_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        assert_eq!(scan_galaxies(&missing).unwrap().len(), 0);
    }
}
