// SPDX-License-Identifier: AGPL-3.0-only

//! Shared energy probing for active workers.
//!
//! Resolution chain: worker → tmux pane PID → `~/.claude/sessions/{pid}.json`
//! → `sessionId` + `cwd` → `~/.claude/projects/{encoded-cwd}/{sessionId}.jsonl`
//! → parse with `claudion` → aggregate tokens and cost.
//!
//! Used by `cs ensemble` and `cs peek` to display the real Claude Code
//! energy spent by every active worker.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cosmon_core::energy::{TokenCost, TokenCount};
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::model_realization::{
    realized_models_from_claude_jsonl, realized_models_from_codex_session, ModelObservationSource,
};

/// Per-worker aggregated energy values.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerEnergy {
    /// Fresh input + cache-creation + cache-read tokens.
    pub input: TokenCount,
    /// Output tokens.
    pub output: TokenCount,
    /// Total cost in USD.
    pub cost: TokenCost,
}

impl WorkerEnergy {
    /// Return `(input_tokens, output_tokens, cost_usd)`.
    #[must_use]
    pub fn as_tuple(&self) -> (u64, u64, f64) {
        (self.input.get(), self.output.get(), self.cost.get())
    }
}

/// Discover all fleet-scoped tmux backends by scanning deployed fleet specs.
///
/// Returns backends for each fleet's socket plus a fallback for `project_socket`.
#[must_use]
pub fn discover_fleet_backends(
    state_dir: &std::path::Path,
    project_socket: &str,
) -> Vec<cosmon_transport::TmuxBackend> {
    let mut backends = Vec::new();
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(name) = spec["name"].as_str() {
                            backends.push(cosmon_transport::TmuxBackend::new(name));
                        }
                    }
                }
            }
        }
    }
    backends.push(cosmon_transport::TmuxBackend::new(project_socket));
    backends
}

/// Load energy for every worker in `fleet`, probing every tmux socket in `backends`.
#[must_use]
pub fn load_worker_energy(
    backends: &[cosmon_transport::TmuxBackend],
    fleet: &cosmon_state::Fleet,
) -> HashMap<WorkerId, WorkerEnergy> {
    let mut map: HashMap<WorkerId, WorkerEnergy> = HashMap::new();
    let pricing = claudion::PricingModel::opus();

    for worker_id in fleet.workers.keys() {
        let Some(energy) = probe_worker_energy(backends, worker_id, &pricing) else {
            continue;
        };
        map.insert(worker_id.clone(), energy);
    }
    map
}

/// Probe a single worker's current energy values.
#[must_use]
pub fn probe_worker_energy(
    backends: &[cosmon_transport::TmuxBackend],
    worker_id: &WorkerId,
    pricing: &claudion::PricingModel,
) -> Option<WorkerEnergy> {
    let pid = resolve_tmux_pid(backends, worker_id)?;
    let (session_id, cwd) = read_claude_pid_file(pid)?;
    let encoded = sanitize_path(&cwd);
    let jsonl_path = claude_projects_dir()
        .join(&encoded)
        .join(format!("{session_id}.jsonl"));

    if !jsonl_path.exists() {
        return None;
    }
    let session_log = claudion::parse_session(&jsonl_path).ok()?;
    let metrics = claudion::compute_metrics(&session_log, pricing);
    let input_total = metrics.total_input + metrics.total_cache_creation + metrics.total_cache_read;
    Some(WorkerEnergy {
        input: TokenCount::new(input_total.get()),
        output: TokenCount::new(metrics.total_output.get()),
        cost: TokenCost::new(metrics.total_cost.get()),
    })
}

/// **Always-on realized-model capture** at the completion seam
/// (delib-20260718-c70e / F-01). Called from `cs complete`/`cs done` — the
/// worker's session log is fully written by then, and this runs regardless of
/// whether anyone is watching `cs peek`. `cs peek` is therefore a **strict
/// reader**: it never emits.
///
/// Resolution is filesystem-only and pane-independent: the completing `cs`
/// process shares the worker's working directory, so the worker's session log
/// is resolved from that `cwd` (claude via its `projects/{sanitize(cwd)}`
/// directory, codex via the `session_meta.payload.cwd` join). The adapter that
/// actually ran and the worker to scope observations to are read from the last
/// `AdapterSelected` / `WorkerSpawned` on `events.jsonl` (F-02). The in-process
/// provider adapters (openai/anthropic/mistral) emit during their run at the
/// response seam, so they are skipped here.
///
/// Best-effort and trace-not-lock: any I/O or resolution failure yields no
/// observation, never an error.
pub fn capture_realized_at_completion(state_dir: &Path, mol_id: &MoleculeId) {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    capture_realized_from_cwd(state_dir, mol_id, &cwd);
}

/// The `cwd`-parameterised core of [`capture_realized_at_completion`], split out
/// so tests can drive it with a fixture directory instead of the process cwd.
pub fn capture_realized_from_cwd(state_dir: &Path, mol_id: &MoleculeId, cwd: &Path) {
    let adapter = last_adapter_for(state_dir, mol_id);
    let worker = last_worker_for(state_dir, mol_id);
    let (observed, source) = match adapter.as_deref() {
        // Subprocess adapters whose model lives in a session log on disk.
        Some("claude") | None => (
            resolve_claude_session_by_cwd(cwd)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|c| realized_models_from_claude_jsonl(&c))
                .unwrap_or_default(),
            ModelObservationSource::ClaudeStreamJson,
        ),
        Some("codex") => (
            resolve_codex_session_by_cwd(cwd)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|c| realized_models_from_codex_session(&c))
                .unwrap_or_default(),
            ModelObservationSource::CodexSessionMeta,
        ),
        // In-process providers emit at their own response seam.
        _ => return,
    };
    if observed.is_empty() {
        return;
    }
    let adapter_name = adapter.as_deref().unwrap_or("claude");
    cosmon_state::events::worker_spawn::emit_new_model_observations(
        state_dir,
        mol_id,
        worker.as_ref(),
        adapter_name,
        &observed,
        source,
    );
}

/// The adapter that most recently ran for `mol_id`, folded from the last
/// [`EventV2::AdapterSelected`] on `events.jsonl`. `None` on read error or when
/// no selection was recorded (legacy → treated as claude by the caller).
fn last_adapter_for(state_dir: &Path, mol_id: &MoleculeId) -> Option<String> {
    let log_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let envelopes = cosmon_state::event_log::read_all(&log_path).ok()?;
    envelopes.into_iter().rev().find_map(|env| match env.event {
        EventV2::AdapterSelected {
            mol_id: ref m,
            adapter_name,
            ..
        } if m == mol_id => Some(adapter_name),
        _ => None,
    })
}

/// The worker most recently spawned for `mol_id` (the current attempt), from the
/// last [`EventV2::WorkerSpawned`]. Scopes the emitted observations (F-02).
fn last_worker_for(state_dir: &Path, mol_id: &MoleculeId) -> Option<WorkerId> {
    let log_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let envelopes = cosmon_state::event_log::read_all(&log_path).ok()?;
    envelopes.into_iter().rev().find_map(|env| match env.event {
        EventV2::WorkerSpawned {
            molecule: Some(ref m),
            worker_id,
            ..
        } if m == mol_id => Some(worker_id),
        _ => None,
    })
}

/// Resolve the claude session `*.jsonl` for a worker whose `cwd` is known: the
/// most-recently-modified log under `~/.claude/projects/{sanitize(cwd)}/`. The
/// completing process shares the worker's cwd, so this needs no live pane.
fn resolve_claude_session_by_cwd(cwd: &Path) -> Option<PathBuf> {
    let dir = claude_projects_dir().join(sanitize_path(&cwd.to_string_lossy()));
    most_recent_jsonl(&dir)
}

/// Resolve the codex session `rollout-*.jsonl` for a worker whose `cwd` is
/// known: the most-recently-modified log under `~/.codex/sessions/**` whose
/// `session_meta.payload.cwd` equals `cwd`. codex writes no pid sidecar, so the
/// worktree `cwd` recorded in `session_meta` is the only join key.
fn resolve_codex_session_by_cwd(cwd: &Path) -> Option<PathBuf> {
    let target = cwd.to_string_lossy();
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for path in codex_session_files() {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !codex_session_matches_cwd(&content, &target) {
            continue;
        }
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Whether a codex session log's `session_meta` line names `cwd` as its
/// working directory (`payload.cwd`, with a top-level `cwd` fallback).
fn codex_session_matches_cwd(content: &str, cwd: &str) -> bool {
    for line in content.lines().take(8) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
            continue;
        }
        let found = value
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .or_else(|| value.get("cwd"))
            .and_then(serde_json::Value::as_str);
        return found == Some(cwd);
    }
    false
}

/// All codex session `*.jsonl` files under `~/.codex/sessions/**` (date-bucketed
/// `YYYY/MM/DD/rollout-*.jsonl`). Best-effort — an unreadable tree yields none.
fn codex_session_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&codex_sessions_dir(), &mut out);
    out
}

/// The most-recently-modified `*.jsonl` directly inside `dir`, or `None` when
/// the directory is absent/empty.
fn most_recent_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
                best = Some((mtime, path));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// `~/.codex/sessions/` directory.
#[must_use]
pub fn codex_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".codex").join("sessions")
}

/// Resolve the tmux pane PID for a worker by probing all sockets.
#[must_use]
pub fn resolve_tmux_pid(
    backends: &[cosmon_transport::TmuxBackend],
    worker_id: &WorkerId,
) -> Option<u32> {
    for be in backends {
        let Ok(output) = std::process::Command::new("tmux")
            .args(["-L", be.socket(), "display-message", "-t"])
            .arg(worker_id.as_str())
            .args(["-p", "#{pane_pid}"])
            .output()
        else {
            continue;
        };
        if output.status.success() {
            let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Ok(pid) = pid_str.parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

/// Read `~/.claude/sessions/{pid}.json` → `(sessionId, cwd)`.
#[must_use]
pub fn read_claude_pid_file(pid: u32) -> Option<(String, String)> {
    let path = claude_sessions_dir().join(format!("{pid}.json"));
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let session_id = json.get("sessionId")?.as_str()?.to_string();
    let cwd = json.get("cwd")?.as_str()?.to_string();
    Some((session_id, cwd))
}

/// Encode a filesystem path the same way Claude Code does
/// (non-alphanumeric → `-`). Mirrors `sanitizePath()` in Claude Code's
/// `sessionStoragePortable.ts`.
#[must_use]
pub fn sanitize_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `~/.claude/sessions/` directory.
#[must_use]
pub fn claude_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".claude").join("sessions")
}

/// `~/.claude/projects/` directory.
#[must_use]
pub fn claude_projects_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".claude").join("projects")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_replaces_non_alphanumeric() {
        assert_eq!(
            sanitize_path("/Users/e/dev/projects/cosmon"),
            "-Users-e-dev-projects-cosmon"
        );
    }

    #[test]
    fn codex_session_matches_cwd_reads_session_meta_payload() {
        let content = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/work/tree","session_id":"s"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-terra"}}"#,
        );
        assert!(codex_session_matches_cwd(content, "/work/tree"));
        assert!(!codex_session_matches_cwd(content, "/other"));
    }

    #[test]
    fn codex_session_matches_cwd_false_without_session_meta() {
        let content = r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#;
        assert!(!codex_session_matches_cwd(content, "/work/tree"));
    }

    #[test]
    fn worker_energy_tuple_roundtrip() {
        let e = WorkerEnergy {
            input: TokenCount::new(100),
            output: TokenCount::new(50),
            cost: TokenCost::new(0.25),
        };
        let (i, o, c) = e.as_tuple();
        assert_eq!(i, 100);
        assert_eq!(o, 50);
        assert!((c - 0.25).abs() < f64::EPSILON);
    }
}
