// SPDX-License-Identifier: AGPL-3.0-only

//! `/inbox` — pending/running molecules across every fleet, read
//! directly from `<state>/fleets/*/molecules/*/state.json`.
//!
//! The handler deliberately does **not** shell out to `cs observe` for
//! the list view: the state-on-disk is the source of truth, parsing
//! the JSON ourselves avoids a shell-out per molecule, and `cs observe
//! --json` (no id) omits the tags/topic/created_at fields the iOS
//! pilot needs to render rows. Single-molecule detail requests can
//! still go through `cs observe <id>` in a later revision.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::instrumentation::{record_in_process, InvocationMode};
use crate::{ApiError, AppState};

/// Default status filter: what a pilot surface cares about by default.
const DEFAULT_STATUS: &str = "pending,running";

/// `GET /inbox` query string.
#[derive(Debug, Deserialize)]
pub(crate) struct InboxQuery {
    /// Comma-separated status filter (e.g. `pending,running`). Empty or
    /// `all` disables the filter and returns every molecule on disk.
    #[serde(default)]
    pub status: Option<String>,
    /// Optional cap on the number of molecules returned.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Row shape matching the schema in the molecule spec: enough to render
/// a list tile on the iOS pilot without a second round-trip.
#[derive(Debug, Serialize)]
struct MoleculeSummary {
    id: String,
    kind: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    tags: Vec<String>,
    created_at: String,
    updated_at: String,
    formula: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    assigned_worker: Option<String>,
}

/// `GET /inbox?status=pending,running` — list molecules matching the filter.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateRead`]. The
/// handler reads `<state>/fleets/*/molecules/*/state.json` directly
/// without going through `cs observe` (see module-level comment for
/// why). This is a key baseline for the T1 mini-rapport: the latency
/// distribution here, compared to the `tackle` / `tag` shell-outs,
/// quantifies the cost of the subprocess boundary.
pub(crate) async fn list_inbox(
    State(state): State<Arc<AppState>>,
    Query(q): Query<InboxQuery>,
) -> Result<Json<Value>, ApiError> {
    record_in_process(
        &state,
        "/inbox",
        "<scan-inbox>",
        InvocationMode::InProcessStateRead,
        || {
            let status_filter = parse_status_filter(q.status.as_deref());
            let state_dir = state.resolve_cosmon_state_dir();
            let mut molecules =
                scan_molecules(&state_dir, status_filter.as_deref()).map_err(|e| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("scan molecules under {}: {e}", state_dir.display()),
                    )
                })?;
            // Most recently touched first — useful default for the iOS list.
            molecules.sort_by_key(|x| std::cmp::Reverse(x.updated_at.clone()));
            if let Some(limit) = q.limit {
                molecules.truncate(limit);
            }
            Ok(Json(serde_json::json!({ "molecules": molecules })))
        },
    )
}

/// Translate the comma-separated query param into either a filter list
/// (lower-case statuses) or `None` meaning "don't filter". The "all"
/// sentinel lets a pilot ask for the full list without listing every
/// status variant.
fn parse_status_filter(raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw.unwrap_or(DEFAULT_STATUS).trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
        return None;
    }
    let out: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Iterate `<state_dir>/fleets/*/molecules/*/state.json`. Corrupt or
/// truncated files are skipped silently so one half-written molecule
/// does not break the whole inbox.
fn scan_molecules(
    state_dir: &Path,
    statuses: Option<&[String]>,
) -> std::io::Result<Vec<MoleculeSummary>> {
    let fleets = state_dir.join("fleets");
    let mut out = Vec::new();
    if !fleets.exists() {
        return Ok(out);
    }
    for fleet_entry in std::fs::read_dir(&fleets)? {
        let fleet_entry = fleet_entry?;
        if !fleet_entry.file_type()?.is_dir() {
            continue;
        }
        let mol_dir = fleet_entry.path().join("molecules");
        let Ok(iter) = std::fs::read_dir(&mol_dir) else {
            continue;
        };
        for mol_entry in iter.flatten() {
            let state_file = mol_entry.path().join("state.json");
            if !state_file.is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&state_file) else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            let Some(summary) = molecule_summary_from_value(&val) else {
                continue;
            };
            if let Some(list) = statuses {
                if !list.iter().any(|s| s == &summary.status) {
                    continue;
                }
            }
            out.push(summary);
        }
    }
    Ok(out)
}

/// Translate one `state.json` document into the wire-level summary.
/// Kind is derived from the id prefix (`task-`, `delib-`, `idea-`, …)
/// because the `kind` field is not persisted in `state.json` today;
/// future work may attach it explicitly.
fn molecule_summary_from_value(v: &Value) -> Option<MoleculeSummary> {
    let id = v.get("id")?.as_str()?.to_owned();
    let status = v.get("status")?.as_str()?.to_lowercase();
    let formula = v
        .get("formula_id")
        .and_then(|f| f.as_str())
        .unwrap_or_default()
        .to_owned();
    let created_at = v
        .get("created_at")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned();
    let updated_at = v
        .get("updated_at")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned();
    let topic = v
        .get("variables")
        .and_then(|vars| vars.get("topic"))
        .and_then(|t| t.as_str())
        .map(str::to_owned);
    let tags = v
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tt| tt.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let assigned_worker = v
        .get("assigned_worker")
        .and_then(|s| s.as_str())
        .map(str::to_owned);
    let kind = kind_from_id(&id);
    Some(MoleculeSummary {
        id,
        kind,
        status,
        topic,
        tags,
        created_at,
        updated_at,
        formula,
        assigned_worker,
    })
}

/// Map the molecule id prefix onto the canonical kind name used in the
/// CLI vocabulary (see [`cosmon_core::kind::MoleculeKind`]). The map is
/// intentionally small and closed — anything unknown returns the raw
/// prefix so the caller still has a usable label.
fn kind_from_id(id: &str) -> String {
    let prefix = id.split_once('-').map_or(id, |(p, _)| p);
    match prefix {
        "task" => "task",
        "idea" => "idea",
        "issue" => "issue",
        "decision" => "decision",
        "signal" => "signal",
        "delib" => "deliberation",
        "const" => "constellation",
        "spark" => "spark",
        "absorb" => "absorb",
        "chronlint" => "chronlint",
        other => other,
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_id_maps_known_prefixes() {
        assert_eq!(kind_from_id("task-20260422-db9f"), "task");
        assert_eq!(kind_from_id("delib-20260422-f6d6"), "deliberation");
        assert_eq!(kind_from_id("const-20260422-4edc"), "constellation");
        assert_eq!(kind_from_id("spark-20260422-68d4"), "spark");
        assert_eq!(kind_from_id("idea-42"), "idea");
        assert_eq!(kind_from_id("unknown-42"), "unknown");
        assert_eq!(kind_from_id("noprefix"), "noprefix");
    }

    #[test]
    fn parse_status_filter_default_is_pending_and_running() {
        let f = parse_status_filter(None).expect("filter");
        assert!(f.iter().any(|s| s == "pending"));
        assert!(f.iter().any(|s| s == "running"));
    }

    #[test]
    fn parse_status_filter_all_disables_filter() {
        assert!(parse_status_filter(Some("all")).is_none());
        assert!(parse_status_filter(Some("")).is_none());
        assert!(parse_status_filter(Some("  ")).is_none());
    }

    #[test]
    fn molecule_summary_extracts_topic_and_tags() {
        let v: Value = serde_json::json!({
            "id": "task-42-abcd",
            "status": "Running",
            "formula_id": "task-work",
            "variables": {"topic": "Ship it"},
            "tags": ["temp:hot", "ux"],
            "created_at": "2026-04-22T10:00:00Z",
            "updated_at": "2026-04-22T11:00:00Z",
            "assigned_worker": "w-1",
        });
        let s = molecule_summary_from_value(&v).expect("summary");
        assert_eq!(s.id, "task-42-abcd");
        assert_eq!(s.kind, "task");
        assert_eq!(s.status, "running");
        assert_eq!(s.topic.as_deref(), Some("Ship it"));
        assert_eq!(s.tags, vec!["temp:hot", "ux"]);
        assert_eq!(s.formula, "task-work");
        assert_eq!(s.assigned_worker.as_deref(), Some("w-1"));
    }

    #[test]
    fn scan_molecules_respects_status_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mol_dir = tmp.path().join("fleets/default/molecules");
        std::fs::create_dir_all(&mol_dir).unwrap();
        for (id, status) in [
            ("task-1", "pending"),
            ("task-2", "running"),
            ("task-3", "completed"),
        ] {
            let dir = mol_dir.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let j = serde_json::json!({
                "id": id,
                "status": status,
                "formula_id": "task-work",
                "variables": {"topic": id},
                "tags": [],
                "created_at": "2026-04-22T10:00:00Z",
                "updated_at": "2026-04-22T11:00:00Z",
            });
            std::fs::write(dir.join("state.json"), j.to_string()).unwrap();
        }
        let filter = Some(vec!["pending".to_owned(), "running".to_owned()]);
        let out = scan_molecules(tmp.path(), filter.as_deref()).unwrap();
        let ids: Vec<&str> = out.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"task-1"));
        assert!(ids.contains(&"task-2"));
        assert!(!ids.contains(&"task-3"));
    }
}
