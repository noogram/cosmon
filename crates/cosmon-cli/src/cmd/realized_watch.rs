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
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_state::StateStore as _;
use cosmon_transport::TmuxBackend;

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

    /// The Claude configuration root the dispatched worker was launched
    /// under — the directory holding `projects/`.
    ///
    /// The watcher cannot re-derive this: cosmon routes workers through
    /// `CLAUDE_CONFIG_DIR` (multi-account `cb` routing, and every container
    /// deployment), and a `cb next`-derived `~/.claude-accounts/<email>/`
    /// appears in no variable this detached child inherits. So `cs tackle`
    /// tells it. Omitted for codex, and for a claude dispatch whose config
    /// dir is the environment default.
    #[arg(long)]
    pub claude_config_dir: Option<PathBuf>,
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
    // The liveness axis (COSMON #35 §1/§2). `cs patrol` already diagnoses a
    // dead worker correctly, but nothing runs `cs patrol`: a plain `cs tackle`
    // dispatch — the shape the container guide teaches — has no supervisor
    // behind it, so a worker that dies reads `active` until a human happens to
    // type the command. This watcher is the one process every dispatch already
    // leaves running, which makes it the place where the diagnosis costs
    // nothing extra.
    let backend = TmuxBackend::new(super::tmux_socket_name(ctx));
    watch_realized(
        &state_dir,
        &mol_id,
        &args.cwd,
        Duration::from_millis(args.interval_ms.max(1)),
        Duration::from_secs(args.timeout_secs),
        args.claude_config_dir
            .as_deref()
            .map(crate::energy_probe::claude_projects_dir_under)
            .as_deref(),
        Some(&backend),
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
/// `backend`, when supplied, arms the second job this loop does: notice that
/// the worker's session is gone and park the molecule, so a crashed dispatch
/// stops reading `active` with nothing behind it. See [`SessionSentinel`].
pub fn watch_realized(
    state_dir: &Path,
    mol_id: &MoleculeId,
    cwd: &Path,
    interval: Duration,
    timeout: Duration,
    claude_projects_root: Option<&Path>,
    backend: Option<&dyn TransportBackend>,
) {
    let store = FileStore::new(state_dir);
    let deadline = Instant::now() + timeout;
    let root = claude_projects_root;
    // The broken-seam diagnostic fires at most once per watch (and, thanks to
    // the emitter's scoped dedup, at most once per dispatch across watchers).
    let mut reported_missing_root = false;
    let mut sentinel = backend.map(SessionSentinel::new);
    while Instant::now() < deadline && molecule_is_live(&store, mol_id) {
        crate::energy_probe::capture_realized_from_cwd_under(state_dir, mol_id, cwd, root);
        if !reported_missing_root {
            reported_missing_root = report_missing_session_log_root(state_dir, mol_id, root);
        }
        if let Some(s) = sentinel.as_mut() {
            if s.tick(&store, state_dir, mol_id) {
                // The molecule is parked and the loop's liveness predicate is
                // now false; fall through to the final capture below.
                break;
            }
        }
        std::thread::sleep(interval);
    }
    // Final sweep: anything the worker wrote after the last tick — or, when
    // it crashed, the durable turns its dead pane can no longer report.
    crate::energy_probe::capture_realized_from_cwd_under(state_dir, mol_id, cwd, root);
    if !reported_missing_root {
        report_missing_session_log_root(state_dir, mol_id, root);
    }
}

/// Say, once, that the session-log root this watch was given does not exist —
/// so no observation can ever arrive for this dispatch.
///
/// Returns `true` when the condition held and a line was written (or was
/// already on the wire from another watcher), `false` when the root is present
/// and there is nothing to report.
///
/// # Why a watcher that finds nothing must speak
///
/// This is the fix for the half of task-20260727-3f46 that cost the most.
/// Before it, a watcher pointed at a non-existent root ticked once a second
/// for seven and a half minutes and emitted nothing — indistinguishable, from
/// the outside, from a worker that had simply not spoken yet. Every container
/// deployment was in that state, permanently, and it took eight runs to
/// notice. A check that cannot check must report *that*, or it is not a check.
///
/// It is a **diagnostic, not a refusal**: the watch continues afterwards. The
/// dispatch is not the thing that is broken — the operator's configuration is
/// — and failing a worker over a telemetry path would trade a lost
/// observation for lost work (trace-not-lock). The loud half is the display:
/// the fold turns this line into `x (unobservable)` instead of an eternal
/// `... (pending)`.
fn report_missing_session_log_root(
    state_dir: &Path,
    mol_id: &MoleculeId,
    claude_projects_root: Option<&Path>,
) -> bool {
    let Some(root) =
        crate::energy_probe::session_log_root_for(state_dir, mol_id, claude_projects_root)
    else {
        // An adapter with no on-disk session log: nothing to miss.
        return false;
    };
    if root.is_dir() {
        return false;
    }
    // Fail-closed scoping, as for observations: an unscoped diagnostic would
    // be ambiguous forever, so no resolvable worker means no line.
    let Some(worker) = crate::energy_probe::last_worker_for(state_dir, mol_id) else {
        return false;
    };
    let adapter =
        crate::energy_probe::last_adapter_for(state_dir, mol_id).unwrap_or_else(|| "claude".into());
    cosmon_state::events::worker_spawn::emit_model_observation_unavailable_once(
        state_dir, mol_id, &worker, &adapter, &root,
    );
    // The condition is a property of the deployment, not of this tick: once
    // seen, stop looking. `true` even when the emitter deduped it away — the
    // caller's latch is about not re-checking, not about who wrote the line.
    true
}

/// Watches the dispatch's tmux session and parks the molecule when it goes.
///
/// # Why the backstop lives here (COSMON #35 §1 and §2)
///
/// `cs patrol` already gets the diagnosis right — it reports the stale worker
/// and auto-freezes the orphaned molecule. What it lacks is anyone to run it.
/// `cs tackle` starts no supervisor, and `cs run --resident` is not what the
/// container guide teaches, so on the plain dispatch path a `kill -9`'d worker
/// leaves a molecule reading `active` with zero processes behind it —
/// indefinitely, and invisibly, holding a fleet slot. The remediation is
/// patrol's own ([`crate::cmd::patrol::auto_freeze_orphans`], called below):
/// this type only supplies the missing *occasion* to run it, on a process the
/// dispatch already pays for.
///
/// # Two latches, because a false freeze costs work
///
/// * **`seen`** — the sentinel does nothing until it has observed the session
///   present at least once. A watcher that raced its own spawn would otherwise
///   read "no session" before there was one and park a molecule that was
///   starting normally.
/// * **`dead_ticks`** — a single absent reading is not a death certificate
///   (the tmux server can be momentarily unreachable). Two consecutive ones
///   are, which mirrors the `Unresponsive` → `Stale` escalation patrol already
///   uses and costs one extra tick of latency.
///
/// Both latches fail in the same safe direction: at worst the molecule is
/// parked one tick later, or not at all, and `cs patrol` remains the operator's
/// hammer. Never the other direction — a live worker must not be frozen out
/// from under itself.
struct SessionSentinel<'a> {
    backend: &'a dyn TransportBackend,
    seen: bool,
    dead_ticks: u8,
}

/// Consecutive absent readings required before the session counts as gone.
const DEAD_TICKS_TO_PARK: u8 = 2;

impl<'a> SessionSentinel<'a> {
    fn new(backend: &'a dyn TransportBackend) -> Self {
        Self {
            backend,
            seen: false,
            dead_ticks: 0,
        }
    }

    /// Probe once. Returns `true` when this tick parked the molecule.
    fn tick(&mut self, store: &FileStore, state_dir: &Path, mol_id: &MoleculeId) -> bool {
        let Ok(mol) = store.load_molecule(mol_id) else {
            return false;
        };
        // Only a molecule this watcher can speak for: it must still be live,
        // and it must actually own a session. A Direct-API dispatch (no tmux)
        // has nothing here to probe and is left entirely alone.
        if !matches!(mol.status, MoleculeStatus::Queued | MoleculeStatus::Running) {
            return false;
        }
        let Some(session) = mol.tmux_session() else {
            return false;
        };

        // Presence, not liveness: `session_exists` also sees the `pane_dead`
        // carcass, and a carcass is not a working agent — but it is also not
        // the shape this sentinel is for. Treating it as present here is
        // deliberate: `cs tackle --force` now reclaims it (COSMON #35 §3), and
        // freezing on a carcass would race that reclaim.
        match self.backend.session_exists(session) {
            Ok(true) => {
                self.seen = true;
                self.dead_ticks = 0;
                false
            }
            Ok(false) if self.seen => {
                self.dead_ticks = self.dead_ticks.saturating_add(1);
                if self.dead_ticks < DEAD_TICKS_TO_PARK {
                    return false;
                }
                park_dead_dispatch(store, state_dir, mol_id, self.backend)
            }
            // Never seen alive, or the probe itself failed — no verdict.
            _ => false,
        }
    }
}

/// Record the death: mark the worker `Stale`/`Stopped`, then hand the molecule
/// to patrol's own orphan transition.
///
/// Both halves matter, and they fix the two directions of the same lie. The
/// fleet write is what stops `cs peek` printing `status: active` for a corpse;
/// the freeze is what stops the molecule holding a slot as `running`. Calling
/// [`crate::cmd::patrol::auto_freeze_orphans`] rather than re-implementing the
/// transition is the point — one definition of "an orphan is parked like
/// this", reached from two occasions.
///
/// Returns `true` when the molecule was actually transitioned.
fn park_dead_dispatch(
    store: &FileStore,
    state_dir: &Path,
    mol_id: &MoleculeId,
    backend: &dyn TransportBackend,
) -> bool {
    let Ok(mol) = store.load_molecule(mol_id) else {
        return false;
    };
    let Some(worker) = mol.worker().cloned() else {
        return false;
    };

    // Mark the worker dead first. `auto_freeze_orphans` reads its verdict off
    // the fleet (`desired == Stopped`), so this write is not cosmetic — it is
    // the input to the transition below.
    if let Ok(_guard) = store.lock_fleet() {
        if let Ok(mut fleet) = store.load_fleet() {
            if let Some(w) = fleet.workers.get_mut(&worker) {
                w.desired = cosmon_core::worker::DesiredState::Stopped;
                w.status = cosmon_core::worker::WorkerStatus::Stale;
                w.updated_at = chrono::Utc::now();
                let _ = store.save_fleet(&fleet);
            }
        }
    }
    let _ = cosmon_state::event_log::emit_one(
        cosmon_state::event_log::resolve_events_log_path(state_dir),
        cosmon_core::event_v2::EventV2::WorkerKilled {
            worker_id: worker,
            reason: "session gone — parked by the dispatch watcher".to_owned(),
        },
        None,
    );

    let Ok(fleet) = store.load_fleet() else {
        return false;
    };
    crate::cmd::patrol::auto_freeze_orphans(
        store,
        state_dir,
        &fleet,
        std::slice::from_ref(&mol),
        crate::cmd::patrol::RespawnOutcome {
            needs_respawn: &[],
            respawned: &[],
        },
        false,
        Some(backend),
    )
    .is_ok_and(|transitioned| !transitioned.is_empty())
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
        crash_worker, fold_from_log, home_guard, seed_dispatch, seed_running_molecule,
    };
    use crate::energy_probe::{claude_projects_dir, sanitize_path};
    use cosmon_core::event_v2::EventV2;

    /// task-20260727-3f46, the silence half: a watcher pointed at a
    /// session-log root that does not exist must SAY SO — once — instead of
    /// ticking forever and emitting nothing. The resulting fold is
    /// `Unobservable` ("no observation can arrive"), and it survives the
    /// liveness promotion that would otherwise render it `... (pending)`
    /// ("none has arrived yet") for the whole life of the worker.
    #[test]
    fn watcher_reports_a_missing_session_log_root_once_and_never_reads_pending() {
        let _guard = home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260727-4b11").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();

        let store = seed_running_molecule(&state_dir, &mol);
        seed_dispatch(&state_dir, &mol, "claude", "worker-1");

        // The container shape: the root the watcher was handed is simply not
        // there. Many ticks run against it.
        let absent = root.path().join("nowhere").join("projects");
        assert!(!absent.exists());
        let watcher = {
            let state_dir = state_dir.clone();
            let mol = mol.clone();
            let wt = wt.clone();
            let absent = absent.clone();
            std::thread::spawn(move || {
                watch_realized(
                    &state_dir,
                    &mol,
                    &wt,
                    Duration::from_millis(5),
                    Duration::from_secs(30),
                    Some(&absent),
                    None,
                )
            })
        };

        let log = cosmon_state::event_log::resolve_events_log_path(&state_dir);
        let spoke = std::iter::repeat_with(|| {
            std::thread::sleep(Duration::from_millis(10));
            cosmon_state::event_log::read_all(&log)
                .unwrap_or_default()
                .iter()
                .any(|e| matches!(e.event, EventV2::ModelObservationUnavailable { .. }))
        })
        .take(200)
        .any(|seen| seen);
        assert!(
            spoke,
            "a watcher that cannot observe must report that, not tick in silence"
        );

        let mut data = store.load_molecule(&mol).unwrap();
        data.status = MoleculeStatus::Collapsed;
        store.save_molecule(&mol, &data).unwrap();
        watcher.join().unwrap();

        let events = cosmon_state::event_log::read_all(&log).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, EventV2::ModelObservationUnavailable { .. }))
                .count(),
            1,
            "hundreds of ticks, one sentence — the condition is a property of \
             the deployment, not of the moment"
        );
        // The actionable half of the diagnostic: the path that was missing.
        let reported = events
            .iter()
            .find_map(|e| match &e.event {
                EventV2::ModelObservationUnavailable { expected_root, .. } => {
                    Some(expected_root.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(reported, absent.to_string_lossy());

        let mut att = fold_from_log(&state_dir, &mol);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Unobservable,
        );
        // The display half: a live worker does NOT turn this into `pending`.
        att.mark_pending_if_live(true);
        assert_eq!(
            att.realized,
            cosmon_core::adapter_attribution::Realized::Unobservable,
            "`...` promises that waiting will resolve it; here nothing will"
        );
        assert_eq!(att.realized.detail_fragment(), "x");
        assert_eq!(att.realized.disposition(), "unobservable");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// A root that exists is not reported — the diagnostic must not cry wolf
    /// on every healthy dispatch. In particular the *per-cwd* subdirectory is
    /// legitimately absent until the worker writes its first turn; only the
    /// shared root's absence proves the seam is broken.
    #[test]
    fn watcher_stays_quiet_when_the_root_exists_but_the_worker_has_not_written() {
        let _guard = home_guard();
        let home = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mol = MoleculeId::new("task-20260727-4b12").unwrap();
        let state_dir = root.path().join(".cosmon").join("state");
        let wt = root.path().join(".worktrees").join(mol.as_str());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        seed_running_molecule(&state_dir, &mol);
        seed_dispatch(&state_dir, &mol, "claude", "worker-1");

        // The root is there; the worker's own directory inside it is not yet.
        let projects = root.path().join("routed").join("projects");
        std::fs::create_dir_all(&projects).unwrap();

        watch_realized(
            &state_dir,
            &mol,
            &wt,
            Duration::from_millis(5),
            // Deadline already elapsed: the loop body never runs, only the
            // final sweep — enough to prove the quiet path stays quiet.
            Duration::from_secs(0),
            Some(&projects),
            None,
        );

        let log = cosmon_state::event_log::resolve_events_log_path(&state_dir);
        assert!(
            !cosmon_state::event_log::read_all(&log)
                .unwrap()
                .iter()
                .any(|e| matches!(e.event, EventV2::ModelObservationUnavailable { .. })),
            "an empty-but-present root is a worker that has not spoken yet, \
             not a broken seam"
        );

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    /// COND-1 first-turn seam, end to end and in the critical order:
    /// the watcher is attached at dispatch (before any turn exists), the
    /// worker then writes its FIRST model-bearing turn, and the observation
    /// lands on `events.jsonl` while the molecule is still Running — with no
    /// `cs wait`, no `cs run`, and no `cs complete` anywhere. The worker is
    /// then killed; the already-durable observation survives, and the dedup
    /// keeps the journal at exactly one line.
    #[test]
    fn watcher_emits_on_first_turn_before_crash_without_wait_or_complete() {
        let _guard = home_guard();
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
                    None,
                    None,
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
    /// COSMON #35 §1 + §2 — a dead worker must stop reading `active`, without
    /// waiting for a human to type `cs patrol`.
    ///
    /// The reporter `kill -9`'d a worker and sampled every twenty seconds:
    /// `molecule=running, claude_procs=0`, three times over, and `cs peek`
    /// showing `worker … status: active`. `cs patrol` diagnosed it perfectly
    /// — when run by hand. Nothing ran it. This asserts the sentinel closes
    /// that loop from the one process a plain `cs tackle` already leaves
    /// behind: session gone ⇒ molecule parked, worker no longer claiming to
    /// be alive.
    #[test]
    fn the_sentinel_parks_a_molecule_whose_session_has_gone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let mol = MoleculeId::new("task-20260802-5a1b").unwrap();
        let store = seed_running_molecule(&state_dir, &mol);

        // Bind the dispatch: worker `worker-1` owning session `worker-1`.
        let worker = cosmon_core::id::WorkerId::new("worker-1").unwrap();
        let mut data = store.load_molecule(&mol).unwrap();
        data.bind_process(cosmon_core::process::MoleculeProcess::new(
            worker.clone(),
            "worker-1".to_owned(),
        ));
        store.save_molecule(&mol, &data).unwrap();

        let mut fleet = cosmon_state::Fleet::default();
        let mut wdata = cosmon_state::WorkerData::new(
            worker.clone(),
            cosmon_core::id::AgentId::new("worker-1").unwrap(),
            cosmon_core::agent::AgentRole::Implementation,
            cosmon_core::clearance::Clearance::Write,
            cosmon_core::worker::WorkerStatus::Active,
        );
        wdata.desired = cosmon_core::worker::DesiredState::Running;
        wdata.current_molecule = Some(mol.clone());
        fleet.workers.insert(worker.clone(), wdata);
        store.save_fleet(&fleet).unwrap();

        let backend = cosmon_transport::MockBackend::new();
        backend
            .spawn(
                &cosmon_core::transport::AgentDefinition {
                    id: cosmon_core::id::AgentId::new("worker-1").unwrap(),
                    role: cosmon_core::agent::AgentRole::Implementation,
                    command: "claude".to_owned(),
                    args: Vec::new(),
                },
                &cosmon_core::transport::RuntimeConfig::default(),
            )
            .unwrap();

        let mut sentinel = SessionSentinel::new(&backend);
        assert!(
            !sentinel.tick(&store, &state_dir, &mol),
            "a live session must never be parked"
        );

        // The kill -9: the session goes, the molecule does not know yet.
        backend.terminate(&worker).unwrap();
        assert!(
            !sentinel.tick(&store, &state_dir, &mol),
            "one absent reading is not a death certificate"
        );
        assert!(
            sentinel.tick(&store, &state_dir, &mol),
            "two consecutive absent readings must park the molecule"
        );

        assert_eq!(
            store.load_molecule(&mol).unwrap().status,
            MoleculeStatus::Frozen,
            "the orphaned molecule must stop holding a slot as `running`"
        );
        assert_ne!(
            store.load_fleet().unwrap().workers[&worker].desired,
            cosmon_core::worker::DesiredState::Running,
            "and the corpse must stop reading `active` to `cs peek`"
        );
    }

    /// The sentinel must never act on a session it has not first seen alive:
    /// a watcher that races its own dispatch would otherwise park a molecule
    /// that is merely starting.
    #[test]
    fn the_sentinel_never_parks_a_session_it_never_saw() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let mol = MoleculeId::new("task-20260802-5a1c").unwrap();
        let store = seed_running_molecule(&state_dir, &mol);
        let worker = cosmon_core::id::WorkerId::new("worker-1").unwrap();
        let mut data = store.load_molecule(&mol).unwrap();
        data.bind_process(cosmon_core::process::MoleculeProcess::new(
            worker,
            "worker-1".to_owned(),
        ));
        store.save_molecule(&mol, &data).unwrap();

        // Empty backend: the session has never existed as far as this watcher
        // can tell.
        let backend = cosmon_transport::MockBackend::new();
        let mut sentinel = SessionSentinel::new(&backend);
        for _ in 0..5 {
            assert!(!sentinel.tick(&store, &state_dir, &mol));
        }
        assert_eq!(
            store.load_molecule(&mol).unwrap().status,
            MoleculeStatus::Running
        );
    }
}
