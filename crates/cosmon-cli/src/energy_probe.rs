// SPDX-License-Identifier: AGPL-3.0-only

//! Shared energy probing for active workers.
//!
//! The probe is **adapter-aware**: the adapter that actually ran a worker's
//! molecule (last `AdapterSelected` on `events.jsonl`) selects the resolution
//! chain.
//!
//! Claude chain: worker → tmux pane PID → `~/.claude/sessions/{pid}.json`
//! → `sessionId` + `cwd` → `~/.claude/projects/{encoded-cwd}/{sessionId}.jsonl`
//! → parse with `claudion` → aggregate tokens and cost.
//!
//! Codex chain: worker → tmux pane cwd (`#{pane_current_path}`, with the
//! fleet-recorded worktree as post-mortem fallback) →
//! [`resolve_codex_session_by_cwd`] (the `session_meta.payload.cwd` join)
//! → [`cosmon_core::codex_energy`] token parser + price table →
//! [`WorkerEnergy`]. Cost is attributed to the last realized model of the
//! session's `turn_context` trajectory; an unpriced model keeps the real
//! token counts and leaves cost at `0.0`, which the UI renders as `—`
//! (honest floor — never fabricate a rate).
//!
//! Used by `cs ensemble` and `cs peek` to display the real energy spent by
//! every active worker, whichever subprocess adapter hosts it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cosmon_core::codex_energy::{codex_price_for, codex_token_usage_from_session};
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

/// Load energy for every worker in `fleet`, probing every tmux socket in
/// `backends`. `state_dir` locates `events.jsonl`, whose last
/// `AdapterSelected` per molecule routes each worker to the right
/// session-log parser (claude vs codex).
#[must_use]
pub fn load_worker_energy(
    state_dir: &Path,
    backends: &[cosmon_transport::TmuxBackend],
    fleet: &cosmon_state::Fleet,
) -> HashMap<WorkerId, WorkerEnergy> {
    let mut map: HashMap<WorkerId, WorkerEnergy> = HashMap::new();
    let pricing = claudion::PricingModel::opus();

    // Fold the global journal **once** into a `mol_id -> last adapter` map,
    // then read the per-worker adapter from that map. The previous shape
    // called `last_adapter_for` inside the loop, and each call re-read and
    // re-parsed the entire `events.jsonl` — so a fleet with `W` workers
    // folded the whole journal `W` times, an `O(W × J)` blowup that
    // dominated `cs peek` on aged galaxies (a 94-worker / 80k-line journal
    // fleet spent ~6 s here alone, folding 7.6M envelopes to answer a
    // question one fold of 80k answers). See
    // `docs/design/peek-fold-cost.md`.
    let adapters = fold_last_adapters(state_dir);

    for (worker_id, data) in &fleet.workers {
        let adapter = data
            .current_molecule
            .as_ref()
            .and_then(|m| adapters.get(m))
            .map(String::as_str);
        let Some(energy) =
            probe_worker_energy_with_adapter(state_dir, backends, worker_id, adapter, &pricing)
        else {
            continue;
        };
        map.insert(worker_id.clone(), energy);
    }
    map
}

/// Fold the global `events.jsonl` **once** into a `mol_id -> last adapter`
/// map: for every [`EventV2::AdapterSelected`], the last one wins (forward
/// scan, overwrite). This is the batched form of [`last_adapter_for`] — one
/// journal read answers the adapter question for *every* molecule at once,
/// so callers with many workers avoid re-parsing the whole journal per
/// worker.
///
/// Returns an empty map on read error (same fail-soft contract as
/// [`last_adapter_for`], whose `None` the caller treats as "legacy → claude").
#[must_use]
fn fold_last_adapters(state_dir: &Path) -> HashMap<MoleculeId, String> {
    let log_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let mut out: HashMap<MoleculeId, String> = HashMap::new();
    let Ok(envelopes) = cosmon_state::event_log::read_all(&log_path) else {
        return out;
    };
    for env in envelopes {
        if let EventV2::AdapterSelected {
            mol_id,
            adapter_name,
            ..
        } = env.event
        {
            out.insert(mol_id, adapter_name);
        }
    }
    out
}

/// Probe a single worker's current energy values, **adapter-aware**.
///
/// The `adapter` that ran the worker's molecule (the last
/// [`EventV2::AdapterSelected`] on `events.jsonl`, resolved by the caller —
/// batched once via [`fold_last_adapters`], or for one molecule via
/// [`last_adapter_for`]) selects the chain: `codex` reads the codex rollout
/// log via [`probe_codex_worker_energy`]; `claude` — and the legacy case
/// where no selection was ever recorded (`adapter == None`) — reads the
/// Claude Code session log via the PID-sidecar chain, unchanged. In-process
/// provider adapters (openai/anthropic/mistral) have no session log on disk
/// and resolve to `None` through the claude chain's missing PID sidecar.
///
/// The adapter is passed in rather than folded here so a caller iterating
/// over a whole fleet folds the journal a **single** time instead of once
/// per worker — the difference between `O(J)` and `O(W × J)` on an aged
/// galaxy. See [`load_worker_energy`].
#[must_use]
pub fn probe_worker_energy_with_adapter(
    state_dir: &Path,
    backends: &[cosmon_transport::TmuxBackend],
    worker_id: &WorkerId,
    adapter: Option<&str>,
    pricing: &claudion::PricingModel,
) -> Option<WorkerEnergy> {
    if adapter == Some("codex") {
        return probe_codex_worker_energy(state_dir, backends, worker_id);
    }
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

/// Probe a **codex** worker's energy from its rollout session log.
///
/// Chain: live pane cwd (`#{pane_current_path}`), falling back to the
/// worktree `cs tackle` recorded on the fleet when no pane answers (dead
/// pane — same post-mortem fallback as the realized-model capture) →
/// [`resolve_codex_session_by_cwd`] → last cumulative `token_count` line
/// ([`codex_token_usage_from_session`]) → cost priced against the **last**
/// realized model of the `turn_context` trajectory
/// ([`cosmon_core::codex_energy::codex_price_for`]).
///
/// Honest floor: a model absent from the price table yields real token
/// counts with `cost = 0.0`, which the ensemble/peek COST column renders
/// as `—`. Input tokens include the cached portion — the same class of
/// total the claude chain reports (fresh + cache creation + cache read).
fn probe_codex_worker_energy(
    state_dir: &Path,
    backends: &[cosmon_transport::TmuxBackend],
    worker_id: &WorkerId,
) -> Option<WorkerEnergy> {
    let cwd = resolve_tmux_pane_cwd(backends, worker_id)
        .or_else(|| resolve_recorded_worker_cwd(state_dir, worker_id))?;
    let session_path = resolve_codex_session_by_cwd(&cwd)?;
    let content = std::fs::read_to_string(session_path).ok()?;
    let usage = codex_token_usage_from_session(&content)?;
    let cost = realized_models_from_codex_session(&content)
        .last()
        .and_then(|model| codex_price_for(model.as_str()))
        .map_or(0.0, |price| usage.cost_usd(&price));
    Some(WorkerEnergy {
        input: TokenCount::new(usage.input_tokens),
        output: TokenCount::new(usage.output_tokens),
        cost: TokenCost::new(cost),
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
/// **Post-mortem resolution** (round-4 / COND-1): a dead pane must not lose
/// the observation — the worker's session JSONL is already durable on disk
/// when the pane dies, so `turn(model) → crash → next poll` must still emit.
/// When no pane answers, the cwd falls back to the worker's working directory
/// recorded at dispatch on the fleet (`WorkerData::repo`, stamped by
/// `cs tackle` when it creates the worktree), which survives the worker's
/// death until harvest tears the state down.
///
/// Best-effort and cheap to call repeatedly: once the trajectory is on the
/// wire, re-capture emits nothing (idempotent), and a missing session
/// resolves to a silent no-op.
pub fn capture_realized_runtime(
    state_dir: &Path,
    mol_id: &MoleculeId,
    backends: &[cosmon_transport::TmuxBackend],
) {
    // Fail-closed scoping (F-02): no resolvable worker → no emission.
    let Some(worker) = last_worker_for(state_dir, mol_id) else {
        return;
    };
    let cwd = resolve_tmux_pane_cwd(backends, &worker)
        .or_else(|| resolve_recorded_worker_cwd(state_dir, &worker));
    let Some(cwd) = cwd else {
        return;
    };
    capture_realized_from_cwd(state_dir, mol_id, &cwd);
}

/// The working directory `cs tackle` recorded for `worker_id` on the fleet
/// (`WorkerData::repo`, relative to the project root) — the pane-independent
/// join key back to the worker's session log. `None` when the worker is not
/// on the fleet or carries no repo (e.g. already harvested).
fn resolve_recorded_worker_cwd(state_dir: &Path, worker_id: &WorkerId) -> Option<PathBuf> {
    use cosmon_state::StateStore as _;
    let store = cosmon_filestore::FileStore::new(state_dir);
    let fleet = store.load_fleet().ok()?;
    let repo = fleet.workers.get(worker_id)?.repo.clone()?;
    Some(match store.project_root() {
        Some(root) => cosmon_filestore::resolve_repo_path(&repo, &root),
        // Legacy absolute path (or rootless test layout): use as recorded.
        None => PathBuf::from(repo),
    })
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
    capture_realized_from_cwd_under(state_dir, mol_id, cwd, None);
}

/// [`capture_realized_from_cwd`] with an explicitly named Claude `projects/`
/// root (`None` = resolve from this process's environment, the historical
/// behaviour).
///
/// The detached `cs realized-watch` child passes `Some(root)` because the
/// dispatcher knows which Claude configuration directory the worker was
/// actually launched under, and the watcher's own environment may not name it
/// (a `cb next`-derived `~/.claude-accounts/<email>/` appears in no inherited
/// variable). Codex resolution is unaffected — it joins on
/// `session_meta.payload.cwd` under `~/.codex`, a root cosmon never rewrites.
pub fn capture_realized_from_cwd_under(
    state_dir: &Path,
    mol_id: &MoleculeId,
    cwd: &Path,
    claude_projects_root: Option<&Path>,
) {
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
            match claude_projects_root {
                Some(root) => resolve_claude_session_by_cwd_under(root, cwd),
                None => resolve_claude_session_by_cwd(cwd),
            }
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

/// The session-log **root directory** the realized-model observer will read
/// for `mol_id`, given the Claude `projects/` root this process was told to
/// use (`None` = resolve from the environment).
///
/// This is the directory whose *absence* means no observation can ever
/// arrive — the thing the watcher checks so it can say so instead of ticking
/// in silence (task-20260727-3f46). `None` is returned for adapters that keep
/// no session log on disk (the in-process providers): there is no root to
/// miss, so there is nothing to report.
///
/// Note this is the shared root (`…/projects`, `~/.codex/sessions`), not the
/// per-`cwd` subdirectory. The subdirectory is legitimately absent for the
/// first seconds of every run — the worker creates it when it writes its
/// first turn — so treating *its* absence as a broken seam would cry wolf on
/// every healthy dispatch.
#[must_use]
pub fn session_log_root_for(
    state_dir: &Path,
    mol_id: &MoleculeId,
    claude_projects_root: Option<&Path>,
) -> Option<PathBuf> {
    match last_adapter_for(state_dir, mol_id).as_deref() {
        Some("claude") | None => {
            Some(claude_projects_root.map_or_else(claude_projects_dir, Path::to_path_buf))
        }
        Some("codex") => Some(codex_sessions_dir()),
        _ => None,
    }
}

/// The adapter that most recently ran for `mol_id`, folded from the last
/// [`EventV2::AdapterSelected`] on `events.jsonl`. `None` on read error or when
/// no selection was recorded (legacy → treated as claude by the caller).
///
/// Public so the detached watcher can scope its own diagnostics to the
/// adapter that actually ran, without re-implementing the fold.
#[must_use]
pub fn last_adapter_for(state_dir: &Path, mol_id: &MoleculeId) -> Option<String> {
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
///
/// Public for the same reason as [`last_adapter_for`]: the watcher's
/// broken-seam diagnostic is scoped to the same attempt as the observations
/// it replaces, and must resolve that scope the same way.
#[must_use]
pub fn last_worker_for(state_dir: &Path, mol_id: &MoleculeId) -> Option<WorkerId> {
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
    resolve_claude_session_by_cwd_under(&claude_projects_dir(), cwd)
}

/// [`resolve_claude_session_by_cwd`] against an explicitly named `projects/`
/// root — the watcher's path, where the root comes from the dispatcher rather
/// than from this process's environment.
fn resolve_claude_session_by_cwd_under(projects_root: &Path, cwd: &Path) -> Option<PathBuf> {
    most_recent_jsonl(&projects_root.join(sanitize_path(&cwd.to_string_lossy())))
}

/// Resolve the codex session `rollout-*.jsonl` for a worker whose `cwd` is
/// known: the most-recently-modified log under `~/.codex/sessions/**` whose
/// `session_meta.payload.cwd` equals `cwd`. codex writes no pid sidecar, so the
/// worktree `cwd` recorded in `session_meta` is the only join key.
///
/// **Multiple sessions per cwd** (resume, retry, a second `codex` run in the
/// same worktree): the most-recent-mtime session wins. The mtime of an
/// append-only log tracks its last write, so the session currently being
/// written — the live worker's — always outranks finished ones. An abandoned
/// earlier attempt is at worst under-reported, never double-counted; the
/// energy shown is that of the *current* attempt, matching what the claude
/// chain reports through its pid sidecar.
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

/// The Claude Code **configuration root** — the directory that holds
/// `projects/` and `sessions/`.
///
/// Precedence mirrors Claude Code's own, and that is the whole point:
/// `CLAUDE_CONFIG_DIR` when it is set to a non-empty value, `$HOME/.claude`
/// otherwise. cosmon *itself* exports `CLAUDE_CONFIG_DIR` onto every worker it
/// spawns (multi-account routing via `cb`, and every container deployment),
/// so a resolver that hardcoded `$HOME/.claude` looked for the session log in
/// a directory that does not exist while the real one sat one level over —
/// silently losing the realized-model observation for the whole life of the
/// run (task-20260727-3f46; measured in a Colima container where
/// `$HOME/.claude` was absent entirely).
///
/// Note the asymmetry with `$HOME`: an unset `HOME` falls back to `"."`
/// (historical behaviour, kept), while an *empty* `CLAUDE_CONFIG_DIR` is
/// treated as unset — an empty path would resolve to `/projects` at the
/// filesystem root, which is never what an operator means.
#[must_use]
pub fn claude_config_dir() -> PathBuf {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".claude")
        }
    }
}

/// The Claude Code `sessions/` directory (pid sidecars), under
/// [`claude_config_dir`].
#[must_use]
pub fn claude_sessions_dir() -> PathBuf {
    claude_config_dir().join("sessions")
}

/// The Claude Code `projects/` directory (session logs), under
/// [`claude_config_dir`].
#[must_use]
pub fn claude_projects_dir() -> PathBuf {
    claude_config_dir().join("projects")
}

/// The `projects/` directory of an **explicitly named** Claude configuration
/// root — the value `cs tackle` resolved for this dispatch and handed to the
/// watcher on its command line.
///
/// The detached watcher cannot re-derive the worker's config dir from its own
/// environment: when the account came from `cb next`, the directory is
/// `~/.claude-accounts/<email>/` and appears in no environment variable the
/// watcher inherits. So the dispatcher tells it, and this is where that
/// answer is turned into a path.
#[must_use]
pub fn claude_projects_dir_under(config_dir: &Path) -> PathBuf {
    config_dir.join("projects")
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared fixtures for the realized-model capture tests — used by this
    //! module's tests and by `cmd::realized_watch`'s: journal seeding, a
    //! Running molecule skeleton, crash simulation, fleet-worker registration,
    //! and the serialized `HOME` swap (capture resolves session logs under
    //! `$HOME`, so tests that redirect it must not overlap).

    use super::*;
    use std::sync::Mutex;

    /// Serializes every test that swaps `HOME` to a fixture root.
    pub(crate) static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Take the `HOME` serialization lock **and neutralize
    /// `CLAUDE_CONFIG_DIR`** for the duration of the test.
    ///
    /// Since task-20260727-3f46 the session-log root honours
    /// `CLAUDE_CONFIG_DIR` in preference to `$HOME/.claude` — correctly, since
    /// that is what Claude Code itself does. But it means a test that
    /// redirects `HOME` to a fixture no longer controls the resolution: on any
    /// developer machine that exports `CLAUDE_CONFIG_DIR` (this fleet's own
    /// multi-account setup does), the fixture would be ignored and the test
    /// would read the operator's real logs. The guard removes the variable on
    /// acquire and restores it on drop, so a fixture `HOME` means what it says.
    pub(crate) fn home_guard() -> HomeGuard {
        // Poisoning is expected here: a test that panics mid-swap leaves the
        // mutex poisoned, and every later test would fail for a reason that
        // has nothing to do with it. The lock serializes; it guards no
        // invariant a panic could have broken.
        let lock = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        HomeGuard {
            _lock: lock,
            previous,
        }
    }

    /// RAII guard returned by [`home_guard`]: holds the serialization lock and
    /// restores the ambient `CLAUDE_CONFIG_DIR` when the test ends.
    pub(crate) struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
                None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
            }
        }
    }

    /// Seed the dispatch journal: `AdapterSelected` + `WorkerSpawned`.
    pub(crate) fn seed_dispatch(state_dir: &Path, mol: &MoleculeId, adapter: &str, worker: &str) {
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

    /// Persist a minimal Running molecule so status-driven loops (`cs wait`,
    /// the realized watcher) see a live run.
    pub(crate) fn seed_running_molecule(
        state_dir: &Path,
        mol: &MoleculeId,
    ) -> cosmon_filestore::FileStore {
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
            base_branch: None,
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
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        };
        use cosmon_state::StateStore as _;
        store.save_molecule(mol, &data).unwrap();
        store
    }

    /// Probe one registered worker's energy through the **production** batch
    /// path ([`super::load_worker_energy`]): fold the journal once, resolve
    /// the adapter from that fold, then probe. Tests use this rather than a
    /// bespoke single-shot probe so they exercise exactly the code `cs peek`
    /// runs — including [`super::fold_last_adapters`] and the
    /// adapter-routing it feeds [`super::probe_worker_energy_with_adapter`].
    pub(crate) fn probe_one_worker_energy(
        state_dir: &Path,
        worker: &str,
    ) -> Option<super::WorkerEnergy> {
        use cosmon_state::StateStore as _;
        let store = cosmon_filestore::FileStore::new(state_dir);
        let fleet = store.load_fleet().unwrap();
        super::load_worker_energy(state_dir, &[], &fleet)
            .get(&WorkerId::new(worker).unwrap())
            .copied()
    }

    /// Register `worker` on the fleet with its working directory recorded
    /// relative to the project root — exactly what `cs tackle`'s
    /// `register_tackle_worker` persists, and the join key the post-mortem
    /// capture falls back to when no pane answers.
    pub(crate) fn register_fleet_worker(
        state_dir: &Path,
        mol: &MoleculeId,
        worker: &str,
        repo_rel: &str,
    ) {
        use cosmon_state::StateStore as _;
        let store = cosmon_filestore::FileStore::new(state_dir);
        let mut fleet = store.load_fleet().unwrap_or_default();
        let mut data = cosmon_state::WorkerData::new(
            WorkerId::new(worker).unwrap(),
            cosmon_core::id::AgentId::new("tackle").unwrap(),
            cosmon_core::agent::AgentRole::Implementation,
            cosmon_core::clearance::Clearance::Write,
            cosmon_core::worker::WorkerStatus::Active,
        );
        data.repo = Some(repo_rel.to_owned());
        data.current_molecule = Some(mol.clone());
        fleet.workers.insert(WorkerId::new(worker).unwrap(), data);
        store.save_fleet(&fleet).unwrap();
    }

    /// Simulate the worker's death on the journal (pane died, exit 137).
    pub(crate) fn crash_worker(state_dir: &Path, mol: &MoleculeId) {
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

    /// Fold the molecule-scoped journal into an [`AdapterAttribution`].
    pub(crate) fn fold_from_log(
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
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    /// The batched fold keeps only the **last** `AdapterSelected` per
    /// molecule (forward scan, overwrite), and each molecule is independent.
    /// This is the map `load_worker_energy` reads instead of re-scanning the
    /// whole journal per worker.
    #[test]
    fn fold_last_adapters_keeps_last_selection_per_molecule() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path();
        let a = MoleculeId::new("task-20260720-aaaa").unwrap();
        let b = MoleculeId::new("task-20260720-bbbb").unwrap();

        // `a` is re-selected (claude → codex); `b` is selected once.
        seed_dispatch(state_dir, &a, "claude", "w-a1");
        seed_dispatch(state_dir, &b, "codex", "w-b1");
        seed_dispatch(state_dir, &a, "codex", "w-a2");

        let map = fold_last_adapters(state_dir);
        assert_eq!(map.get(&a).map(String::as_str), Some("codex"), "last wins");
        assert_eq!(map.get(&b).map(String::as_str), Some("codex"));
        assert_eq!(map.len(), 2);
    }

    /// A molecule with no recorded selection is absent from the fold, so the
    /// caller's `adapters.get(m)` yields `None` — the legacy "treat as
    /// claude" path, exactly as `last_adapter_for` returned `None` before.
    #[test]
    fn fold_last_adapters_omits_molecules_without_a_selection() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(fold_last_adapters(tmp.path()).is_empty());
    }

    /// Non-regression on the **fold cost** (task-20260720-6699): reading
    /// energy for a fleet of many workers must fold the journal *once*, not
    /// once per worker. The previous shape re-parsed the whole `events.jsonl`
    /// inside the worker loop — an `O(W × J)` blowup that made `cs peek` take
    /// ~11 s on an aged galaxy (94 workers × an 80k-line journal). Here a
    /// 64-worker fleet over a ~4k-line journal would fold ~256k envelopes
    /// under the old shape and ~4k under the batched one; the coarse ceiling
    /// separates the two by more than an order of magnitude without asserting
    /// a brittle absolute latency.
    #[test]
    fn load_worker_energy_does_not_scale_with_worker_count() {
        use std::time::Instant;

        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path();

        // 64 workers, each on its own molecule with a recorded selection.
        // `claude` with no live pane resolves to `None` fast (no pid
        // sidecar), so per-worker cost is dominated purely by the journal
        // read the fix removes.
        const WORKERS: usize = 64;
        let mut fleet = cosmon_state::Fleet::default();
        for i in 0..WORKERS {
            let mol = MoleculeId::new(&format!("task-20260720-{i:04x}")).unwrap();
            seed_dispatch(state_dir, &mol, "claude", &format!("w-{i:04}"));
            let wid = WorkerId::new(&format!("w-{i:04}")).unwrap();
            let mut data = cosmon_state::WorkerData::new(
                wid.clone(),
                cosmon_core::id::AgentId::new("polecat").unwrap(),
                cosmon_core::agent::AgentRole::Implementation,
                cosmon_core::clearance::Clearance::Write,
                cosmon_core::worker::WorkerStatus::Active,
            );
            data.current_molecule = Some(mol);
            fleet.workers.insert(wid, data);
        }
        {
            use cosmon_state::StateStore as _;
            cosmon_filestore::FileStore::new(state_dir)
                .save_fleet(&fleet)
                .unwrap();
        }

        let started = Instant::now();
        let energy = load_worker_energy(state_dir, &[], &fleet);
        let elapsed = started.elapsed();

        // No live sessions, so no energy resolves — the point is the fold
        // cost, not the token values.
        assert!(energy.is_empty());
        // The batched fold is milliseconds; the `O(W × J)` regression would
        // be well into the seconds on this fixture. A 3 s ceiling is far
        // above the batched path on any CI host yet far below the quadratic
        // shape.
        assert!(
            elapsed.as_secs() < 3,
            "load_worker_energy folded the journal per worker again \
             (took {elapsed:?} for {WORKERS} workers) — the O(W × J) \
             regression is back",
        );
    }

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

    // ---- task-20260727-3f46: the session-log root actually used ----------

    /// The defect, stated as a test: cosmon exports `CLAUDE_CONFIG_DIR` onto
    /// the worker it spawns, so the resolver that reads the worker's session
    /// log must honour the same variable. Before the fix it resolved
    /// `$HOME/.claude/projects` unconditionally — a directory that does not
    /// exist at all in a container deployment.
    #[test]
    fn projects_dir_honours_claude_config_dir_over_home() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let routed = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        assert_eq!(claude_projects_dir(), home.path().join(".claude/projects"));
        assert_eq!(claude_sessions_dir(), home.path().join(".claude/sessions"));

        std::env::set_var("CLAUDE_CONFIG_DIR", routed.path());
        assert_eq!(claude_projects_dir(), routed.path().join("projects"));
        assert_eq!(claude_sessions_dir(), routed.path().join("sessions"));

        // An empty value is not a path to the filesystem root — it means
        // "unset", exactly as an absent variable does.
        std::env::set_var("CLAUDE_CONFIG_DIR", "");
        assert_eq!(claude_projects_dir(), home.path().join(".claude/projects"));

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// The same defect end to end, through the real capture core: the worker
    /// wrote its log under the routed config dir and `$HOME/.claude` does not
    /// exist — the exact shape measured in the container. The observation must
    /// still land.
    #[test]
    fn capture_finds_the_session_log_under_a_routed_claude_config_dir() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let routed = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        std::env::set_var("CLAUDE_CONFIG_DIR", routed.path());

        // `$HOME/.claude` is deliberately never created: in the container it
        // did not exist, and the old resolver looked there anyway.
        assert!(!home.path().join(".claude").exists());

        let proj = routed
            .path()
            .join("projects")
            .join(sanitize_path(&cwd.path().to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260727-1f01").unwrap();
        seed_dispatch(state.path(), &mol, "claude", "worker-1");
        capture_realized_from_cwd(state.path(), &mol, cwd.path());

        assert_eq!(
            fold_from_log(state.path(), &mol).realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec![
                "claude-opus-4-8".to_string()
            ]),
        );

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// The watcher's path: the root comes from the dispatcher on the command
    /// line, not from this process's environment. This is the `cb next` case,
    /// where the worker's `~/.claude-accounts/<email>/` is named by no
    /// variable the detached child inherits — so an explicit root must
    /// override whatever the environment happens to say.
    #[test]
    fn capture_under_an_explicit_root_ignores_the_ambient_environment() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let decoy = tempfile::TempDir::new().unwrap();
        let account = tempfile::TempDir::new().unwrap();
        let state = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        // The environment points somewhere else entirely — and stays wrong.
        std::env::set_var("CLAUDE_CONFIG_DIR", decoy.path());

        let encoded = sanitize_path(&cwd.path().to_string_lossy());
        let real = claude_projects_dir_under(account.path()).join(&encoded);
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(
            real.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-5\"}}\n",
        )
        .unwrap();

        let mol = MoleculeId::new("task-20260727-1f02").unwrap();
        seed_dispatch(state.path(), &mol, "claude", "worker-1");
        capture_realized_from_cwd_under(
            state.path(),
            &mol,
            cwd.path(),
            Some(&claude_projects_dir_under(account.path())),
        );

        assert_eq!(
            fold_from_log(state.path(), &mol).realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec![
                "claude-sonnet-5".to_string()
            ]),
            "the explicitly-named root must win over the ambient variable"
        );

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    // ---- F-01/F-06 end-to-end: completion-seam capture -------------------
    //
    // fixture session file on disk → `capture_realized_from_cwd` (the real
    // completion seam) → `ModelObserved` on `events.jsonl` → retrospective
    // fold → `compact_cell`. Exercised for claude and codex, which resolve
    // their session by the worker's cwd. HOME is swapped to a fixture root;
    // the swap is serialized so a parallel test never sees the borrowed HOME.

    #[test]
    fn capture_at_completion_claude_end_to_end() {
        let _guard = test_support::home_guard();
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
        let _guard = test_support::home_guard();
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

    // ---- Codex energy: adapter-aware probe (idea-20260718-e622) -----------
    //
    // fixture rollout log on disk → `probe_worker_energy` routed by the
    // journal's `AdapterSelected: codex` → cwd join → token parse → pricing
    // → `WorkerEnergy`. The pane is dead in the fixture (no backend), so the
    // cwd resolves through the fleet-recorded worktree — the same
    // post-mortem fallback the realized-model capture uses.

    /// Seed a codex rollout log carrying a `session_meta.cwd` join key, a
    /// `turn_context` model, and one cumulative `token_count` line.
    fn seed_codex_rollout(cwd: &Path, model: &str) {
        let sess = codex_sessions_dir().join("2026").join("07").join("19");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("rollout-x.jsonl"),
            format!(
                concat!(
                    r#"{{"type":"session_meta","payload":{{"cwd":"{cwd}","session_id":"s"}}}}"#,
                    "\n",
                    r#"{{"type":"turn_context","payload":{{"model":"{model}"}}}}"#,
                    "\n",
                    r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":2000000,"cached_input_tokens":1000000,"output_tokens":100000,"reasoning_output_tokens":40000,"total_tokens":2100000}}}}}}}}"#,
                    "\n",
                ),
                cwd = cwd.to_string_lossy(),
                model = model,
            ),
        )
        .unwrap();
    }

    /// A codex dispatch fills the row with real tokens and a
    /// realized-model-priced cost — same fidelity class as a claude row.
    #[test]
    fn codex_worker_energy_probes_tokens_and_priced_cost() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260719-e401").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        seed_codex_rollout(&wt, "gpt-5.6-terra");

        seed_dispatch(&state_dir, &mol, "codex", "worker-1");
        register_fleet_worker(
            &state_dir,
            &mol,
            "worker-1",
            &format!(".worktrees/{}", mol.as_str()),
        );

        let energy = probe_one_worker_energy(&state_dir, "worker-1")
            .expect("codex energy must resolve through the recorded worktree");
        let (input, output, cost) = energy.as_tuple();
        assert_eq!(input, 2_000_000, "input includes the cached portion");
        assert_eq!(output, 100_000);
        // 1M fresh × $2.50 + 1M cached × $0.25 + 100k out × $15 = $4.25.
        assert!((cost - 4.25).abs() < 1e-9);

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// Honest floor: an unpriced model keeps the real token counts and
    /// reports `cost = 0.0`, which the COST column renders as `—`.
    #[test]
    fn codex_worker_energy_unpriced_model_keeps_tokens_zero_cost() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260719-e402").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        seed_codex_rollout(&wt, "gpt-7-hypothetical");

        seed_dispatch(&state_dir, &mol, "codex", "worker-1");
        register_fleet_worker(
            &state_dir,
            &mol,
            "worker-1",
            &format!(".worktrees/{}", mol.as_str()),
        );

        let energy = probe_one_worker_energy(&state_dir, "worker-1")
            .expect("tokens stay computable for an unpriced model");
        let (input, output, cost) = energy.as_tuple();
        assert_eq!(input, 2_000_000);
        assert_eq!(output, 100_000);
        assert!(cost.abs() < f64::EPSILON, "no fabricated rate — cost is 0");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// A claude dispatch never takes the codex branch: with no live pane and
    /// no pid sidecar the claude chain resolves to `None`, even when a codex
    /// rollout log happens to name the same cwd.
    #[test]
    fn claude_worker_never_reads_codex_rollout() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260719-e403").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        seed_codex_rollout(&wt, "gpt-5.6-terra");

        seed_dispatch(&state_dir, &mol, "claude", "worker-1");
        register_fleet_worker(
            &state_dir,
            &mol,
            "worker-1",
            &format!(".worktrees/{}", mol.as_str()),
        );

        let energy = probe_one_worker_energy(&state_dir, "worker-1");
        assert!(
            energy.is_none(),
            "claude chain must stay on the pid-sidecar path"
        );

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

    /// Claude: the FIRST model-bearing turn is captured by the runtime poll
    /// (real `cs wait` loop, probed per tick) → the worker crashes before any
    /// `cs complete` → the observation is durable and the fold renders it.
    #[test]
    fn runtime_first_turn_observation_survives_claude_crash_before_complete() {
        let _guard = test_support::home_guard();
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
        let _guard = test_support::home_guard();
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
        let _guard = test_support::home_guard();
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
        let _guard = test_support::home_guard();
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
        let _guard = test_support::home_guard();
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

    // ---- COND-1 (round-4): post-mortem capture, crash BEFORE any tick -----
    //
    // The round-3 audit's strict counter-example, in the CRITICAL order the
    // previous tests inverted: (1) the worker writes its first model-bearing
    // turn, (2) the pane DIES — no tmux pane will ever answer again, (3) only
    // THEN does a poll tick fire. The session JSONL is already durable on
    // disk, so the capture must resolve it without the live pane — via the
    // worker cwd `cs tackle` recorded on the fleet — and still emit.

    /// Claude: turn → pane death → first-ever capture tick. The observation
    /// must be emitted post-mortem through the recorded-cwd fallback (the
    /// backends list resolves no pane, exactly like a dead pane in prod).
    #[test]
    fn post_mortem_capture_claude_turn_then_kill_then_capture() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260719-4a01").unwrap();
        // Canonical project layout: state under .cosmon/state, worker parked
        // in .worktrees/<mol> — the repo path tackle records on the fleet.
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        // (1) The first model-bearing turn is on disk.
        let proj = claude_projects_dir().join(sanitize_path(&wt.to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        )
        .unwrap();

        seed_running_molecule(&state_dir, &mol);
        seed_dispatch(&state_dir, &mol, "claude", "worker-1");
        register_fleet_worker(
            &state_dir,
            &mol,
            "worker-1",
            &format!(".worktrees/{}", mol.as_str()),
        );

        // (2) The pane dies. No capture has run yet — zero observations.
        crash_worker(&state_dir, &mol);

        // (3) THEN the first tick fires. No backend can answer for the dead
        // pane; resolution must go through the fleet-recorded cwd.
        capture_realized_runtime(&state_dir, &mol, &[]);

        let att = fold_from_log(&state_dir, &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec![
                "claude-opus-4-8".to_string()
            ]),
            "a crash before the first tick must not lose the durable turn"
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// Codex: same turn → kill → capture order through the codex
    /// `session_meta.payload.cwd` join — pane-independent by construction.
    #[test]
    fn post_mortem_capture_codex_turn_then_kill_then_capture() {
        let _guard = test_support::home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260719-4a02").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let sess = codex_sessions_dir().join("2026").join("07").join("19");
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
                cwd = wt.to_string_lossy()
            ),
        )
        .unwrap();

        seed_running_molecule(&state_dir, &mol);
        seed_dispatch(&state_dir, &mol, "codex", "worker-1");
        register_fleet_worker(
            &state_dir,
            &mol,
            "worker-1",
            &format!(".worktrees/{}", mol.as_str()),
        );

        crash_worker(&state_dir, &mol);
        capture_realized_runtime(&state_dir, &mol, &[]);

        let att = fold_from_log(&state_dir, &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Observed(vec!["gpt-5.6-terra".to_string()]),
            "codex post-mortem resolution must survive the dead pane"
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}
