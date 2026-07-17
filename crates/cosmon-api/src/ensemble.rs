// SPDX-License-Identifier: AGPL-3.0-only

//! `/ensemble` — cluster-wide workers + molecules snapshot.
//!
//! Where `/galaxies` returns one summary row per galaxy and `/inbox`
//! returns a flat list of molecules for a single galaxy, `/ensemble`
//! folds both into a complete "what is the cluster currently holding"
//! view: every worker of every galaxy, plus molecules grouped by status
//! (pending / running / completed / collapsed) with per-status counts
//! and a capped sample list per group.
//!
//! # Design
//!
//! The aggregator walks every `<galaxies_root>/<galaxy>/.cosmon/state/`
//! tree under [`AppState::galaxies_root`]. For each galaxy it reads two
//! independent sources:
//!
//! - `fleet.json` — active workers (status, role, current molecule).
//! - `fleets/*/molecules/*/state.json` — every molecule on disk.
//!
//! Output is deterministic: galaxies sorted by name, workers by name,
//! molecule sample rows by `updated_at` descending. The sample lists
//! are capped at [`MAX_MOLECULES_PER_GROUP`] — the full-list counter
//! (`total`) always reflects the true total so the client can render
//! "top 50 of 187" without a second request.
//!
//! # Query parameters
//!
//! - `scope=local` (default) — scan the full galaxies root. Reserved
//!   keyword for future `cloud` / `peer` scopes.
//! - `galaxies=cosmon,mailroom` — optional allowlist.
//! - `statuses=running,pending` — optional status filter (default: all
//!   open-ish statuses, which is pending+queued+running+frozen).
//! - `include_archived=false` (default) — when true, `archived: true`
//!   molecules are counted and sampled.
//! - `limit_per_group=50` — override the per-status sample cap.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::instrumentation::InvocationMode;
use crate::{ApiError, AppState};

/// Default sample cap per status group. The full count is reported
/// separately so the client can paginate if it ever needs to.
pub const MAX_MOLECULES_PER_GROUP: usize = 50;

/// Hard cap on `limit_per_group` — defends the server against a caller
/// asking for `?limit_per_group=999999`.
pub const MAX_LIMIT_PER_GROUP: usize = 500;

/// `GET /ensemble` query string.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct EnsembleQuery {
    /// Reserved for future cross-host scoping. Today only `local`
    /// (the default) is honored; unknown values fall back to `local`
    /// rather than erroring so clients that pin `?scope=local` keep
    /// working.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional comma-separated galaxy allowlist.
    #[serde(default)]
    pub galaxies: Option<String>,
    /// Optional comma-separated status filter applied to the molecule
    /// scan. Defaults to "all" when omitted (see
    /// [`EnsembleQuery::status_filter`]).
    #[serde(default)]
    pub statuses: Option<String>,
    /// When true, molecules with `archived == true` are included.
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Override the default [`MAX_MOLECULES_PER_GROUP`] sample cap.
    #[serde(default)]
    pub limit_per_group: Option<usize>,
}

impl EnsembleQuery {
    fn status_filter(&self) -> Option<HashSet<String>> {
        let raw = self.statuses.as_deref()?.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
            return None;
        }
        let out: HashSet<String> = raw
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

    fn galaxy_allowlist(&self) -> Option<HashSet<String>> {
        let raw = self.galaxies.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        let out: HashSet<String> = raw
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

    fn sample_cap(&self) -> usize {
        self.limit_per_group
            .unwrap_or(MAX_MOLECULES_PER_GROUP)
            .clamp(1, MAX_LIMIT_PER_GROUP)
    }
}

/// One worker row flattened across a galaxy.
#[derive(Debug, Serialize)]
struct WorkerRow {
    name: String,
    galaxy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    molecule_id: Option<String>,
    /// True when the worker's desired state is `running` (the "is this
    /// worker supposed to be doing something?" bit).
    live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
}

/// One molecule row sampled inside a per-status bucket.
#[derive(Debug, Serialize)]
struct MoleculeRow {
    id: String,
    kind: String,
    galaxy: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    formula: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    assigned_worker: Option<String>,
}

/// One status group — total count + capped sample.
#[derive(Debug, Serialize)]
struct StatusGroup {
    status: String,
    total: usize,
    sample: Vec<MoleculeRow>,
}

/// Per-galaxy roll-up — workers + molecules grouped by status.
#[derive(Debug, Serialize)]
struct GalaxyBlock {
    name: String,
    path: String,
    workers: Vec<WorkerRow>,
    worker_count: usize,
    molecule_groups: Vec<StatusGroup>,
    total_molecules: usize,
}

/// `GET /ensemble` handler — aggregates on a blocking thread.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateRead`]. Like
/// `/inbox`, the handler walks `<state>/fleets/*` directly. The latency
/// recorded by the T1 instrumentation is the wall-clock cost of the
/// directory walk + JSON parse, which scales with the number of
/// molecules — a useful baseline for sizing the in-process path.
pub(crate) async fn get_ensemble(
    State(state): State<Arc<AppState>>,
    Query(q): Query<EnsembleQuery>,
) -> Result<Json<Value>, ApiError> {
    let started = std::time::Instant::now();
    let galaxies_root = state.galaxies_root.clone();
    let result = tokio::task::spawn_blocking(move || aggregate_ensemble(&galaxies_root, &q))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("ensemble aggregation panicked: {e}"),
            )
        });
    crate::instrumentation::emit(
        &state,
        crate::instrumentation::EngineCallEntered {
            verb: "<scan-ensemble>".to_owned(),
            args_hash: 0,
            caller: "/ensemble".to_owned(),
            mode: InvocationMode::InProcessStateRead,
            latency_ms: crate::instrumentation::elapsed_ms(started),
            stdout_bytes: 0,
            timestamp: crate::instrumentation::now_iso(),
        },
    );
    let val = result??;
    Ok(Json(val))
}

/// Aggregate the full cluster snapshot.
///
/// Sync + reentrant + read-only. The CLI shim (`cs ensemble --all`)
/// calls this directly without going through HTTP.
pub fn aggregate_ensemble(galaxies_root: &Path, q: &EnsembleQuery) -> Result<Value, ApiError> {
    let allowlist = q.galaxy_allowlist();
    let status_filter = q.status_filter();
    let include_archived = q.include_archived.unwrap_or(false);
    let sample_cap = q.sample_cap();

    let galaxies = discover_galaxies(galaxies_root, allowlist.as_ref()).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scan galaxies under {}: {e}", galaxies_root.display()),
        )
    })?;

    let mut blocks: Vec<GalaxyBlock> = Vec::new();
    let mut grand_workers = 0usize;
    let mut grand_molecules = 0usize;

    for g in &galaxies {
        let workers = collect_workers(g);
        let worker_count = workers.len();
        grand_workers += worker_count;

        let (groups, total) =
            collect_molecule_groups(g, status_filter.as_ref(), include_archived, sample_cap);
        grand_molecules += total;

        blocks.push(GalaxyBlock {
            name: g.name.clone(),
            path: g.root.to_string_lossy().into_owned(),
            workers,
            worker_count,
            molecule_groups: groups,
            total_molecules: total,
        });
    }

    Ok(serde_json::json!({
        "scope": q.scope.as_deref().unwrap_or("local"),
        "galaxies_root": galaxies_root.to_string_lossy(),
        "galaxies_scanned": galaxies.iter().map(|g| g.name.clone()).collect::<Vec<_>>(),
        "galaxies": blocks,
        "totals": {
            "galaxies": blocks.len(),
            "workers": grand_workers,
            "molecules": grand_molecules,
        },
        "sample_cap": sample_cap,
    }))
}

#[derive(Debug, Clone)]
struct GalaxyHandle {
    name: String,
    root: PathBuf,
}

impl GalaxyHandle {
    fn state_dir(&self) -> PathBuf {
        self.root.join(".cosmon/state")
    }
}

fn discover_galaxies(
    root: &Path,
    allowlist: Option<&HashSet<String>>,
) -> std::io::Result<Vec<GalaxyHandle>> {
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
        if let Some(allow) = allowlist {
            if !allow.contains(&name) {
                continue;
            }
        }
        out.push(GalaxyHandle { name, root: path });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn collect_workers(g: &GalaxyHandle) -> Vec<WorkerRow> {
    let fleet_path = g.state_dir().join("fleet.json");
    let Ok(content) = std::fs::read_to_string(&fleet_path) else {
        return Vec::new();
    };
    let Ok(fleet) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };
    let Some(workers_obj) = fleet.get("workers").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<WorkerRow> = workers_obj
        .iter()
        .map(|(name, w)| {
            let desired = w
                .get("desired")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            WorkerRow {
                name: name.clone(),
                galaxy: g.name.clone(),
                role: w.get("role").and_then(Value::as_str).map(str::to_owned),
                status: w.get("status").and_then(Value::as_str).map(str::to_owned),
                molecule_id: w
                    .get("current_molecule")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                live: desired == "running",
                updated_at: w
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn collect_molecule_groups(
    g: &GalaxyHandle,
    status_filter: Option<&HashSet<String>>,
    include_archived: bool,
    sample_cap: usize,
) -> (Vec<StatusGroup>, usize) {
    let fleets = g.state_dir().join("fleets");
    let Ok(fleet_iter) = std::fs::read_dir(&fleets) else {
        return (Vec::new(), 0);
    };
    let mut by_status: std::collections::BTreeMap<String, Vec<MoleculeRow>> =
        std::collections::BTreeMap::new();
    let mut total = 0usize;
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
            let state_file = mol_entry.path().join("state.json");
            if !state_file.is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&state_file) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            if !include_archived && v.get("archived").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let Some(row) = molecule_row(&g.name, &v) else {
                continue;
            };
            if let Some(allow) = status_filter {
                if !allow.contains(&row.status) {
                    continue;
                }
            }
            total += 1;
            by_status.entry(row.status.clone()).or_default().push(row);
        }
    }
    let mut groups: Vec<StatusGroup> = by_status
        .into_iter()
        .map(|(status, mut rows)| {
            rows.sort_by_key(|x| std::cmp::Reverse(x.updated_at.clone()));
            let total_rows = rows.len();
            rows.truncate(sample_cap);
            StatusGroup {
                status,
                total: total_rows,
                sample: rows,
            }
        })
        .collect();
    // Status display order prefers "active" statuses first.
    groups.sort_by_key(|g| status_sort_key(&g.status));
    (groups, total)
}

fn status_sort_key(s: &str) -> u8 {
    match s {
        "running" => 0,
        "queued" => 1,
        "pending" => 2,
        "frozen" => 3,
        "completed" => 4,
        "collapsed" => 5,
        _ => 9,
    }
}

fn molecule_row(galaxy: &str, v: &Value) -> Option<MoleculeRow> {
    let id = v.get("id")?.as_str()?.to_owned();
    let status = v.get("status")?.as_str()?.to_lowercase();
    let kind = kind_from_id(&id);
    let formula = v
        .get("formula_id")
        .and_then(|f| f.as_str())
        .unwrap_or_default()
        .to_owned();
    let updated_at = v
        .get("updated_at")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let topic = v
        .get("variables")
        .and_then(|vars| vars.get("topic"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let tags = v
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let assigned_worker = v
        .get("assigned_worker")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(MoleculeRow {
        id,
        kind,
        galaxy: galaxy.to_owned(),
        status,
        topic,
        tags,
        updated_at,
        formula,
        assigned_worker,
    })
}

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
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_mol(
        root: &Path,
        galaxy: &str,
        fleet: &str,
        id: &str,
        status: &str,
        updated_at: &str,
        archived: bool,
    ) {
        let dir = root
            .join(galaxy)
            .join(".cosmon/state/fleets")
            .join(fleet)
            .join("molecules")
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let j = serde_json::json!({
            "id": id,
            "fleet_id": fleet,
            "formula_id": "task-work",
            "status": status,
            "archived": archived,
            "variables": {"topic": format!("{id}-topic")},
            "tags": ["temp:hot"],
            "created_at": updated_at,
            "updated_at": updated_at,
            "assigned_worker": null,
        });
        std::fs::write(dir.join("state.json"), j.to_string()).unwrap();
    }

    fn write_fleet(root: &Path, galaxy: &str, worker: &str, desired: &str) {
        let state = root.join(galaxy).join(".cosmon/state");
        std::fs::create_dir_all(&state).unwrap();
        let j = serde_json::json!({
            "workers": {
                worker: {
                    "role": "implementation",
                    "desired": desired,
                    "status": "active",
                    "current_molecule": "task-20260423-abcd",
                    "updated_at": "2026-04-23T10:00:00Z",
                }
            }
        });
        std::fs::write(state.join("fleet.json"), j.to_string()).unwrap();
    }

    #[test]
    fn ensemble_groups_molecules_by_status_and_caps_sample() {
        let tmp = TempDir::new().unwrap();
        write_fleet(tmp.path(), "cosmon", "ruby", "running");
        write_mol(
            tmp.path(),
            "cosmon",
            "default",
            "task-1",
            "pending",
            "2026-04-22T10:00:00Z",
            false,
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "default",
            "task-2",
            "running",
            "2026-04-22T12:00:00Z",
            false,
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "default",
            "task-3",
            "completed",
            "2026-04-22T11:00:00Z",
            false,
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "default",
            "task-archived",
            "completed",
            "2026-04-22T09:00:00Z",
            true,
        );

        let q = EnsembleQuery::default();
        let out = aggregate_ensemble(tmp.path(), &q).unwrap();
        let galaxies = out["galaxies"].as_array().unwrap();
        assert_eq!(galaxies.len(), 1);
        let cosmon = &galaxies[0];
        assert_eq!(cosmon["name"], "cosmon");
        assert_eq!(cosmon["worker_count"], 1);
        assert_eq!(cosmon["workers"][0]["live"], true);
        assert_eq!(cosmon["workers"][0]["name"], "ruby");

        let groups = cosmon["molecule_groups"].as_array().unwrap();
        // archived excluded by default, so 3 rows split across 3 statuses.
        assert_eq!(groups.len(), 3);
        let running = groups
            .iter()
            .find(|g| g["status"] == "running")
            .expect("running group");
        assert_eq!(running["total"], 1);
        assert_eq!(running["sample"][0]["id"], "task-2");
        assert_eq!(cosmon["total_molecules"], 3);
    }

    #[test]
    fn ensemble_respects_status_filter() {
        let tmp = TempDir::new().unwrap();
        for (id, status, updated) in [
            ("task-a", "pending", "2026-04-22T10:00:00Z"),
            ("task-b", "running", "2026-04-22T11:00:00Z"),
            ("task-c", "completed", "2026-04-22T12:00:00Z"),
        ] {
            write_mol(tmp.path(), "cosmon", "default", id, status, updated, false);
        }
        let mut q = EnsembleQuery::default();
        q.statuses = Some("running".to_owned());
        let out = aggregate_ensemble(tmp.path(), &q).unwrap();
        let cosmon = &out["galaxies"][0];
        let groups = cosmon["molecule_groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["status"], "running");
        assert_eq!(groups[0]["total"], 1);
    }

    #[test]
    fn ensemble_galaxies_allowlist_excludes_others() {
        let tmp = TempDir::new().unwrap();
        for name in ["cosmon", "mailroom", "workshop"] {
            std::fs::create_dir_all(tmp.path().join(name).join(".cosmon")).unwrap();
        }
        let mut q = EnsembleQuery::default();
        q.galaxies = Some("cosmon,mailroom".to_owned());
        let out = aggregate_ensemble(tmp.path(), &q).unwrap();
        let names: Vec<&str> = out["galaxies_scanned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["cosmon", "mailroom"]);
    }

    #[test]
    fn ensemble_include_archived_flag() {
        let tmp = TempDir::new().unwrap();
        write_mol(
            tmp.path(),
            "cosmon",
            "default",
            "task-open",
            "pending",
            "2026-04-22T10:00:00Z",
            false,
        );
        write_mol(
            tmp.path(),
            "cosmon",
            "default",
            "task-done",
            "completed",
            "2026-04-22T09:00:00Z",
            true,
        );
        let q = EnsembleQuery::default();
        let out = aggregate_ensemble(tmp.path(), &q).unwrap();
        assert_eq!(out["galaxies"][0]["total_molecules"], 1);

        let mut q2 = EnsembleQuery::default();
        q2.include_archived = Some(true);
        let out2 = aggregate_ensemble(tmp.path(), &q2).unwrap();
        assert_eq!(out2["galaxies"][0]["total_molecules"], 2);
    }

    #[test]
    fn ensemble_sample_cap_is_bounded() {
        let tmp = TempDir::new().unwrap();
        for i in 0..5 {
            let id = format!("task-{i:04}");
            let ts = format!("2026-04-22T10:0{i}:00Z");
            write_mol(tmp.path(), "cosmon", "default", &id, "pending", &ts, false);
        }
        let mut q = EnsembleQuery::default();
        q.limit_per_group = Some(2);
        let out = aggregate_ensemble(tmp.path(), &q).unwrap();
        let groups = out["galaxies"][0]["molecule_groups"].as_array().unwrap();
        let pending = groups.iter().find(|g| g["status"] == "pending").unwrap();
        assert_eq!(pending["total"], 5);
        assert_eq!(pending["sample"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn sample_cap_clamps_high_and_low_input() {
        let mut q = EnsembleQuery::default();
        q.limit_per_group = Some(0);
        assert_eq!(q.sample_cap(), 1, "clamps zero up to the 1-minimum");
        q.limit_per_group = Some(10_000);
        assert_eq!(q.sample_cap(), MAX_LIMIT_PER_GROUP, "clamps to hard cap");
    }

    #[test]
    fn kind_from_id_agrees_with_inbox_module() {
        assert_eq!(kind_from_id("task-20260422-abcd"), "task");
        assert_eq!(kind_from_id("delib-20260422-f6d6"), "deliberation");
        assert_eq!(kind_from_id("spark-20260422-abcd"), "spark");
        assert_eq!(kind_from_id("custom-prefix-no-map"), "custom");
    }
}
