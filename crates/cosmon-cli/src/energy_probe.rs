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

/// **Runtime realized-model capture** (round-3 / F-01) — called from the poll
/// tick of a live surface (`cs wait`, `cs run`) *while the worker is still
/// running*, so `ModelObserved` lands on `events.jsonl` at the **first**
/// model-bearing turn, not at teardown. This is what makes the observation
/// durable across a worker crash that never reaches `cs complete` (D4:
/// "premier assistant turn, pas au teardown").
///
/// Unlike [`capture_realized_at_completion`] the polling process does **not**
/// share the worker's cwd (the operator runs `cs wait` from the repo root),
/// so the worker's working directory is resolved live from its tmux pane
/// (`#{pane_current_path}`). The rest of the chain is the same
/// [`capture_realized_from_cwd`] core: session-log resolution by cwd, typed
/// parse, first-observation + on-change dedup, worker-scoped emission.
///
/// Best-effort and cheap to call repeatedly: once the trajectory is on the
/// wire, re-capture emits nothing (idempotent), and a dead pane / missing
/// session resolves to a silent no-op.
pub fn capture_realized_runtime(
    state_dir: &Path,
    mol_id: &MoleculeId,
    backends: &[cosmon_transport::TmuxBackend],
) {
    // Fail-closed scoping (F-02): no resolvable worker → no emission.
    let Some(worker) = last_worker_for(state_dir, mol_id) else {
        return;
    };
    let Some(cwd) = resolve_tmux_pane_cwd(backends, &worker) else {
        return;
    };
    capture_realized_from_cwd(state_dir, mol_id, &cwd);
}

/// The live working directory of a worker's tmux pane
/// (`#{pane_current_path}`), probing every socket in `backends`. `None` when
/// no pane answers — the worker is dead or was never tmux-hosted.
fn resolve_tmux_pane_cwd(
    backends: &[cosmon_transport::TmuxBackend],
    worker_id: &WorkerId,
) -> Option<PathBuf> {
    for be in backends {
        let Ok(output) = std::process::Command::new("tmux")
            .args(["-L", be.socket(), "display-message", "-t"])
            .arg(worker_id.as_str())
            .args(["-p", "#{pane_current_path}"])
            .output()
        else {
            continue;
        };
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// The `cwd`-parameterised core of [`capture_realized_at_completion`] and
/// [`capture_realized_runtime`], split out so tests can drive it with a
/// fixture directory instead of a process cwd / live pane.
pub fn capture_realized_from_cwd(state_dir: &Path, mol_id: &MoleculeId, cwd: &Path) {
    let adapter = last_adapter_for(state_dir, mol_id);
    // Fail-closed worker scoping (round-3 / F-02): every new observation must
    // be attached to the worker that produced it. No resolvable worker → no
    // emission — an unscoped line would be ambiguous forever.
    let Some(worker) = last_worker_for(state_dir, mol_id) else {
        return;
    };
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
        &worker,
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

    // ---- F-01/F-06 end-to-end: completion-seam capture -------------------
    //
    // fixture session file on disk → `capture_realized_from_cwd` (the real
    // completion seam) → `ModelObserved` on `events.jsonl` → retrospective
    // fold → `compact_cell`. Exercised for claude and codex, which resolve
    // their session by the worker's cwd. HOME is swapped to a fixture root;
    // the swap is serialized so a parallel test never sees the borrowed HOME.

    use std::sync::Mutex;
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn seed_dispatch(state_dir: &Path, mol: &MoleculeId, adapter: &str, worker: &str) {
        let log = cosmon_state::event_log::resolve_events_log_path(state_dir);
        cosmon_state::event_log::emit_one(
            &log,
            EventV2::AdapterSelected {
                mol_id: mol.clone(),
                adapter_name: adapter.to_owned(),
                selected_at: chrono::Utc::now(),
                selection_source: cosmon_core::event_v2::AdapterSelectionSource::Cli {
                    flag: adapter.to_owned(),
                },
                role_hint: None,
                loop_ownership: Default::default(),
            },
            None,
        )
        .unwrap();
        cosmon_state::event_log::emit_one(
            &log,
            EventV2::WorkerSpawned {
                worker_id: WorkerId::new(worker).unwrap(),
                molecule: Some(mol.clone()),
                session_name: "sess".to_owned(),
                role: "polecat".to_owned(),
                adapter_name: adapter.to_owned(),
                loop_ownership: Default::default(),
            },
            None,
        )
        .unwrap();
    }

    fn fold_from_log(
        state_dir: &Path,
        mol: &MoleculeId,
    ) -> cosmon_core::adapter_attribution::AdapterAttribution {
        let log = cosmon_state::event_log::resolve_events_log_path(state_dir);
        let events: Vec<EventV2> = cosmon_state::event_log::read_all(&log)
            .unwrap()
            .into_iter()
            .filter(|e| e.event.molecule_id() == Some(mol))
            .map(|e| e.event)
            .collect();
        cosmon_core::adapter_attribution::AdapterAttribution::fold(&events)
    }

    #[test]
    fn capture_at_completion_claude_end_to_end() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // Claude wrote a session log under projects/{sanitize(cwd)}/.
        let proj = claude_projects_dir().join(sanitize_path(&cwd.path().to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            concat!(
                r#"{"type":"system","subtype":"init","model":"claude-opus-4-8"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-opus-4-8"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5"}}"#,
                "\n",
            ),
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260718-c1a1").unwrap();
        seed_dispatch(state.path(), &mol, "claude", "worker-1");
        // Intention pinned opus.
        cosmon_state::events::worker_spawn::emit_model_selected(
            state.path(),
            &mol,
            "claude",
            Some("claude-opus-4-8"),
            cosmon_core::event_v2::ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );

        capture_realized_from_cwd(state.path(), &mol, cwd.path());

        let att = fold_from_log(state.path(), &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec![
                "claude-opus-4-8".to_string(),
                "claude-sonnet-5".to_string(),
            ]),
        );
        // A real quota fallback surfaces as drift in the compact cell.
        assert_eq!(
            att.compact_cell(),
            "claude/claude-opus-4-8~>claude-sonnet-5 [cli]"
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn capture_at_completion_codex_end_to_end() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // Codex wrote a date-bucketed rollout log; session_meta.payload.cwd is
        // the only join key back to the worker's worktree.
        let sess = codex_sessions_dir().join("2026").join("07").join("18");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("rollout-x.jsonl"),
            format!(
                concat!(
                    r#"{{"type":"session_meta","payload":{{"cwd":"{cwd}","session_id":"s"}}}}"#,
                    "\n",
                    r#"{{"type":"turn_context","payload":{{"model":"gpt-5.6-terra"}}}}"#,
                    "\n",
                ),
                cwd = cwd.path().to_string_lossy()
            ),
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260718-c0de").unwrap();
        seed_dispatch(state.path(), &mol, "codex", "worker-1");

        capture_realized_from_cwd(state.path(), &mol, cwd.path());

        let att = fold_from_log(state.path(), &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec!["gpt-5.6-terra".to_string()]),
        );
        // No pin, but a model was observed → shown after adapter with `~>`.
        assert_eq!(att.compact_cell(), "codex~>gpt-5.6-terra [cli]");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    // ---- COND-1 (round-3 / F-01): runtime capture + crash durability ------
    //
    // The normative cadence (D4) is "first assistant turn, not teardown". The
    // runtime consumer rides the poll tick of `cs wait` / `cs run`
    // (`wait_for_status_with_metrics_probed` / `Runtime::with_tick_probe`), so
    // the observation is on `events.jsonl` while the worker is still running.
    // These tests drive the REAL wait loop against a real `FileStore` and the
    // real capture core, then crash the worker (WorkerExited, never
    // `MoleculeCompleted`) and prove the observation is durable — with
    // `cs peek` never involved.

    fn seed_running_molecule(state_dir: &Path, mol: &MoleculeId) -> cosmon_filestore::FileStore {
        use std::collections::{BTreeSet, HashMap};
        let store = cosmon_filestore::FileStore::new(state_dir);
        let data = cosmon_state::MoleculeData {
            id: mol.clone(),
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            formula_id: cosmon_core::id::FormulaId::new("task-work").unwrap(),
            status: cosmon_core::molecule::MoleculeStatus::Running,
            variables: HashMap::new(),
            assigned_worker: Some(WorkerId::new("worker-1").unwrap()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 1,
            current_step: 0,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        };
        use cosmon_state::StateStore as _;
        store.save_molecule(mol, &data).unwrap();
        store
    }

    fn crash_worker(state_dir: &Path, mol: &MoleculeId) {
        let log = cosmon_state::event_log::resolve_events_log_path(state_dir);
        cosmon_state::event_log::emit_one(
            &log,
            EventV2::WorkerExited {
                molecule_id: mol.clone(),
                exit_code: Some(137),
                reason: "pane_died".to_owned(),
            },
            None,
        )
        .unwrap();
    }

    /// Claude: the FIRST model-bearing turn is captured by the runtime poll
    /// (real `cs wait` loop, probed per tick) → the worker crashes before any
    /// `cs complete` → the observation is durable and the fold renders it.
    #[test]
    fn runtime_first_turn_observation_survives_claude_crash_before_complete() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // The worker has produced exactly ONE model-bearing turn so far — the
        // session is mid-run, nowhere near teardown.
        let proj = claude_projects_dir().join(sanitize_path(&cwd.path().to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260718-11f1").unwrap();
        let store = seed_running_molecule(state.path(), &mol);
        seed_dispatch(state.path(), &mol, "claude", "worker-1");

        // Drive the REAL runtime seam: the wait poll loop, with the capture as
        // its per-poll probe (exactly what `cs wait` wires). The molecule
        // stays Running, so the wait times out — but the probe has fired
        // during the run.
        let err = cosmon_state::wait::wait_for_status_with_metrics_probed(
            &store,
            state.path(),
            &mol,
            &[cosmon_core::molecule::MoleculeStatus::Completed],
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(1),
            || capture_realized_from_cwd(state.path(), &mol, cwd.path()),
        )
        .expect_err("worker is still running — the wait must time out");
        assert!(matches!(err, cosmon_state::wait::WaitError::Timeout { .. }));

        // The observation landed DURING the run, before any completion.
        let log = cosmon_state::event_log::resolve_events_log_path(state.path());
        let events = cosmon_state::event_log::read_all(&log).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, EventV2::ModelObserved { .. }))
                .count(),
            1,
            "first model-bearing turn must be observed during the run"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.event, EventV2::MoleculeCompleted { .. })),
            "no completion ever happened — D4: not at teardown"
        );

        // The worker crashes. `cs complete` never runs, `cs peek` never opens.
        crash_worker(state.path(), &mol);

        // The observation is durable: the retrospective fold still renders it.
        let att = fold_from_log(state.path(), &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec![
                "claude-opus-4-8".to_string()
            ]),
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// Codex: same crash-durability property through the codex session-log
    /// resolution (`session_meta.payload.cwd` join).
    #[test]
    fn runtime_first_turn_observation_survives_codex_crash_before_complete() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let sess = codex_sessions_dir().join("2026").join("07").join("18");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("rollout-x.jsonl"),
            format!(
                concat!(
                    r#"{{"type":"session_meta","payload":{{"cwd":"{cwd}","session_id":"s"}}}}"#,
                    "\n",
                    r#"{{"type":"turn_context","payload":{{"model":"gpt-5.6-terra"}}}}"#,
                    "\n",
                ),
                cwd = cwd.path().to_string_lossy()
            ),
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260718-11f2").unwrap();
        let store = seed_running_molecule(state.path(), &mol);
        seed_dispatch(state.path(), &mol, "codex", "worker-1");

        let err = cosmon_state::wait::wait_for_status_with_metrics_probed(
            &store,
            state.path(),
            &mol,
            &[cosmon_core::molecule::MoleculeStatus::Completed],
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(1),
            || capture_realized_from_cwd(state.path(), &mol, cwd.path()),
        )
        .expect_err("worker is still running — the wait must time out");
        assert!(matches!(err, cosmon_state::wait::WaitError::Timeout { .. }));

        crash_worker(state.path(), &mol);

        let att = fold_from_log(state.path(), &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec!["gpt-5.6-terra".to_string()]),
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// Round-3 / F-02 fail-closed at the emitter: a dispatch with NO
    /// `WorkerSpawned` on the journal must not emit an unscoped observation —
    /// no worker, no line.
    #[test]
    fn capture_without_worker_boundary_emits_nothing() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let proj = claude_projects_dir().join(sanitize_path(&cwd.path().to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260718-11f3").unwrap();
        // AdapterSelected only — no WorkerSpawned boundary.
        let log = cosmon_state::event_log::resolve_events_log_path(state.path());
        cosmon_state::event_log::emit_one(
            &log,
            EventV2::AdapterSelected {
                mol_id: mol.clone(),
                adapter_name: "claude".to_owned(),
                selected_at: chrono::Utc::now(),
                selection_source: cosmon_core::event_v2::AdapterSelectionSource::Cli {
                    flag: "claude".to_owned(),
                },
                role_hint: None,
                loop_ownership: Default::default(),
            },
            None,
        )
        .unwrap();

        capture_realized_from_cwd(state.path(), &mol, cwd.path());

        let n_observed = cosmon_state::event_log::read_all(&log)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.event, EventV2::ModelObserved { .. }))
            .count();
        assert_eq!(n_observed, 0, "no worker boundary → no unscoped emission");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn capture_is_idempotent_across_repeated_completion_reads() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let proj = claude_projects_dir().join(sanitize_path(&cwd.path().to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260718-1de1").unwrap();
        seed_dispatch(state.path(), &mol, "claude", "worker-1");

        // Two capture passes must emit the observation exactly once.
        capture_realized_from_cwd(state.path(), &mol, cwd.path());
        capture_realized_from_cwd(state.path(), &mol, cwd.path());

        let log = cosmon_state::event_log::resolve_events_log_path(state.path());
        let n_observed = cosmon_state::event_log::read_all(&log)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.event, EventV2::ModelObserved { .. }))
            .count();
        assert_eq!(n_observed, 1, "idempotent: one observation, not two");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn capture_skips_in_process_provider_adapters() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // An openai dispatch: the completion seam must NOT read a session file
        // (there is none) — the provider emitted at its own response seam.
        let mol = MoleculeId::new("task-20260718-0a11").unwrap();
        seed_dispatch(state.path(), &mol, "openai", "worker-1");
        capture_realized_from_cwd(state.path(), &mol, cwd.path());

        let log = cosmon_state::event_log::resolve_events_log_path(state.path());
        let n_observed = cosmon_state::event_log::read_all(&log)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.event, EventV2::ModelObserved { .. }))
            .count();
        assert_eq!(n_observed, 0, "completion seam is a no-op for openai");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}
