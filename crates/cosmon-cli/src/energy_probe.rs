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
    realized_models_from_claude_jsonl, ModelObservationSource,
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

/// Best-effort **realized-model capture** for one live claude worker
/// (delib-20260718-c70e).
///
/// Resolves the worker's Claude Code session `*.jsonl` (the same
/// pid → session → jsonl chain as [`probe_worker_energy`]), parses the realized
/// model trajectory (`message.model` per assistant turn), and emits an
/// [`EventV2::ModelObserved`] for every model **not already recorded** for the
/// molecule — the first-observation + on-change cadence, idempotent under the
/// repeated reloads a live TUI performs.
///
/// This is *opportunistic* capture: `cs peek` is the live observer that already
/// pays the cost of reading the session for energy, so folding the realized-id
/// out of the same bytes is nearly free. It is best-effort and trace-not-lock —
/// any I/O failure yields no observation, never an error. (A future always-on
/// emitter in the supervision loop would capture even when nothing is watching;
/// until then, the observer that looks is the one that records.)
pub fn capture_realized_for_worker(
    backends: &[cosmon_transport::TmuxBackend],
    state_dir: &Path,
    worker_id: &WorkerId,
    mol_id: &MoleculeId,
) {
    let Some(pid) = resolve_tmux_pid(backends, worker_id) else {
        return;
    };
    let Some((session_id, cwd)) = read_claude_pid_file(pid) else {
        return;
    };
    let jsonl_path = claude_projects_dir()
        .join(sanitize_path(&cwd))
        .join(format!("{session_id}.jsonl"));
    let Ok(content) = std::fs::read_to_string(&jsonl_path) else {
        return;
    };
    let observed = realized_models_from_claude_jsonl(&content);
    if observed.is_empty() {
        return;
    }
    let recorded = recorded_realized_models(state_dir, mol_id);
    for model in newly_observed(&recorded, &observed) {
        cosmon_state::events::worker_spawn::emit_model_observed(
            state_dir,
            mol_id,
            "claude",
            model,
            ModelObservationSource::ClaudeStreamJson,
        );
    }
}

/// The realized models already on the wire for `mol_id`, in append order —
/// folded from the [`EventV2::ModelObserved`] events in `events.jsonl`. Any I/O
/// error yields an empty list (best-effort), so a first observation is emitted.
fn recorded_realized_models(state_dir: &Path, mol_id: &MoleculeId) -> Vec<String> {
    let log_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let Ok(envelopes) = cosmon_state::event_log::read_all(&log_path) else {
        return Vec::new();
    };
    envelopes
        .into_iter()
        .filter_map(|env| match env.event {
            EventV2::ModelObserved {
                mol_id: ref m,
                model,
                ..
            } if m == mol_id => Some(model),
            _ => None,
        })
        .collect()
}

/// The suffix of `observed` not yet present in `recorded` — the models to emit.
///
/// Pure and total. The common case is monotonic growth: `recorded` is a prefix
/// of `observed` (the same trajectory, fewer turns seen last time), so the new
/// tail is `observed[recorded.len()..]`. When the sequences diverge (they should
/// not, given the collapse-consecutive fold), nothing is emitted — silence is
/// safer than a fabricated re-observation.
fn newly_observed<'a>(recorded: &[String], observed: &'a [String]) -> &'a [String] {
    if recorded.is_empty() {
        return observed;
    }
    if observed.len() > recorded.len() && observed[..recorded.len()] == *recorded {
        return &observed[recorded.len()..];
    }
    &[]
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

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn newly_observed_first_time_emits_all() {
        assert_eq!(newly_observed(&[], &v(&["opus"])), v(&["opus"]).as_slice());
    }

    #[test]
    fn newly_observed_no_change_emits_nothing() {
        // Idempotency: a reload that sees the same trajectory emits nothing.
        assert!(newly_observed(&v(&["opus"]), &v(&["opus"])).is_empty());
    }

    #[test]
    fn newly_observed_growth_emits_only_the_new_tail() {
        // A quota fallback appended sonnet after opus was already recorded.
        assert_eq!(
            newly_observed(&v(&["opus"]), &v(&["opus", "sonnet"])),
            v(&["sonnet"]).as_slice()
        );
    }

    #[test]
    fn newly_observed_divergence_emits_nothing() {
        // Should not happen given the collapse fold; if it does, stay silent
        // rather than fabricate a re-observation.
        assert!(newly_observed(&v(&["opus"]), &v(&["sonnet"])).is_empty());
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
