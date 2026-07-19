// SPDX-License-Identifier: AGPL-3.0-only

//! `cs realized-watch` — internal first-turn realized-model watcher.
//!
//! Spawned detached by `cs tackle` for every subprocess session-log adapter
//! (claude/codex), this is the consumer that makes D4's cadence real: emit
//! `ModelObserved` on the **first assistant turn** carrying a concrete model
//! id (delib-20260718-c70e / D4), not "at some later poll while the worker
//! happens to still be alive". `cs wait` / `cs run` remain opportunistic
//! re-capture surfaces, but neither is guaranteed to be running — `cs tackle`
//! does not launch `cs wait`, and the default poll is five seconds. This
//! watcher is attached to the dispatch itself, so the guarantee holds even
//! when nobody watches.
//!
//! Resolution is **pane-independent by construction**: the worker's working
//! directory is passed on the command line (tackle knows the worktree it just
//! created), and the capture core resolves the session JSONL from that cwd
//! alone. A worker that crashes between its first turn and the next tick
//! therefore loses nothing — the session log is already durable on disk and
//! the next tick still reads it (round-4 / COND-1 post-mortem property).
//!
//! Lifecycle (ADR-016-aligned — bounded, never a daemon): tick at
//! `--interval-ms` while the molecule is Pending/Queued/Running, then fire
//! one final capture (turns written after the last tick, or after a crash)
//! and exit. A hard `--timeout-secs` bounds the run even when a crashed
//! worker's molecule is never harvested. Hidden from help: this is dispatch
//! plumbing, not an operator verb — and it deliberately does not reuse the
//! bare verb `observe`, reserved for read-only surfaces (D2/wheeler).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::StateStore as _;

use super::Context;

/// Arguments for the hidden `realized-watch` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID whose worker session to watch.
    pub molecule: String,

    /// The worker's working directory (the worktree `cs tackle` created) —
    /// the pane-independent join key to the claude/codex session log.
    #[arg(long)]
    pub cwd: PathBuf,

    /// Milliseconds between capture ticks. The default keeps the
    /// first-turn latency within one second of the turn landing on disk.
    #[arg(long, default_value_t = 1000)]
    pub interval_ms: u64,

    /// Hard upper bound on the watch, in seconds. Bounds the process even
    /// when a crashed worker's molecule is never moved out of Running.
    #[arg(long, default_value_t = 21_600)]
    pub timeout_secs: u64,
}

/// Execute the `realized-watch` command.
///
/// # Errors
///
/// Returns an error only for an invalid molecule id; the watch itself is
/// best-effort and never fails (trace-not-lock).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id =
        MoleculeId::new(&args.molecule).map_err(|e| anyhow::anyhow!("invalid molecule id: {e}"))?;
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    watch_realized(
        &state_dir,
        &mol_id,
        &args.cwd,
        Duration::from_millis(args.interval_ms.max(1)),
        Duration::from_secs(args.timeout_secs),
    );
    Ok(())
}

/// The watch loop: capture every `interval` while the molecule is live, then
/// one final post-exit capture. Extracted from [`run`] so tests can drive it
/// with a fixture state dir and millisecond cadence.
///
/// Each tick runs the same idempotent capture core as the completion seam
/// (`capture_realized_from_cwd`): first observation emits, unchanged
/// trajectories emit nothing, on-change re-emits the new tail (D4). The final
/// capture after the molecule leaves the live set covers turns written
/// between the last tick and the worker's exit — including a crash, where the
/// session log outlives the pane.
pub fn watch_realized(
    state_dir: &Path,
    mol_id: &MoleculeId,
    cwd: &Path,
    interval: Duration,
    timeout: Duration,
) {
    let store = FileStore::new(state_dir);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && molecule_is_live(&store, mol_id) {
        crate::energy_probe::capture_realized_from_cwd(state_dir, mol_id, cwd);
        std::thread::sleep(interval);
    }
    // Final sweep: anything the worker wrote after the last tick — or, when
    // it crashed, the durable turns its dead pane can no longer report.
    crate::energy_probe::capture_realized_from_cwd(state_dir, mol_id, cwd);
}

/// Whether the molecule still counts as a live run worth ticking on. A
/// missing/unreadable molecule (harvested, archived) ends the watch.
fn molecule_is_live(store: &FileStore, mol_id: &MoleculeId) -> bool {
    store.load_molecule(mol_id).is_ok_and(|m| {
        matches!(
            m.status,
            MoleculeStatus::Pending | MoleculeStatus::Queued | MoleculeStatus::Running
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::energy_probe::test_support::{
        crash_worker, fold_from_log, seed_dispatch, seed_running_molecule, HOME_LOCK,
    };
    use crate::energy_probe::{claude_projects_dir, sanitize_path};
    use cosmon_core::event_v2::EventV2;

    /// COND-1 first-turn seam, end to end and in the critical order:
    /// the watcher is attached at dispatch (before any turn exists), the
    /// worker then writes its FIRST model-bearing turn, and the observation
    /// lands on `events.jsonl` while the molecule is still Running — with no
    /// `cs wait`, no `cs run`, and no `cs complete` anywhere. The worker is
    /// then killed; the already-durable observation survives, and the dedup
    /// keeps the journal at exactly one line.
    #[test]
    fn watcher_emits_on_first_turn_before_crash_without_wait_or_complete() {
        let _guard = HOME_LOCK.lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260719-4a03").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let store = seed_running_molecule(&state_dir, &mol);
        seed_dispatch(&state_dir, &mol, "claude", "worker-1");

        // The watcher starts at dispatch — BEFORE any turn exists.
        let watcher = {
            let state_dir = state_dir.clone();
            let mol = mol.clone();
            let wt = wt.clone();
            std::thread::spawn(move || {
                watch_realized(
                    &state_dir,
                    &mol,
                    &wt,
                    Duration::from_millis(5),
                    Duration::from_secs(30),
                )
            })
        };

        // The worker produces its FIRST model-bearing turn mid-run.
        let proj = claude_projects_dir().join(sanitize_path(&wt.to_string_lossy()));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
        )
        .unwrap();

        // The observation must appear while the molecule is still Running.
        let log = cosmon_state::event_log::resolve_events_log_path(&state_dir);
        let observed_live = std::iter::repeat_with(|| {
            std::thread::sleep(Duration::from_millis(10));
            cosmon_state::event_log::read_all(&log)
                .unwrap_or_default()
                .iter()
                .any(|e| matches!(e.event, EventV2::ModelObserved { .. }))
        })
        .take(500)
        .any(|seen| seen);
        assert!(
            observed_live,
            "first turn must be observed during the run — no wait/run/complete involved"
        );

        // NOW the worker is killed, and the molecule leaves the live set so
        // the watcher winds down (in prod: harvest/collapse does this).
        crash_worker(&state_dir, &mol);
        let mut data = store.load_molecule(&mol).unwrap();
        data.status = MoleculeStatus::Collapsed;
        store.save_molecule(&mol, &data).unwrap();
        watcher.join().unwrap();

        let events = cosmon_state::event_log::read_all(&log).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, EventV2::ModelObserved { .. }))
                .count(),
            1,
            "many ticks, one observation — the dedup holds across the crash"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.event, EventV2::MoleculeCompleted { .. })),
            "no completion ever happened — the emission cannot be teardown-borne"
        );
        let att = fold_from_log(&state_dir, &mol);
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
}
