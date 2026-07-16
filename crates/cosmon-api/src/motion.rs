// SPDX-License-Identifier: AGPL-3.0-only

//! `/motion` — "molécules en mouvement" cross-galaxy aggregate.
//!
//! NOTE: `aggregate_motion` is used by the `cs motion` CLI, but the
//! HTTP handler `get_motion` / `MotionQuery` types are not yet wired
//! into the router. The module is kept in-tree and `pub` (from lib.rs)
//! so the CLI can reach `aggregate_motion`; route registration lands in
//! a follow-up.
#![allow(dead_code)]
//!
//! The endpoint tells the operator, in near-real-time, what the whole
//! local cluster is doing: which workers are live, which molecules are
//! advancing step-by-step, what git commits just landed, which whispers
//! and sparks appeared during the last window.
//!
//! # Design
//!
//! Aggregation scans every directory under [`AppState::galaxies_root`]
//! that carries a `.cosmon/` subtree. For each galaxy we read five
//! independent sources — one per response section — and merge them into
//! a flat JSON envelope. The scan is idempotent, filesystem-only, and
//! caps every list to avoid unbounded growth:
//!
//! - Workers: `<galaxy>/.cosmon/state/fleet.json` (+ `fleet.runtime.json`
//!   for the worktree path when present).
//! - Running molecules: `<galaxy>/.cosmon/state/fleets/*/molecules/*/state.json`,
//!   filtered on `status == running`, capped at 50 per galaxy.
//! - Recent git commits: `git log --since=<window>` at the galaxy root,
//!   capped at 20 per galaxy.
//! - Recent whispers: `<galaxy>/.cosmon/whispers/inbox/**/*.md` with
//!   `received_at` newer than the window boundary.
//! - Recent sparks: molecules whose id starts with `spark-` and whose
//!   `created_at` is within the window.
//!
//! # Query parameters
//!
//! - `window=15m` (default) — span considered "recent". Accepted units:
//!   `s`, `m`, `h`. Anything else falls back to 15 minutes.
//! - `galaxies=cosmon,mailroom` — optional allowlist. When omitted
//!   every cosmon-bearing directory is scanned.
//! - `include=workers,molecules,commits,whispers,sparks` — optional
//!   section filter. Omitted keys are still rendered as empty arrays so
//!   the client never has to pattern-match on presence.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApiError, AppState};

/// Maximum number of running molecules returned per galaxy. Keeps the
/// response size bounded even if an operator has dozens of workers.
const MAX_RUNNING_MOLECULES_PER_GALAXY: usize = 50;
/// Maximum number of recent commits returned per galaxy.
const MAX_COMMITS_PER_GALAXY: usize = 20;
/// Maximum whispers per galaxy (recent window).
const MAX_WHISPERS_PER_GALAXY: usize = 50;
/// Maximum sparks per galaxy (recent window).
const MAX_SPARKS_PER_GALAXY: usize = 50;
/// Default window when the caller did not provide `?window=...`.
const DEFAULT_WINDOW_MINUTES: i64 = 15;
/// Default whispers window (the task ask: "last 30 min").
const DEFAULT_WHISPERS_WINDOW_MINUTES: i64 = 30;

/// `GET /motion` query string.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct MotionQuery {
    /// Time window for "recent" sections, e.g. `15m`, `30m`, `1h`.
    #[serde(default)]
    pub window: Option<String>,
    /// Optional comma-separated galaxy allowlist.
    #[serde(default)]
    pub galaxies: Option<String>,
    /// Optional comma-separated section allowlist
    /// (`workers,molecules,commits,whispers,sparks`).
    #[serde(default)]
    pub include: Option<String>,
}

/// One worker row — mirrors the fields the Mac/iOS pilots render.
#[derive(Debug, Serialize)]
struct WorkerRow {
    name: String,
    galaxy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    molecule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    /// Cost in USD over the worker's lifetime. `None` today — reserved
    /// for the claudion-probe wiring (follow-up task).
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_heartbeat: Option<String>,
}

/// One running molecule row — "what step, since when".
#[derive(Debug, Serialize)]
struct RunningMoleculeRow {
    id: String,
    galaxy: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_step: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_steps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_evolve_at: Option<String>,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assigned_worker: Option<String>,
}

/// One recent git commit, scoped to its galaxy.
#[derive(Debug, Serialize)]
struct CommitRow {
    galaxy: String,
    sha: String,
    subject: String,
    timestamp: String,
    author: String,
}

/// One recent whisper, flattened from the matrix-tick frontmatter.
#[derive(Debug, Serialize)]
struct WhisperRow {
    galaxy: String,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_nucleon_id: Option<String>,
    received_at: String,
    body_preview: String,
}

/// One recent spark molecule (idea captured via `cs spark`).
#[derive(Debug, Serialize)]
struct SparkRow {
    id: String,
    galaxy: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic_preview: Option<String>,
    tags: Vec<String>,
}

/// Which sections to emit. Selecting all is the default.
#[derive(Debug, Clone, Copy)]
struct IncludeMask {
    workers: bool,
    molecules: bool,
    commits: bool,
    whispers: bool,
    sparks: bool,
}

impl IncludeMask {
    fn all() -> Self {
        Self {
            workers: true,
            molecules: true,
            commits: true,
            whispers: true,
            sparks: true,
        }
    }

    fn from_raw(raw: Option<&str>) -> Self {
        let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Self::all();
        };
        let mut mask = Self {
            workers: false,
            molecules: false,
            commits: false,
            whispers: false,
            sparks: false,
        };
        for part in raw.split(',') {
            match part.trim().to_lowercase().as_str() {
                "workers" => mask.workers = true,
                "molecules" | "running" | "running_molecules" => mask.molecules = true,
                "commits" | "git" => mask.commits = true,
                "whispers" => mask.whispers = true,
                "sparks" => mask.sparks = true,
                "all" => return Self::all(),
                _ => {}
            }
        }
        mask
    }
}

/// Public entry for the `/motion` endpoint — extracts the query string
/// then defers to [`aggregate_motion`] on a blocking thread.
pub(crate) async fn get_motion(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MotionQuery>,
) -> Result<Json<Value>, ApiError> {
    let galaxies_root = state.galaxies_root.clone();
    let val = tokio::task::spawn_blocking(move || {
        aggregate_motion(
            &galaxies_root,
            q.window.as_deref(),
            q.galaxies.as_deref(),
            q.include.as_deref(),
        )
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("motion aggregation panicked: {e}"),
        )
    })??;
    Ok(Json(val))
}

/// Aggregate every "in motion" signal across the cluster and return the
/// JSON document served by `/motion`. Sync / reentrant / read-only — the
/// `cs motion` CLI calls this directly without going through HTTP.
///
/// - `galaxies_root` is the directory that contains one subdirectory per
///   galaxy (as seen by `GET /galaxies`).
/// - `window_raw` is the `?window=15m`-style query parameter; missing or
///   unparseable input falls back to 15 minutes.
/// - `galaxies_raw` is a comma-separated allowlist (or `None` for all).
/// - `include_raw` is a comma-separated section selector.
pub fn aggregate_motion(
    galaxies_root: &Path,
    window_raw: Option<&str>,
    galaxies_raw: Option<&str>,
    include_raw: Option<&str>,
) -> Result<Value, ApiError> {
    let now = Utc::now();
    let window = parse_window(window_raw, DEFAULT_WINDOW_MINUTES);
    let since = now - window;
    let whispers_since =
        now - chrono::Duration::minutes(DEFAULT_WHISPERS_WINDOW_MINUTES.max(window.num_minutes()));

    let allowlist = parse_allowlist(galaxies_raw);
    let mask = IncludeMask::from_raw(include_raw);

    let galaxies = discover_galaxies(galaxies_root, allowlist.as_ref()).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scan galaxies under {}: {e}", galaxies_root.display()),
        )
    })?;

    let mut workers: Vec<WorkerRow> = Vec::new();
    let mut running: Vec<RunningMoleculeRow> = Vec::new();
    let mut commits: Vec<CommitRow> = Vec::new();
    let mut whispers: Vec<WhisperRow> = Vec::new();
    let mut sparks: Vec<SparkRow> = Vec::new();

    for g in &galaxies {
        if mask.workers {
            collect_workers(g, &mut workers);
        }
        if mask.molecules || mask.sparks {
            collect_molecules(g, since, &mut running, &mut sparks, mask);
        }
        if mask.whispers {
            collect_whispers(g, whispers_since, &mut whispers);
        }
        if mask.commits {
            let since_arg = format_git_since(&window);
            for row in collect_commits(&g.root, &since_arg) {
                commits.push(CommitRow {
                    galaxy: g.name.clone(),
                    sha: row.sha,
                    subject: row.subject,
                    timestamp: row.timestamp,
                    author: row.author,
                });
            }
        }
    }

    // Stable sort so the UI renders deterministically. Newest first
    // where a timestamp is available.
    running.sort_by(|a, b| b.last_evolve_at.cmp(&a.last_evolve_at));
    commits.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    whispers.sort_by(|a, b| b.received_at.cmp(&a.received_at));
    sparks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    workers.sort_by(|a, b| b.last_heartbeat.cmp(&a.last_heartbeat));

    Ok(serde_json::json!({
        "timestamp": iso8601(now),
        "window": format_window(&window),
        "galaxies_scanned": galaxies.iter().map(|g| g.name.clone()).collect::<Vec<_>>(),
        "workers": workers,
        "running_molecules": running,
        "recent_git_commits": commits,
        "recent_whispers": whispers,
        "recent_sparks": sparks,
    }))
}

/// Parse a `?window=NNm` spec into a [`chrono::Duration`]. Falls back
/// to the default when the input is missing or unparseable.
pub(crate) fn parse_window(raw: Option<&str>, default_minutes: i64) -> chrono::Duration {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return chrono::Duration::minutes(default_minutes);
    };
    let (num_part, unit) = raw.split_at(raw.len().saturating_sub(1));
    let unit = unit.to_lowercase();
    let (num_str, unit) = if matches!(unit.as_str(), "s" | "m" | "h" | "d") {
        (num_part, unit.as_str())
    } else {
        (raw, "m")
    };
    let Ok(n) = num_str.parse::<i64>() else {
        return chrono::Duration::minutes(default_minutes);
    };
    let n = n.max(0);
    match unit {
        "s" => chrono::Duration::seconds(n),
        "h" => chrono::Duration::hours(n),
        "d" => chrono::Duration::days(n),
        _ => chrono::Duration::minutes(n),
    }
}

fn format_window(d: &chrono::Duration) -> String {
    let minutes = d.num_minutes();
    if minutes % 60 == 0 && minutes >= 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}m", minutes)
    }
}

fn format_git_since(d: &chrono::Duration) -> String {
    let secs = d.num_seconds().max(1);
    format!("{secs} seconds ago")
}

fn parse_allowlist(raw: Option<&str>) -> Option<HashSet<String>> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
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

/// A discovered galaxy — the root directory (which contains `.cosmon/`)
/// plus its basename.
#[derive(Debug, Clone)]
struct GalaxyHandle {
    name: String,
    root: PathBuf,
}

impl GalaxyHandle {
    fn state_dir(&self) -> PathBuf {
        self.root.join(".cosmon/state")
    }
    fn whispers_inbox(&self) -> PathBuf {
        self.root.join(".cosmon/whispers/inbox")
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

// --- workers ---------------------------------------------------------

fn collect_workers(g: &GalaxyHandle, out: &mut Vec<WorkerRow>) {
    let fleet_path = g.state_dir().join("fleet.json");
    let Ok(content) = std::fs::read_to_string(&fleet_path) else {
        return;
    };
    let Ok(fleet) = serde_json::from_str::<Value>(&content) else {
        return;
    };
    let runtime_map = read_runtime_repos(&g.state_dir());
    let Some(workers_obj) = fleet.get("workers").and_then(|v| v.as_object()) else {
        return;
    };
    for (name, w) in workers_obj {
        let molecule_id = w
            .get("current_molecule")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let role = w.get("role").and_then(Value::as_str).map(str::to_owned);
        let status = w.get("status").and_then(Value::as_str).map(str::to_owned);
        let last_heartbeat = w
            .get("updated_at")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let repo = runtime_map
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| r.clone());
        out.push(WorkerRow {
            name: name.clone(),
            galaxy: g.name.clone(),
            molecule_id,
            role,
            status,
            repo,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            last_heartbeat,
        });
    }
}

fn read_runtime_repos(state_dir: &Path) -> Vec<(String, String)> {
    let runtime = state_dir.join("fleet.runtime.json");
    let Ok(content) = std::fs::read_to_string(&runtime) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };
    let Some(map) = v.get("workers").and_then(|w| w.as_object()) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(name, body)| {
            body.get("repo")
                .and_then(Value::as_str)
                .map(|r| (name.clone(), r.to_owned()))
        })
        .collect()
}

// --- molecules + sparks ---------------------------------------------------

fn collect_molecules(
    g: &GalaxyHandle,
    since: DateTime<Utc>,
    running: &mut Vec<RunningMoleculeRow>,
    sparks: &mut Vec<SparkRow>,
    mask: IncludeMask,
) {
    let fleets = g.state_dir().join("fleets");
    let Ok(fleet_iter) = std::fs::read_dir(&fleets) else {
        return;
    };
    let mut per_galaxy_running = 0usize;
    let mut per_galaxy_sparks = 0usize;
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
            let id = v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if id.is_empty() {
                continue;
            }
            let status = v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            if mask.molecules
                && status == "running"
                && per_galaxy_running < MAX_RUNNING_MOLECULES_PER_GALAXY
            {
                if let Some(row) = running_row_from_value(&g.name, &id, &v) {
                    running.push(row);
                    per_galaxy_running += 1;
                }
            }
            if mask.sparks && id.starts_with("spark-") && per_galaxy_sparks < MAX_SPARKS_PER_GALAXY
            {
                let created_at = v
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(ts) = parse_iso(created_at) {
                    if ts >= since {
                        sparks.push(SparkRow {
                            id: id.clone(),
                            galaxy: g.name.clone(),
                            created_at: created_at.to_owned(),
                            topic_preview: extract_topic(&v).map(|t| truncate(&t, 120)),
                            tags: extract_tags(&v),
                        });
                        per_galaxy_sparks += 1;
                    }
                }
            }
        }
    }
}

fn running_row_from_value(galaxy: &str, id: &str, v: &Value) -> Option<RunningMoleculeRow> {
    let kind = kind_from_id(id);
    let current_step = v
        .get("current_step")
        .and_then(Value::as_u64)
        .or_else(|| v.get("step_idx").and_then(Value::as_u64));
    let total_steps = v.get("total_steps").and_then(Value::as_u64);
    // Prefer a dedicated `last_evolve_at`; fall back to `updated_at`.
    let last_evolve_at = v
        .get("last_evolve_at")
        .and_then(Value::as_str)
        .or_else(|| v.get("updated_at").and_then(Value::as_str))
        .map(str::to_owned);
    let tags = extract_tags(v);
    let topic_preview = extract_topic(v).map(|t| truncate(&t, 160));
    let assigned_worker = v
        .get("assigned_worker")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(RunningMoleculeRow {
        id: id.to_owned(),
        galaxy: galaxy.to_owned(),
        kind,
        current_step,
        total_steps,
        last_evolve_at,
        tags,
        topic_preview,
        assigned_worker,
    })
}

fn extract_topic(v: &Value) -> Option<String> {
    v.get("variables")
        .and_then(|vars| vars.get("topic"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn extract_tags(v: &Value) -> Vec<String> {
    v.get("tags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
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

// --- whispers --------------------------------------------------------

fn collect_whispers(g: &GalaxyHandle, since: DateTime<Utc>, out: &mut Vec<WhisperRow>) {
    let inbox = g.whispers_inbox();
    let Ok(rooms) = std::fs::read_dir(&inbox) else {
        return;
    };
    let mut per_galaxy = 0usize;
    for room_entry in rooms.flatten() {
        let Ok(ft) = room_entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(room_entry.path()) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(w) = parse_whisper_frontmatter(&path, &content) else {
                continue;
            };
            let Some(ts) = parse_iso(&w.received_at) else {
                continue;
            };
            if ts < since {
                continue;
            }
            if per_galaxy >= MAX_WHISPERS_PER_GALAXY {
                break;
            }
            out.push(WhisperRow {
                galaxy: g.name.clone(),
                id: w.id,
                sender_nucleon_id: w.sender_nucleon_id,
                received_at: w.received_at,
                body_preview: truncate(&w.body, 160),
            });
            per_galaxy += 1;
        }
    }
}

struct WhisperParts {
    id: String,
    sender_nucleon_id: Option<String>,
    received_at: String,
    body: String,
}

fn parse_whisper_frontmatter(path: &Path, content: &str) -> Option<WhisperParts> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let front = &rest[..end];
    let body = rest[end + 5..].trim_matches('\n').to_owned();
    let mut sender_nucleon_id: Option<String> = None;
    let mut received_at = String::new();
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let value = v.trim().trim_matches('"').to_owned();
        if value.is_empty() {
            continue;
        }
        match key {
            "sender_nucleon_id" => sender_nucleon_id = Some(value),
            "received_at" => received_at = value,
            _ => {}
        }
    }
    let id = path.file_stem()?.to_str()?.to_owned();
    if received_at.is_empty() {
        return None;
    }
    Some(WhisperParts {
        id,
        sender_nucleon_id,
        received_at,
        body,
    })
}

// --- git commits -----------------------------------------------------

struct RawCommit {
    sha: String,
    subject: String,
    timestamp: String,
    author: String,
}

fn collect_commits(root: &Path, since: &str) -> Vec<RawCommit> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root);
    cmd.args([
        "log",
        "--since",
        since,
        "--no-merges",
        // Tabs are never introduced by conventional commit subjects
        // so they make a robust field separator.
        "--pretty=format:%h\t%cI\t%an\t%s",
        "-n",
        "200",
    ]);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in stdout.lines() {
        if rows.len() >= MAX_COMMITS_PER_GALAXY {
            break;
        }
        let mut parts = line.splitn(4, '\t');
        let sha = parts.next().unwrap_or("").trim();
        let ts = parts.next().unwrap_or("").trim();
        let author = parts.next().unwrap_or("").trim();
        let subject = parts.next().unwrap_or("").trim();
        if sha.is_empty() || ts.is_empty() || subject.is_empty() {
            continue;
        }
        rows.push(RawCommit {
            sha: sha.to_owned(),
            subject: subject.to_owned(),
            timestamp: ts.to_owned(),
            author: author.to_owned(),
        });
    }
    rows
}

// --- helpers ---------------------------------------------------------

fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn iso8601(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn truncate(s: &str, max_chars: usize) -> String {
    let flat = s.replace('\n', " ");
    let flat = flat.trim();
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max_chars {
        return flat.to_owned();
    }
    let cut: String = chars.into_iter().take(max_chars).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_window_accepts_known_units() {
        assert_eq!(parse_window(Some("30s"), 15), chrono::Duration::seconds(30));
        assert_eq!(parse_window(Some("5m"), 15), chrono::Duration::minutes(5));
        assert_eq!(parse_window(Some("2h"), 15), chrono::Duration::hours(2));
        assert_eq!(parse_window(Some("1d"), 15), chrono::Duration::days(1));
    }

    #[test]
    fn parse_window_falls_back_on_junk() {
        assert_eq!(parse_window(Some(""), 15), chrono::Duration::minutes(15));
        assert_eq!(
            parse_window(Some("blorp"), 15),
            chrono::Duration::minutes(15)
        );
        assert_eq!(parse_window(None, 7), chrono::Duration::minutes(7));
    }

    #[test]
    fn include_mask_selects_sections() {
        let m = IncludeMask::from_raw(Some("workers,commits"));
        assert!(m.workers);
        assert!(m.commits);
        assert!(!m.molecules);
        assert!(!m.whispers);
        assert!(!m.sparks);
        let all = IncludeMask::from_raw(None);
        assert!(all.workers && all.molecules && all.commits && all.whispers && all.sparks);
        let all_sentinel = IncludeMask::from_raw(Some("all"));
        assert!(
            all_sentinel.workers
                && all_sentinel.molecules
                && all_sentinel.commits
                && all_sentinel.whispers
                && all_sentinel.sparks
        );
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("é".repeat(5).as_str(), 3), "ééé…");
        assert_eq!(truncate("line\nbreak", 20), "line break");
    }

    #[test]
    fn kind_from_id_matches_inbox_table() {
        assert_eq!(kind_from_id("task-20260422-db9f"), "task");
        assert_eq!(kind_from_id("spark-20260423-1234"), "spark");
        assert_eq!(kind_from_id("delib-20260422-f6d6"), "deliberation");
        assert_eq!(kind_from_id("unknown-1"), "unknown");
    }

    #[test]
    fn parse_whisper_frontmatter_reads_received_at_and_body() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("1234-abcd.md");
        std::fs::write(
            &path,
            "---\nsender_nucleon_id: \"tenant_auditor\"\nreceived_at: \"2026-04-23T08:00:00Z\"\n---\n\nhello\n",
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let w = parse_whisper_frontmatter(&path, &content).unwrap();
        assert_eq!(w.id, "1234-abcd");
        assert_eq!(w.sender_nucleon_id.as_deref(), Some("tenant_auditor"));
        assert_eq!(w.received_at, "2026-04-23T08:00:00Z");
        assert_eq!(w.body, "hello");
    }

    #[test]
    fn format_window_prefers_hours_when_aligned() {
        assert_eq!(format_window(&chrono::Duration::minutes(15)), "15m");
        assert_eq!(format_window(&chrono::Duration::minutes(60)), "1h");
        assert_eq!(format_window(&chrono::Duration::minutes(120)), "2h");
    }

    #[test]
    fn discover_galaxies_respects_allowlist() {
        let tmp = TempDir::new().unwrap();
        for name in ["cosmon", "mailroom", "other"] {
            std::fs::create_dir_all(tmp.path().join(name).join(".cosmon")).unwrap();
        }
        std::fs::create_dir_all(tmp.path().join("not-a-galaxy")).unwrap();
        let all = discover_galaxies(tmp.path(), None).unwrap();
        assert_eq!(all.len(), 3);
        let allow: HashSet<String> = ["cosmon".to_owned(), "mailroom".to_owned()]
            .into_iter()
            .collect();
        let two = discover_galaxies(tmp.path(), Some(&allow)).unwrap();
        let names: Vec<&str> = two.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["cosmon", "mailroom"]);
    }
}
