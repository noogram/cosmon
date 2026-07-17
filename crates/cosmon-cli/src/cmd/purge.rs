// SPDX-License-Identifier: AGPL-3.0-only

//! `cs purge` — infrastructure teardown for workers.
//!
//! Two modes, one verb (ADR-052 §D3):
//!
//! 1. **Sweep** (no positional arg) — remove workers whose fleet entry is
//!    no longer load-bearing. Three populations qualify:
//!    * `desired = Stopped` workers (the pre-existing terminal case),
//!    * `desired = Running` / `desired = Paused` workers whose tmux session
//!      no longer exists — reclassified to [`WorkerStatus::Stale`] on the
//!      way out (the surface-lie bug where fleet read `Running` while the
//!      pane had been dead for hours, and `cs purge` reported "nothing to
//!      purge"), and
//!    * workers bound to a `Completed` / `Collapsed` molecule — the
//!      merge-without-done case. The tmux session may
//!      still be alive (the worker is idling at `❯` after `cs complete`);
//!      we only remove the fleet entry so `cs ensemble` stops reporting
//!      the worker as in flight. Tmux is left untouched — killing it is
//!      policy that belongs to `cs done` or `cs purge <worker> --force`.
//!
//!    The sweep touches no Active/Paused/Unresponsive/Starting/Stopping
//!    workers whose tmux session is still alive AND whose molecule is
//!    still alive — only truly orphaned ones.
//!
//! 2. **Targeted** (`cs purge <worker>`) — purge one specific worker. With
//!    `--force` the tmux session is SIGKILL'd before the fleet entry is
//!    removed, subsuming the former `cs kill` verb. Without `--force` the
//!    worker is expected to already be in a terminal state (graceful path).
//!
//! ADR-052 §D3 collapses `cs kill` + `cs purge` into this one command:
//! both are infrastructure teardown; the difference was always the force
//! flag, not the perimeter.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use cosmon_core::event_v2::{CollapseReason, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{DesiredState, WorkerRole, WorkerStatus};
use cosmon_state::StateStore;
use cosmon_transport::TmuxBackend;

use super::Context;

/// Flip a zombie molecule's `state.json` from `Running` to `Collapsed` when
/// the worker bound to it is being purged because the worker process is gone
/// — dead tmux on the sweep `stale` path, or an explicit
/// `cs purge <worker> --force`.
///
/// This closes the machine-crash zombie window. Before this fix, `cs purge`
/// removed the worker's fleet entry but left `state.json` at
/// `status = running`, so the board read undrained on a raw read and the
/// operator had to `cs collapse` each zombie by hand. The exact pathology
/// hit grace (verify-20260620-7e7b / verify-20260621-2b67) and cosmon (four
/// cosmon-ward molecules left `running` after their workers 401-died; purge
/// removed the workers but left the molecules running).
///
/// Defensive, in the spirit of the briefing seal (CLAUDE.md §briefing
/// seals): only a `Running` molecule is touched — terminal, frozen, pending,
/// and starved molecules are left exactly as they are, so an intentionally
/// suspended molecule is never collapsed out from under the operator. The
/// cause is recorded as [`CollapseCause::ProcessDeath`] and the reason-kind
/// as `worker_crashed` so `cs errors` aggregates it correctly. Any I/O
/// failure is swallowed so the purge hot path never blocks. Returns the
/// molecule id when a flip happened, so the caller can report it.
fn collapse_zombie_molecule(
    store: &dyn StateStore,
    events_path: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
) -> Option<MoleculeId> {
    let mut mol = store.load_molecule(mol_id).ok()?;
    if mol.status != MoleculeStatus::Running {
        return None;
    }
    let prev = mol.status;
    let reason = format!(
        "worker {worker_id} gone (purged); molecule was left running — \
         auto-collapsed by cs purge"
    );
    let kind = CollapseReason::from("worker_crashed".to_owned());

    mol.status = MoleculeStatus::Collapsed;
    mol.collapse_cause = Some(CollapseCause::ProcessDeath);
    mol.collapse_reason = Some(reason.clone());
    mol.collapse_reason_kind = Some(kind.clone());
    mol.collapsed_step = Some(mol.current_step);
    // Terminal transition: clear any inline live-process record so a
    // collapsed molecule never carries a phantom worker pointer (mirrors
    // `cs collapse`).
    if mol.process.is_some() {
        mol.release_process();
    }
    mol.updated_at = Utc::now();
    store.save_molecule(&mol.id.clone(), &mol).ok()?;

    let status_seq = cosmon_state::event_log::emit_one(
        events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol_id.clone(),
            from: prev.to_string(),
            to: "collapsed".to_owned(),
        },
        None,
    )
    .ok();
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        EventV2::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason,
            kind: Some(kind),
        },
        status_seq,
    );
    Some(mol_id.clone())
}

/// Flip every zombie molecule pinned to a `stale` worker (dead tmux = the
/// worker process is gone). Reads each stale worker's `current_molecule`
/// from `fleet` BEFORE the caller reclassifies them (which nulls the
/// binding), and returns the ids of the molecules actually collapsed.
///
/// Orphan workers are excluded by construction: the classifier only files a
/// worker as `orphan` when its molecule is already terminal, so there is no
/// zombie to flip there. The per-molecule `is_running` guard inside
/// [`collapse_zombie_molecule`] makes a double call a no-op.
fn collapse_stale_zombies(
    fleet: &cosmon_state::Fleet,
    store: &dyn StateStore,
    events_path: &Path,
    stale: &[WorkerId],
) -> Vec<String> {
    let mut collapsed = Vec::new();
    for wid in stale {
        if let Some(mid) = fleet
            .workers
            .get(wid)
            .and_then(|w| w.current_molecule.clone())
        {
            if let Some(flipped) = collapse_zombie_molecule(store, events_path, &mid, wid) {
                collapsed.push(flipped.as_str().to_owned());
            }
        }
    }
    collapsed
}

/// Arguments for the `purge` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Optional worker ID — when given, targeted purge of that worker only.
    ///
    /// Without a worker the command sweeps every terminal-state worker
    /// from fleet state (the pre-ADR-052 behaviour). With a worker, only
    /// that worker is removed; pair with `--force` to SIGKILL its tmux
    /// session first (formerly `cs kill`).
    pub worker: Option<String>,

    /// In targeted mode, SIGKILL the tmux session before removing the
    /// fleet entry. Ignored in sweep mode. Supersedes the stand-alone
    /// `cs kill` verb (ADR-052 §D3).
    #[arg(long)]
    pub force: bool,

    /// Only purge workers matching this desired state (default: sweep all
    /// workers — Stopped ones and Running/Paused ones whose tmux session
    /// is gone).
    #[arg(long)]
    pub status: Option<String>,

    /// Restrict the purge to workers matching this role discriminator —
    /// either `cognition` or `runtime` (see `WorkerRole`). Without this
    /// flag `cs purge` removes both runtime and cognition workers that
    /// meet the status predicate; with it, operators can clean up one
    /// half of a runtime+cognition pair without collapsing the other.
    #[arg(long, value_parser = parse_worker_role)]
    pub role: Option<WorkerRole>,
}

fn parse_worker_role(s: &str) -> Result<WorkerRole, String> {
    s.parse::<WorkerRole>().map_err(|e| e.to_string())
}

/// Execute the `purge` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    // Targeted mode — `cs purge <worker> [--force]` (supersedes `cs kill`).
    if let Some(ref worker_name) = args.worker {
        return run_targeted(ctx, store.as_ref(), &state_dir, worker_name, args.force);
    }

    let socket = super::tmux_socket_name(ctx);
    let backend = TmuxBackend::new(&socket);
    run_sweep(ctx, store.as_ref(), &state_dir, &backend, args)
}

/// Populations produced by [`classify_sweep`] — one vec per reason code
/// so each can carry a distinct `WorkerKilled` event message and the
/// operator output can report "3 stale + 2 orphan" rather than a single
/// opaque total.
struct SweepBuckets {
    /// `desired = Stopped` workers — clean terminal state, purge as-is.
    terminal: Vec<WorkerId>,
    /// `desired = Running|Paused` with dead tmux — surface-lie population.
    /// Reclassified to `Stale` on the way out.
    stale: Vec<WorkerId>,
    /// Tmux alive but `current_molecule` is `Completed` / `Collapsed`.
    /// Fleet entry is removed; tmux untouched.
    orphan: Vec<WorkerId>,
}

/// Classify fleet workers into the three sweep populations.
///
/// Split out of [`run_sweep`] both to keep the outer function readable
/// and because the decision is policy (desired-state + molecule
/// terminality + tmux liveness) while the outer function is mechanism
/// (reclassify, remove, emit events).
fn classify_sweep<B: TransportBackend>(
    fleet: &cosmon_state::Fleet,
    store: &dyn StateStore,
    backend: &B,
    filter_desired: Option<DesiredState>,
    filter_role: Option<WorkerRole>,
) -> SweepBuckets {
    // Pre-load molecule terminality for every worker's current_molecule.
    // A miss (unreadable molecule file, unknown id) is treated as
    // non-terminal — the conservative default matching the stale-tmux
    // branch below, since a false-positive orphan reclassify would
    // silently destroy a live worker's fleet entry.
    let mol_terminal: HashMap<MoleculeId, bool> = fleet
        .workers
        .values()
        .filter_map(|w| w.current_molecule.clone())
        .map(|mid| {
            let terminal = store
                .load_molecule(&mid)
                .is_ok_and(|m| m.status.is_terminal());
            (mid, terminal)
        })
        .collect();

    let mut buckets = SweepBuckets {
        terminal: Vec::new(),
        stale: Vec::new(),
        orphan: Vec::new(),
    };

    for worker in fleet.workers.values() {
        if filter_role.is_some_and(|r| worker.worker_role != r) {
            continue;
        }
        if let Some(f) = filter_desired {
            if worker.desired != f {
                continue;
            }
        }
        let mol_is_terminal = worker
            .current_molecule
            .as_ref()
            .and_then(|mid| mol_terminal.get(mid))
            .copied()
            .unwrap_or(false);

        match worker.desired {
            DesiredState::Stopped => buckets.terminal.push(worker.id.clone()),
            DesiredState::Running | DesiredState::Paused => {
                // Probe the transport. An Err here (e.g. tmux not
                // installed on this host, socket permission error) is
                // treated as "alive" — only a definitive `Ok(false)`
                // counts as a stale-tmux verdict.
                let alive = backend.is_alive(&worker.id).unwrap_or(true);
                if !alive {
                    buckets.stale.push(worker.id.clone());
                } else if mol_is_terminal {
                    // tmux alive but molecule Completed/Collapsed — the
                    // fleet entry is the only thing keeping `cs ensemble`
                    // convinced the worker is in flight. Remove the
                    // entry; leave tmux alone (the agent may still be
                    // sitting at `❯` — the operator decides whether to
                    // kill the session).
                    buckets.orphan.push(worker.id.clone());
                }
            }
        }
    }
    buckets
}

/// Sweep-mode purge, parameterised over the transport backend so tests
/// can inject `MockBackend` without spinning up a real tmux server.
#[allow(clippy::too_many_lines)]
fn run_sweep<B: TransportBackend>(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &Path,
    backend: &B,
    args: &Args,
) -> anyhow::Result<()> {
    let mut fleet = store.load_fleet()?;

    let filter_desired: Option<DesiredState> = args
        .status
        .as_ref()
        .map(|s| s.parse())
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid status filter: {e}"))?;
    let filter_role = args.role;

    let SweepBuckets {
        terminal,
        stale,
        orphan,
    } = classify_sweep(&fleet, store, backend, filter_desired, filter_role);

    let total = terminal.len() + stale.len() + orphan.len();
    if total == 0 {
        if ctx.json {
            println!(
                r#"{{"command":"purge","purged":0,"workers":[],"terminal":[],"stale":[],"orphan":[]}}"#
            );
        } else {
            println!("Nothing to purge.");
        }
        return Ok(());
    }

    // Before clearing `current_molecule` below, collapse any zombie
    // molecule still pinned to a stale worker (machine crash / 401-death).
    let events_path = state_dir.join("events.jsonl");
    let zombies_collapsed = collapse_stale_zombies(&fleet, store, &events_path, &stale);

    // Reclassify stale + orphan workers' status so the fleet.json
    // snapshot on disk carries an accurate reason before the entry is
    // removed — any audit tooling that reads the pre-purge projection
    // (e.g. `cs reconcile`) sees `Stale`, not `Running`.
    let now = Utc::now();
    for wid in stale.iter().chain(orphan.iter()) {
        if let Some(w) = fleet.workers.get_mut(wid) {
            w.status = WorkerStatus::Stale;
            w.desired = DesiredState::Stopped;
            w.updated_at = now;
            w.current_molecule = None;
        }
    }

    let mut purged: Vec<String> = Vec::new();
    for wid in terminal.iter().chain(stale.iter()).chain(orphan.iter()) {
        fleet.workers.remove(wid);
        purged.push(wid.as_str().to_owned());
    }

    store.save_fleet(&fleet)?;

    // Emit WorkerKilled events. Distinct `reason` strings let downstream
    // consumers (the overseer, the chronicle sweep, `cs events`) tell
    // the two populations apart without cross-referencing fleet state.
    for wid in &terminal {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "purged".to_owned(),
            },
            None,
        );
    }
    for wid in &stale {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "purged: stale tmux (session missing)".to_owned(),
            },
            None,
        );
    }
    for wid in &orphan {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "purged: orphan (molecule terminal, fleet entry stale)".to_owned(),
            },
            None,
        );
    }

    if ctx.json {
        let out = serde_json::json!({
            "command": "purge",
            "purged": total,
            "workers": purged,
            "terminal": terminal.iter().map(|w| w.as_str().to_owned()).collect::<Vec<_>>(),
            "stale": stale.iter().map(|w| w.as_str().to_owned()).collect::<Vec<_>>(),
            "orphan": orphan.iter().map(|w| w.as_str().to_owned()).collect::<Vec<_>>(),
            "zombies_collapsed": zombies_collapsed,
        });
        println!("{out}");
    } else {
        if !zombies_collapsed.is_empty() {
            println!(
                "Collapsed {} zombie molecule(s) (running → collapsed, cause=process_death):",
                zombies_collapsed.len()
            );
            for mid in &zombies_collapsed {
                println!("  - {mid}");
            }
        }
        if !stale.is_empty() {
            println!(
                "Reclassified {} worker(s) to Stale (tmux session missing).",
                stale.len()
            );
        }
        if !orphan.is_empty() {
            println!(
                "Reclassified {} worker(s) to Stale (molecule terminal, fleet entry orphaned).",
                orphan.len()
            );
        }
        println!("Purged {total} worker(s):");
        for name in &purged {
            println!("  - {name}");
        }
    }

    Ok(())
}

/// Targeted purge — remove a single worker, optionally SIGKILL'ing tmux first.
///
/// Supersedes the legacy `cs kill` verb. With `force = true`, attempts a
/// best-effort graceful exit (short timeout) before force-terminating the
/// tmux session; with `force = false`, only the fleet record is cleaned up
/// (the worker is expected to already have exited). The fleet entry is
/// removed on success, emitting a `WorkerKilled` audit event.
fn run_targeted(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    worker_name: &str,
    force: bool,
) -> anyhow::Result<()> {
    let worker_id = WorkerId::new(worker_name)?;

    let mut fleet = store.load_fleet()?;

    let worker = fleet
        .workers
        .get_mut(&worker_id)
        .ok_or_else(|| anyhow::anyhow!("worker not found: {worker_id}"))?;

    let previous_status = worker.status.to_string();
    // Capture the molecule binding before we null it — a targeted purge of
    // a worker whose molecule is still `running` leaves a crash zombie just
    // like the sweep stale path, so flip it below (the `is_running` guard
    // inside the helper leaves terminal/frozen molecules alone).
    let bound_molecule = worker.current_molecule.clone();
    worker.desired = DesiredState::Stopped;
    worker.status = WorkerStatus::Stopped;
    worker.updated_at = Utc::now();
    worker.current_molecule = None;

    // Force mode: try a quick graceful exit (triggers SessionEnd hooks,
    // memory flush), then terminate what survives.
    let tmux_killed = if force {
        let backend = TmuxBackend::new(super::tmux_socket_name(ctx));
        backend
            .graceful_exit(&worker_id, std::time::Duration::from_secs(5))
            .is_ok()
    } else {
        false
    };

    // Remove the worker from fleet state.
    fleet.workers.remove(&worker_id);
    store.save_fleet(&fleet)?;

    // Emit both legacy and V2 events so the audit trail is identical to
    // the old `cs kill` path (backward-compatible for consumers).
    let events_path = state_dir.join("events.jsonl");

    // Flip a zombie molecule the purged worker left running. Mirrors the
    // sweep stale path so `cs purge <worker> --force` no longer leaves the
    // board reading undrained.
    let zombie_collapsed = bound_molecule
        .as_ref()
        .and_then(|mid| collapse_zombie_molecule(store, &events_path, mid, &worker_id))
        .map(|mid| mid.as_str().to_owned());

    let _ = cosmon_filestore::event::append(
        &events_path,
        &cosmon_core::event::Envelope::now(cosmon_core::event::Event::WorkerKilled {
            worker_id: worker_id.clone(),
        }),
    );
    let reason = if force {
        format!("purged --force (was {previous_status})")
    } else {
        format!("purged (was {previous_status})")
    };
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::WorkerKilled {
            worker_id: worker_id.clone(),
            reason,
        },
        None,
    );

    if ctx.json {
        let out = serde_json::json!({
            "command": "purge",
            "worker_id": worker_id.as_str(),
            "previous_status": previous_status,
            "status": "stopped",
            "force": force,
            "tmux_killed": tmux_killed,
            "purged": 1,
            "workers": [worker_id.as_str()],
            "zombie_collapsed": zombie_collapsed,
        });
        println!("{out}");
    } else {
        let verb = if force { "Force-purged" } else { "Purged" };
        println!("{verb} worker {worker_id} ({previous_status} -> removed)");
        if let Some(mid) = &zombie_collapsed {
            println!(
                "  • collapsed zombie molecule {mid} (running → collapsed, cause=process_death)"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
    use cosmon_transport::MockBackend;
    use tempfile::TempDir;

    use super::*;

    /// Build a minimal [`MoleculeData`] for purge-sweep tests.
    fn sample_mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
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
            tags: std::collections::BTreeSet::new(),
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
        }
    }

    /// Register `worker_id` as an alive tmux session in `backend`.
    ///
    /// Mirrors what `TmuxBackend::is_alive` would return `true` for — the
    /// mock is keyed by worker id, so once this is called the sweep's
    /// liveness probe sees the session as alive and leaves the entry.
    fn register_alive(backend: &MockBackend, worker_id: &str) {
        let agent = AgentDefinition {
            id: AgentId::new(worker_id).unwrap(),
            role: AgentRole::Implementation,
            command: "true".to_owned(),
            args: vec![],
        };
        let _ = backend.spawn(&agent, &RuntimeConfig::default());
    }

    fn ctx_for(tmp: &TempDir, json: bool) -> Context {
        Context {
            verbose: false,
            json,
            config: Some(tmp.path().to_path_buf()),
        }
    }

    fn worker(name: &str, status: WorkerStatus, desired: DesiredState) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("a").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            status,
        );
        w.desired = desired;
        w
    }

    #[test]
    fn test_purge_removes_terminal_workers() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();

        // Active (desired=Running) AND tmux alive — should NOT be purged.
        let w1 = worker("alive", WorkerStatus::Active, DesiredState::Running);
        // Stopped — should be purged.
        let w2 = worker("dead", WorkerStatus::Stopped, DesiredState::Stopped);
        // Error + desired=Stopped — should be purged.
        let w3 = worker(
            "errored",
            WorkerStatus::Error("crash".to_owned()),
            DesiredState::Stopped,
        );

        fleet.workers.insert(w1.id.clone(), w1);
        fleet.workers.insert(w2.id.clone(), w2);
        fleet.workers.insert(w3.id.clone(), w3);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "alive");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(fleet.workers.contains_key(&WorkerId::new("alive").unwrap()));
    }

    #[test]
    fn test_purge_reclassifies_dead_tmux_to_stale() {
        // desired=Running but tmux session is gone → reclassify to Stale
        // and purge. This is the surface-lie fix (task-20260419-5982):
        // before the probe, fleet reported Running + "nothing to purge"
        // while the pane had been dead for hours.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();

        let ghost = worker("ghost", WorkerStatus::Active, DesiredState::Running);
        let live = worker("live", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(ghost.id.clone(), ghost);
        fleet.workers.insert(live.id.clone(), live);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "live");
        // "ghost" deliberately not registered → is_alive returns false.

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1, "ghost worker must be purged");
        assert!(
            fleet.workers.contains_key(&WorkerId::new("live").unwrap()),
            "live worker must survive the sweep"
        );

        // Stale reclassification is traced in the event log.
        let events_path = tmp.path().join("events.jsonl");
        let events = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            events.contains("stale tmux"),
            "WorkerKilled reason must flag the stale-tmux population; got: {events}"
        );
        assert!(
            events.contains("\"worker_id\":\"ghost\""),
            "events.jsonl must name the ghost worker; got: {events}"
        );
    }

    #[test]
    fn test_purge_paused_with_dead_tmux_is_also_reclassified() {
        // desired=Paused matters too — a paused worker whose pane has
        // been SIGKILL'd cannot be resumed. Same treatment as Running.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("paused-ghost", WorkerStatus::Paused, DesiredState::Paused);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty());
    }

    #[test]
    fn test_purge_status_filter_running_only_probes_running() {
        // --status=running should only purge running-with-dead-tmux; a
        // Stopped worker must be left alone even though it would
        // normally qualify in the default sweep.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let ghost = worker("ghost", WorkerStatus::Active, DesiredState::Running);
        let stopped = worker("retired", WorkerStatus::Stopped, DesiredState::Stopped);
        fleet.workers.insert(ghost.id.clone(), ghost);
        fleet.workers.insert(stopped.id.clone(), stopped);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: Some("running".to_owned()),
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(
            fleet
                .workers
                .contains_key(&WorkerId::new("retired").unwrap()),
            "stopped worker must survive --status=running"
        );
    }

    #[test]
    fn test_purge_role_filter_keeps_opposite_half() {
        use cosmon_core::worker::WorkerRole;
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();

        // Stopped Cognition worker — should be purged when --role=cognition.
        let cog = worker("cog-dead", WorkerStatus::Stopped, DesiredState::Stopped);
        // Stopped Runtime worker — should NOT be purged when --role=cognition.
        let mut rt = WorkerData::new(
            WorkerId::new("runtime-dead").unwrap(),
            AgentId::new("runtime").unwrap(),
            AgentRole::Runtime,
            Clearance::Write,
            WorkerStatus::Stopped,
        );
        rt.desired = DesiredState::Stopped;

        fleet.workers.insert(cog.id.clone(), cog);
        fleet.workers.insert(rt.id.clone(), rt);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: Some(WorkerRole::Cognition),
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(fleet
            .workers
            .contains_key(&WorkerId::new("runtime-dead").unwrap()));
    }

    #[test]
    fn test_purge_targeted_removes_single_worker() {
        // `cs purge <worker>` removes the named worker regardless of desired
        // state — it is the explicit operator intent, not a sweep predicate.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("wire", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("wire".to_owned()),
            force: false,
            status: None,
            role: None,
        };
        run(&ctx, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(!fleet.workers.contains_key(&WorkerId::new("wire").unwrap()));
    }

    #[test]
    fn test_purge_targeted_nonexistent_errors() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("ghost".to_owned()),
            force: false,
            status: None,
            role: None,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_purge_nothing_to_purge() {
        // All workers alive and running → nothing to sweep.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("active", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "active");

        let ctx = ctx_for(&tmp, true);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
    }

    #[test]
    #[allow(clippy::items_after_statements)]
    fn test_purge_transport_error_is_conservative() {
        // If the transport can't answer (returns Err), treat the worker
        // as alive — a false-reclassify would silently destroy a real
        // worker's fleet entry, the worst-case failure mode. The test
        // uses a canned-error backend to force the Err path.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("maybe-alive", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        // ErrBackend: always returns Err from is_alive.
        struct ErrBackend;
        impl TransportBackend for ErrBackend {
            fn spawn(
                &self,
                _agent: &AgentDefinition,
                _config: &RuntimeConfig,
            ) -> Result<cosmon_core::transport::SpawnHandle, cosmon_core::transport::TransportError>
            {
                unreachable!()
            }
            fn terminate(
                &self,
                _id: &WorkerId,
            ) -> Result<(), cosmon_core::transport::TransportError> {
                unreachable!()
            }
            fn is_alive(
                &self,
                _id: &WorkerId,
            ) -> Result<bool, cosmon_core::transport::TransportError> {
                Err(cosmon_core::transport::TransportError::Io(
                    "simulated".to_owned(),
                ))
            }
            fn send_input(
                &self,
                _id: &WorkerId,
                _input: &str,
            ) -> Result<(), cosmon_core::transport::TransportError> {
                unreachable!()
            }
            fn capture_output(
                &self,
                _id: &WorkerId,
                _lines: usize,
            ) -> Result<String, cosmon_core::transport::TransportError> {
                unreachable!()
            }
            fn list_sessions(
                &self,
            ) -> Result<
                Vec<cosmon_core::transport::SessionInfo>,
                cosmon_core::transport::TransportError,
            > {
                unreachable!()
            }
            fn graceful_exit(
                &self,
                _id: &WorkerId,
                _timeout: std::time::Duration,
            ) -> Result<bool, cosmon_core::transport::TransportError> {
                unreachable!()
            }
        }

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &ErrBackend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(
            fleet.workers.len(),
            1,
            "transport error must NOT trigger a stale reclassify"
        );
    }

    /// Bind a worker to a molecule via `current_molecule`, parallel to what
    /// `cs tackle` does at spawn time.
    fn worker_with_mol(name: &str, mol: &str) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("a").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        w.desired = DesiredState::Running;
        w.current_molecule = Some(MoleculeId::new(mol).unwrap());
        w
    }

    #[test]
    fn test_purge_orphans_worker_when_molecule_completed_even_if_tmux_alive() {
        // bead ae83: a worker whose molecule was merged via manual
        // cherry-pick (bypassing `cs done`) stays in fleet.json forever
        // — tmux is still alive (agent idling at ❯ after `cs complete`)
        // but the molecule is `Completed`. `cs ensemble` then displays
        // it as `running/active`, noise that drowns the real signal.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        // Persist a Completed molecule and bind a worker to it.
        let mol = sample_mol("task-20260422-ae83", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("orphan-worker-ae83", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        // Tmux is alive for the orphan — the agent is still idling.
        register_alive(&backend, "orphan-worker-ae83");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(
            fleet.workers.is_empty(),
            "orphan worker bound to Completed molecule must be purged"
        );

        // Event reason must flag the new population so the chronicle
        // sweep can distinguish orphan from stale-tmux.
        let events = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
        assert!(
            events.contains("orphan (molecule terminal"),
            "WorkerKilled reason must flag the orphan population; got: {events}"
        );
    }

    #[test]
    fn test_purge_orphans_worker_when_molecule_collapsed() {
        // Symmetric case: collapsed molecule is also terminal. Worker
        // bound to a collapsed molecule is equally orphaned.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260422-c0ld", MoleculeStatus::Collapsed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("orphan-worker-c0ld", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "orphan-worker-c0ld");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty());
    }

    #[test]
    fn test_purge_keeps_worker_when_molecule_still_running() {
        // Guard: a worker bound to a Running molecule with live tmux is
        // the healthy case — the sweep MUST NOT touch it.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260422-live", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("live-worker", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "live-worker");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(
            fleet.workers.len(),
            1,
            "healthy worker must survive the sweep"
        );
    }

    #[test]
    fn test_purge_orphan_missing_molecule_file_is_conservative() {
        // If the molecule file cannot be loaded (deleted, permission
        // error, partial checkout), the terminality check defaults to
        // `false` — we leave the fleet entry alone rather than risk a
        // false-positive orphan reclassify. This mirrors the
        // `transport_error_is_conservative` guarantee on the stale path.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        // Note: no `save_molecule` call — the molecule file is absent.
        let mut fleet = Fleet::new();
        let w = worker_with_mol("bound-to-ghost", "task-20260422-ghst");
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "bound-to-ghost");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(
            fleet.workers.len(),
            1,
            "missing molecule file must NOT trigger an orphan reclassify"
        );
    }

    // -----------------------------------------------------------------
    // Zombie-molecule flip (task-20260622-29e3, cosmon-ward from grace)
    // -----------------------------------------------------------------

    #[test]
    fn test_purge_sweep_collapses_running_molecule_of_stale_worker() {
        // The exact grace / cosmon zombie pathology: a worker's tmux dies
        // (machine crash, 401-death), the molecule is left at status
        // `running`, and the sweep removes the worker. Before the fix the
        // molecule stayed `running` forever — the board read undrained and
        // the operator had to `cs collapse` it by hand. The sweep must now
        // flip it to `Collapsed` with cause `process_death`.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("verify-20260620-7e7b", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("zombie-worker", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        // Deliberately NOT registered → is_alive == false → stale population.

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        // Worker removed.
        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty(), "stale worker must be purged");

        // Molecule flipped to Collapsed / ProcessDeath.
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Collapsed,
            "zombie running molecule must be collapsed by the sweep"
        );
        assert_eq!(
            reloaded.collapse_cause,
            Some(cosmon_core::molecule::CollapseCause::ProcessDeath),
            "cause must be process_death"
        );
        assert_eq!(reloaded.collapsed_step, Some(reloaded.current_step));

        // The flip is traced in the event log.
        let events = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
        assert!(
            events.contains("\"verify-20260620-7e7b\"") && events.contains("collapsed"),
            "events.jsonl must record the molecule collapse; got: {events}"
        );
    }

    #[test]
    fn test_purge_sweep_leaves_completed_molecule_of_stale_worker_untouched() {
        // Guard: a stale worker bound to an already-terminal molecule must
        // NOT have its molecule rewritten — only `Running` molecules are
        // zombies. A Completed molecule stays Completed.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260620-done", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("stale-but-done", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        // Not registered → stale.

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &args).unwrap();

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Completed,
            "terminal molecule must never be rewritten by purge"
        );
        assert!(reloaded.collapse_cause.is_none());
    }

    #[test]
    fn test_purge_targeted_force_collapses_running_molecule() {
        // `cs purge <worker> --force` on a worker whose molecule is still
        // running leaves the same crash zombie as the sweep stale path —
        // flip it to Collapsed / ProcessDeath.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("verify-20260621-2b67", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("force-target", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("force-target".to_owned()),
            force: true,
            status: None,
            role: None,
        };
        run(&ctx, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(!fleet
            .workers
            .contains_key(&WorkerId::new("force-target").unwrap()));

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        assert_eq!(
            reloaded.collapse_cause,
            Some(cosmon_core::molecule::CollapseCause::ProcessDeath)
        );
    }

    #[test]
    fn test_purge_targeted_leaves_completed_molecule_untouched() {
        // Symmetric guard for the targeted path: a Completed molecule is
        // not a zombie and must survive a targeted purge unchanged.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260621-keep", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("target-done", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("target-done".to_owned()),
            force: false,
            status: None,
            role: None,
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Completed);
        assert!(reloaded.collapse_cause.is_none());
    }
}
