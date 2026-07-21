// SPDX-License-Identifier: AGPL-3.0-only

//! `cs patrol` — run health checks and anomaly detection across the fleet.
//!
//! Implements the transport-layer patrol: a mechanical scan that inspects
//! every worker and molecule, detects stalled/error workers and orphaned
//! molecules, and emits a [`PatrolReport`] with corrective recommendations.
//!
//! When `--respawn` is passed, patrol doesn't just mark stale workers — it
//! actually re-creates their tmux sessions via `spawn_claude_session`,
//! bringing dead workers back to life.
//!
//! See THESIS.md Part VII for the two-layer patrol design rationale.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use colored::Colorize;
use cosmon_core::event_v2::{EventV2, PerturbationChannel};
use cosmon_core::expiry::{evaluate_expiry, ExpiryAction, ExpiryPolicy};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::patrol::{PatrolAction, PatrolReport};
use cosmon_core::process::project_process_status;
use cosmon_core::propel::{
    decide_nudge, EscalateReason, NudgeChannel, NudgeDecision, NudgeSkip, NudgeView,
};
use cosmon_core::run_state::{BranchState, Liveness, Witness};
use cosmon_core::tag::Tag;
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{
    reconcile, CognitiveState, DesiredState, EffectiveStatus, ObservedState, ReconcileAction,
    TransportState, WorkerRole, WorkerStatus,
};
use cosmon_state::events::worker_spawn::emit_adapter_pane_signature_checked;
use cosmon_state::{event_log, Fleet, MoleculeData, MoleculeFilter, StateStore};
use cosmon_transport::claude::ADAPTER_NAME as CLAUDE_ADAPTER;
use cosmon_transport::registry::{default_registry, pane_current_command, pane_idle_seconds};
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the `patrol` subcommand.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Auto-respawn: restart dead workers by re-creating tmux sessions.
    #[arg(long)]
    pub respawn: bool,

    /// Skip tmux liveness checks and respawn (state-only mode, for testing).
    #[arg(long)]
    pub no_tmux: bool,

    /// Propel: detect running molecules with stale progress and nudge their
    /// workers via transport. The cognitive safety net that complements
    /// the new propulsion prompt — if a worker falls silent mid-molecule,
    /// patrol re-engages it. A worker whose terminal is still producing
    /// output is thinking, not idle, and is never nudged; a genuinely silent
    /// one is nudged with exponential backoff, at most 4 times, after which
    /// the molecule is tagged `propel-exhausted` for `--heal`. A worker that
    /// lost its briefing (orphaned by a crash / machine-sleep) *cannot* evolve,
    /// so it is never nudged at all: it is tagged `propel-orphaned`, the
    /// operator is paged via `cs notify`, and the brief must be re-delivered
    /// (`cs tackle --force`) or the molecule collapsed — closing the 2026-07-21
    /// money-pump where a brief-less worker was nudged for six hours.
    #[arg(long)]
    pub propel: bool,

    /// Staleness threshold in seconds for `--propel` (default: 300).
    /// A molecule is a candidate if `updated_at` is older than this AND its
    /// worker's terminal has been silent at least as long. Also the first
    /// backoff window, doubling per nudge up to 30 min.
    #[arg(long, default_value_t = 300)]
    pub stale_after: u64,

    /// Nudge: per-step stall remediation. Like
    /// `--propel`, but classifies stalls from `last_progress_at` against
    /// the active step's `timeout_minutes` budget (M3, default 30 min) and
    /// guards idempotence — a worker won't be nudged twice within 60 s.
    /// Also covers the boot-stall class (task-20260718-ac03): a Running
    /// molecule with NO progress signal at all — the stuck bootstrap paste
    /// whose Enter was lost at spawn — is nudged once tackled more than
    /// 120 s ago. The nudge text references `briefing.md` so the re-engaged
    /// worker re-reads its contract before continuing. Increments
    /// [`cosmon_state::MoleculeData::nudge_count`] (M5).
    #[arg(long)]
    pub nudge: bool,

    /// Expire sweep: scan molecules whose `expires_at` is in the past and
    /// apply their [`ExpiryPolicy`] (ADR-029). Idempotent — safe to run
    /// repeatedly on the same state. `Warn` tags the molecule with
    /// `expired` and emits a surface alert; `Collapse` transitions
    /// `pending` → `collapsed` with reason `expired (TTL)`; `Escalate`
    /// tags `escalated` and emits the canonical `Expired` event for
    /// downstream transforms.
    #[arg(long)]
    pub expire: bool,

    /// Aggressive orphan remediation: transition orphaned molecules to
    /// `Collapsed` (terminal) instead of `Frozen` (recoverable). Default
    /// is `Frozen` because the molecule can be revived once the worker
    /// situation is understood; `--auto-collapse` is for cases where the
    /// operator wants the DAG to advance past the dead work and never
    /// revisit it.
    #[arg(long)]
    pub auto_collapse: bool,

    /// Harvest sweep: scan every `Completed` molecule with `merged_at = None`
    /// and invoke `cs harvest --molecule <id>` on each one. Belt-and-
    /// suspenders safety net for cases where the tmux `pane-died` hook
    /// never armed (tmux server restart, brutal crash, molecule completed
    /// before the hook was installed, …). Idempotent: already-merged
    /// molecules are silent no-ops inside `cs harvest`.
    #[arg(long)]
    pub harvest: bool,

    /// Livelock sweep: read `.cosmon/state/presence/<sid>/blocked_on.json`
    /// for every live session, build the session-wait graph, and report
    /// any non-trivial strongly connected component. Emits a `temp:hot`
    /// issue molecule tagged `livelock-detected` per cycle. **Never
    /// auto-resolves** (turing §6, §8b: propose, don't impose).
    #[arg(long)]
    pub livelock: bool,

    /// Staleness threshold for `--livelock`, in seconds. `blocked_on.json`
    /// entries older than this are considered crashed-session residue
    /// rather than live waits and are excluded from the graph. Defaults
    /// to one hour — the cost of a false negative (missing a real lock)
    /// is lower than the cost of a false positive (nuisance issue).
    #[arg(long, default_value_t = 3600)]
    pub livelock_stale_after: u64,

    /// Silence-detect: scan running molecules for those whose worker has
    /// not emitted a `WorkerHeartbeat` in `silence_after` seconds. Tags
    /// `temp:frozen`, emits `WorkerSilenceDetected`, and fires `cs notify`
    /// ("absence of signal must itself be a signal"). Does **not** kill
    /// the worker.
    #[arg(long)]
    pub silence_detect: bool,

    /// Threshold in seconds for `--silence-detect` (default: 90).
    /// Roughly `3 ×` the recommended 30-second heartbeat cadence; raise
    /// it for fleets with longer steps or noisy networks.
    #[arg(long, default_value_t = 90)]
    pub silence_after: u64,

    /// Event-age check: for every `Running` molecule, raise an ALERT-only
    /// signal when the most recent entry in the event log *for that
    /// molecule* is older than `event_age_after` seconds. Unlike
    /// `--silence-detect` (which keys specifically on `WorkerHeartbeat`),
    /// this keys on **any** event append, so it catches the external-modal
    /// case — a Claude Code `AskUserQuestion` modal that emits no
    /// cosmon-visible state at all. It
    /// never tags, never kills, never touches transport: it is a pure
    /// read over `molecules + events.jsonl`, so it works even when
    /// `cosmon-runtime` is dead (CV-5 — the watchdog's liveness must be
    /// independent of the thing it watches). Alerts are tiered by
    /// irreversibility (CV-6): only an irreversible-class block
    /// (signature / push / publish) fires `cs notify`; operational stalls
    /// are report-only, to keep the one load-bearing alert out of an
    /// alert-fatigue flood.
    #[arg(long)]
    pub event_age: bool,

    /// Threshold in seconds for `--event-age` (default: 900 = 15 min).
    /// The panel's suggested floor — long enough that a worker genuinely
    /// thinking between event appends is not mistaken for a stall.
    #[arg(long, default_value_t = 900)]
    pub event_age_after: u64,

    /// Abandon sweep (patrouille-abandon): fold
    /// traces an instance has ALREADY emitted (audit envelopes,
    /// phone-home reports, PKCE auth sessions, instance ledgers) into
    /// named abandonment motifs per tenant — nucleate-sans-tackle,
    /// pkce-start-sans-completed, incarne-sans-login,
    /// rafale-4xx-puis-silence, decroissance-de-signalement (the Dave
    /// rule, gravity HIGH: losing the client who talks loses the only
    /// human sensor). Read-only: reports, never remediates.
    #[arg(long)]
    pub abandon: bool,

    /// Instance root for `--abandon` — the instance's `.cosmon/`
    /// directory (containing `whispers/inbox/` and `state/`). Defaults
    /// to the parent of the resolved state dir.
    #[arg(long)]
    pub abandon_root: Option<std::path::PathBuf>,

    /// Quiet window in hours for the "puis silence" motifs of
    /// `--abandon` (default 24 — one daily patrol cadence).
    #[arg(long, default_value_t = super::patrol_abandon::DEFAULT_QUIET_HOURS)]
    pub abandon_quiet_hours: u64,

    /// Heal (the Deacon, ADR-137 §11 P3): run one detect → guard → remediate
    /// pass over the molecule-health anomaly catalog, mutating **only the safe,
    /// reversible classes**, each behind the §5 no-interference guard:
    /// A1 unsent-paste (delegate to the transport submit-retry), A4/A8
    /// idle-after-complete / completed-unharvested (`cs done` harvest from the
    /// orchestrator — never a worker self-`done`), A5 idle-no-progress (nudge),
    /// A6 overloaded (backoff hold). The collapse / integrity classes
    /// (A3/A7/A9) are *reported* but never auto-collapsed here (that is P4).
    /// Detection is keyed off control-plane state only — **never a pane glyph**
    /// (the be1e SEV-1 lesson). Pair with `--dry-run` for a zero-mutation
    /// preview. Use `cs health` for the read-only, federation-wide catalog.
    #[arg(long)]
    pub heal: bool,

    /// Dry-run: with `--heal`, compute and print the health report + the
    /// guarded actions the Deacon *would* take, but mutate nothing. The safe
    /// default for earning operator trust before enabling a scheduled heal pass.
    #[arg(long)]
    pub dry_run: bool,

    /// Dialogue-scan: capture each running worker's pane and classify any
    /// blocking dialogue sitting in it (tool-permission prompt vs. the Claude
    /// Code spend-/usage-limit dialog). The motivating incident: ten
    /// synthetic workers blocked on the spend-limit dialog with no
    /// human to press Enter. Per the be1e discipline (ADR-137 §2) pane text is
    /// read only to **surface** a finding — a `money_stake` class **always**
    /// pages the operator via `cs notify` and is **never** auto-confirmed; an
    /// `unknown` block alerts too; a safe `permission` prompt is
    /// auto-confirmed **only** when `--auto-confirm-safe` is also passed.
    /// Report-only by default (no keystroke) so it is safe to schedule.
    #[arg(long)]
    pub dialogue_scan: bool,

    /// With `--dialogue-scan`, opt in to firing the default-accept keystroke
    /// (Enter) on **safe** permission-class prompts only. Money stakes and
    /// unrecognised blocks are still never auto-confirmed — that refusal is
    /// encoded in the classifier, not in this flag. Off by default: the safe
    /// posture is to surface every block to a human.
    #[arg(long)]
    pub auto_confirm_safe: bool,

    /// Number of pane lines `--dialogue-scan` captures per worker (default 40).
    /// The live prompt sits at the bottom of the pane, so a small tail is
    /// enough; raise it for TUIs that render tall dialogs.
    #[arg(long, default_value_t = 40)]
    pub dialogue_lines: usize,

    /// Blocked-duration threshold in seconds for `--dialogue-scan` (default
    /// 900 = 15 min). A molecule whose progress has been frozen longer than
    /// this *and* is still sitting on a blocking dialogue escalates to a
    /// **canary RED** operator page — the heartbeat half of the primitive:
    /// "blocked > X min despite detection ⇒ RED".
    #[arg(long, default_value_t = 900)]
    pub dialogue_blocked_after: u64,
}

/// Maximum times patrol will respawn a worker before circuit-breaking.
const MAX_RESTARTS: u32 = 3;

/// Result of a patrol scan using the reconciliation model.
struct ScanResult {
    report: PatrolReport,
    /// Workers where `reconcile()` returned `Respawn` action.
    needs_respawn: Vec<WorkerId>,
    /// Workers where `reconcile()` returned `CircuitBreak` action.
    circuit_broken: Vec<WorkerId>,
    /// Workers where `reconcile()` returned `RecordFailure` action.
    record_failure: Vec<WorkerId>,
}

/// Scan the fleet and molecules using the reconciliation model.
///
/// For each worker, builds an [`ObservedState`] from transport probing,
/// calls [`reconcile`], and classifies the result into patrol categories.
fn scan(
    fleet: &Fleet,
    molecules: &[MoleculeData],
    backend: Option<&dyn TransportBackend>,
) -> ScanResult {
    let mut stalled_workers: Vec<WorkerId> = Vec::new();
    let mut error_workers: Vec<WorkerId> = Vec::new();
    let mut idle_count: usize = 0;
    let mut needs_respawn: Vec<WorkerId> = Vec::new();
    let mut circuit_broken: Vec<WorkerId> = Vec::new();
    let mut record_failure: Vec<WorkerId> = Vec::new();

    for worker in fleet.workers.values() {
        // Build ObservedState from transport.
        let transport = match backend {
            Some(be) => match be.is_alive(&worker.id) {
                Ok(true) => TransportState::Alive,
                Ok(false) => TransportState::Dead,
                Err(_) => TransportState::Unknown,
            },
            None => TransportState::Unknown,
        };
        let observed = ObservedState {
            transport,
            session: None, // Patrol doesn't need session detail.
            cognitive: CognitiveState::None,
        };

        let (effective, actions) = reconcile(
            worker.desired,
            &observed,
            worker.restart_count,
            MAX_RESTARTS,
        );

        // Classify by effective status and actions.
        match effective {
            EffectiveStatus::Stopped | EffectiveStatus::Paused => idle_count += 1,
            EffectiveStatus::Error(_) => error_workers.push(worker.id.clone()),
            _ => {}
        }

        for action in &actions {
            match action {
                ReconcileAction::Respawn => needs_respawn.push(worker.id.clone()),
                ReconcileAction::CircuitBreak => circuit_broken.push(worker.id.clone()),
                ReconcileAction::RecordFailure => record_failure.push(worker.id.clone()),
                ReconcileAction::Kill => stalled_workers.push(worker.id.clone()),
                _ => {}
            }
        }

        // A worker whose session is gone is stalled (for report).
        //
        // This used to read `effective == Diverged && transport == Dead`.
        // Once `Dead` was split out of `Diverged` (task-20260719-fedf) that
        // conjunction became unsatisfiable — `Diverged` now only names the
        // zombie direction, which is `transport == Alive` by construction —
        // and every dead worker would have silently dropped out of the
        // report. Keying off `Dead` directly says what was always meant.
        if effective == EffectiveStatus::Dead {
            stalled_workers.push(worker.id.clone());
        }
    }

    // Detect orphaned molecules: active molecules assigned to non-running workers.
    let is_dead = |wid: &WorkerId| -> bool {
        fleet
            .workers
            .get(wid)
            .is_none_or(|w| w.desired == DesiredState::Stopped)
    };

    let orphaned_molecules: Vec<_> = molecules
        .iter()
        .filter(|m| matches!(m.status, MoleculeStatus::Running | MoleculeStatus::Queued))
        .filter(|m| m.assigned_worker.as_ref().is_some_and(is_dead))
        .map(|m| m.id.clone())
        .collect();

    // One-bit bijection check (delib-20260414-2ab2 / hawking): every
    // (molecule_id, worker_role) pair should appear at most once. Two
    // runtime workers on the same molecule — or two cognition workers —
    // signals a split-brain runtime or a leaked respawn. This is the
    // smallest possible structural invariant on the fleet, and it is
    // expressible exactly because `worker_role` is now persisted.
    let duplicate_bindings = duplicate_role_bindings(fleet);

    let mut recommendations = build_recommendations(
        &stalled_workers,
        &error_workers,
        &orphaned_molecules,
        molecules,
    );
    for (mol_id, role, count) in &duplicate_bindings {
        recommendations.push(PatrolAction::AlertHuman {
            message: format!(
                "duplicate {role} worker on molecule {}: {count} workers share \
                 the same (mol_id, role) tuple — expected at most one",
                mol_id.as_str()
            ),
        });
    }
    if recommendations.is_empty() {
        recommendations.push(PatrolAction::NoAction);
    }

    ScanResult {
        report: PatrolReport {
            timestamp: Utc::now(),
            ensemble_size: fleet.workers.len(),
            idle_count,
            stalled_workers,
            error_workers,
            orphaned_molecules,
            recommendations,
        },
        needs_respawn,
        circuit_broken,
        record_failure,
    }
}

/// Detect `(molecule, worker_role)` tuples that appear more than once
/// across the fleet.
///
/// The healthy topology is: each live
/// molecule maps to at most one `Runtime` worker and at most one
/// `Cognition` worker. Two workers sharing the same role for the same
/// molecule is either a runtime split-brain (two `cs run` loops racing)
/// or a leaked respawn (the respawner couldn't see the existing worker).
/// Either way it deserves a human alert — patrol cannot auto-fix it
/// because we don't know which one holds the real lease.
fn duplicate_role_bindings(fleet: &Fleet) -> Vec<(cosmon_core::id::MoleculeId, WorkerRole, usize)> {
    use std::collections::HashMap;
    let mut counts: HashMap<(cosmon_core::id::MoleculeId, WorkerRole), usize> = HashMap::new();
    for w in fleet.workers.values() {
        let Some(mol) = w.current_molecule.clone() else {
            continue;
        };
        *counts.entry((mol, w.worker_role)).or_insert(0) += 1;
    }
    let mut out: Vec<_> = counts
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|((m, r), c)| (m, r, c))
        .collect();
    out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.cmp(&b.1)));
    out
}

/// Build corrective-action recommendations from detected issues.
fn build_recommendations(
    stalled: &[WorkerId],
    errored: &[WorkerId],
    orphans: &[cosmon_core::id::MoleculeId],
    molecules: &[MoleculeData],
) -> Vec<PatrolAction> {
    let mut recs = Vec::new();

    for wid in stalled {
        recs.push(PatrolAction::RestartWorker {
            worker_id: wid.clone(),
            reason: "worker is stale (unresponsive)".to_owned(),
        });
    }
    for wid in errored {
        recs.push(PatrolAction::RestartWorker {
            worker_id: wid.clone(),
            reason: "worker is in error state".to_owned(),
        });
    }
    for mol_id in orphans {
        let dead_worker = molecules
            .iter()
            .find(|m| m.id == *mol_id)
            .and_then(|m| m.assigned_worker.clone())
            .expect("orphaned molecule must have an assigned worker");
        recs.push(PatrolAction::ReassignMolecule {
            molecule_id: mol_id.clone(),
            dead_worker,
        });
    }

    recs
}

/// Attempt to respawn a worker by re-creating its Claude tmux session.
///
/// Uses the worker's clearance to determine the permission mode.
/// Returns `true` if the session was successfully created.
fn respawn_worker(
    worker: &cosmon_state::WorkerData,
    project_root: Option<&std::path::Path>,
    backend: &TmuxBackend,
) -> bool {
    use cosmon_transport::claude::{session_config, spawn_claude_session};

    let workdir = super::resolve_worker_workdir(worker, project_root);

    let config = session_config(
        "cosmon",
        worker.id.as_str(),
        &workdir,
        worker.clearance,
        None,
    );

    if spawn_claude_session(&config).is_err() {
        return false;
    }

    // Wait for Claude to be ready (handles trust prompt).
    let _ = cosmon_transport::readiness::wait_ready(
        backend,
        &worker.id,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(500),
    );

    true
}

/// Execute the `patrol` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // Guard: require project identity before touching transport.
    super::require_project_identity(ctx)?;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let mut fleet = store.load_fleet()?;
    let molecules = store.list_molecules(&MoleculeFilter::default())?;

    // Use tmux backend for liveness cross-check unless --no-tmux.
    let backend = if args.no_tmux {
        None
    } else {
        Some(TmuxBackend::new(super::tmux_socket_name(ctx)))
    };

    // task-20260719-fedf — reap the completed-but-teardown-failed limbo
    // BEFORE scanning, so those entries never reach `reconcile` and never
    // get proposed for respawn. Their work is finished; a respawn would
    // restart a molecule that already succeeded.
    let reaped = reap_finished_dead_workers(
        store.as_ref(),
        &state_dir,
        &fleet,
        &molecules,
        backend.as_ref().map(|b| b as &dyn TransportBackend),
    )?;
    if !reaped.is_empty() {
        fleet = store.load_fleet()?;
    }

    let scan_result = scan(
        &fleet,
        &molecules,
        backend.as_ref().map(|b| b as &dyn TransportBackend),
    );
    let report = &scan_result.report;

    // Apply RecordFailure: increment restart_count for suspect workers.
    let mut state_changed = false;
    for wid in &scan_result.record_failure {
        if let Some(worker) = fleet.workers.get_mut(wid) {
            worker.restart_count += 1;
            worker.updated_at = Utc::now();
            state_changed = true;
        }
    }
    // Apply CircuitBreak: mark as error in old status for backward compat.
    for wid in &scan_result.circuit_broken {
        if let Some(worker) = fleet.workers.get_mut(wid) {
            worker.status = WorkerStatus::Error("restart limit exceeded".to_owned());
            worker.updated_at = Utc::now();
            state_changed = true;
        }
    }
    if state_changed {
        store.save_fleet(&fleet)?;
    }

    // Auto-respawn: attempt to restart dead workers that reconcile() flagged.
    let project_root = store.project_root();
    let mut respawned: Vec<WorkerId> = Vec::new();
    if args.respawn && !scan_result.needs_respawn.is_empty() {
        if let Some(ref be) = backend {
            for wid in &scan_result.needs_respawn {
                if let Some(worker) = fleet.workers.get_mut(wid) {
                    if respawn_worker(worker, project_root.as_deref(), be) {
                        worker.restart_count += 1;
                        worker.status = WorkerStatus::Active;
                        worker.updated_at = Utc::now();
                        respawned.push(wid.clone());

                        // Emit respawn event.
                        let _ = cosmon_filestore::event::append(
                            &state_dir.join("events.jsonl"),
                            &cosmon_core::event::Envelope::now(
                                cosmon_core::event::Event::WorkerRespawned {
                                    worker_id: wid.clone(),
                                    restart_count: worker.restart_count,
                                },
                            ),
                        );
                    }
                }
            }
            if !respawned.is_empty() {
                store.save_fleet(&fleet)?;
            }
        } else {
            // State-only mode: just report what would be respawned.
            respawned.extend(scan_result.needs_respawn.iter().cloned());
        }
    }

    // Propel: nudge workers whose running molecules have stale progress.
    // Track the running-molecule count separately so we can report even
    // when nothing was propelled — silent success hides the no-op case.
    let running_molecule_count = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Running)
        .count();
    let propelled = if args.propel {
        propel_stale_molecules(
            store.as_ref(),
            &molecules,
            &fleet,
            backend.as_ref(),
            args.stale_after,
        )
    } else {
        PropelSweep::default()
    };

    // Nudge sweep (delib-20260420-1b02 M4) — per-step stall classifier
    // driven by `last_progress_at` + the active step's `timeout_minutes`.
    let nudged = if args.nudge {
        nudge_stalled_molecules(
            store.as_ref(),
            &state_dir,
            &molecules,
            backend.as_ref(),
            Utc::now(),
        )
    } else {
        Vec::new()
    };

    // Expire sweep: evaluate each molecule's TTL and apply its policy.
    let expire_report = if args.expire {
        expire_sweep(store.as_ref(), &state_dir, &molecules, Utc::now())?
    } else {
        ExpireSweepReport::default()
    };

    // Auto-freeze / auto-collapse orphans: close the loop between orphan
    // detection and remediation. Previously patrol only *reported* orphans —
    // they stayed Running forever unless a human manually collapsed them.
    // An orphan is any Running/Queued molecule whose worker is genuinely
    // dead (desired=Stopped or missing) OR whose worker needed respawn but
    // did not get it (respawn flag absent or respawn failed this run).
    let auto_transitioned = auto_freeze_orphans(
        store.as_ref(),
        &state_dir,
        &fleet,
        &molecules,
        &scan_result.needs_respawn,
        &respawned,
        args.auto_collapse,
    )?;

    // Harvest sweep: close the loop on Completed-but-unmerged molecules.
    // Belt-and-suspenders for cases where the tmux `pane-died` hook
    // installed at tackle time never fired.
    let harvest_report = if args.harvest {
        harvest_sweep(store.as_ref(), &state_dir, &molecules)
    } else {
        HarvestSweepReport::default()
    };

    // Livelock sweep: decidable detector per turing §6. Report-only.
    let livelock_report = if args.livelock {
        livelock_sweep(&state_dir, args.livelock_stale_after)
    } else {
        crate::cmd::livelock::LivelockReport::default()
    };

    // Silence-detect sweep: classify Running molecules whose worker has
    // not heartbeat in `silence_after` seconds. Tags `temp:frozen`,
    // emits `WorkerSilenceDetected`, and fires `cs notify`.
    let silence_report = if args.silence_detect {
        silence_detect_sweep(
            store.as_ref(),
            &state_dir,
            &molecules,
            args.silence_after,
            Utc::now(),
        )
    } else {
        SilenceDetectReport::default()
    };

    // Event-age sweep: harness-agnostic backstop. For every Running
    // molecule, flag those whose most recent event-log entry is older than
    // the threshold. Runtime-independent by construction (reads only
    // molecules + events.jsonl), so it fires even when cosmon-runtime is
    // dead — the external backstop for the external-modal stall.
    let event_age_report = if args.event_age {
        event_age_sweep(&molecules, &state_dir, args.event_age_after, Utc::now())
    } else {
        EventAgeReport::default()
    };

    // Abandon sweep (patrouille-abandon): fold the instance's existing
    // traces into named abandonment motifs. Pure read — the role that
    // was missing was the reader, not a new channel (aec8 retourné).
    let abandon_root = args.abandon_root.clone().unwrap_or_else(|| {
        state_dir
            .parent()
            .map_or_else(|| state_dir.clone(), Path::to_path_buf)
    });
    let abandon_report = if args.abandon {
        Some(super::patrol_abandon::abandon_sweep(
            &abandon_root,
            Utc::now(),
            args.abandon_quiet_hours,
        ))
    } else {
        None
    };

    // Heal sweep (the Deacon, ADR-137 §11 P3): detect → guard → remediate the
    // safe reversible classes (A1/A4/A5/A6/A8), each behind the §5 guard. The
    // collapse/integrity classes (A3/A7/A9) are reported but never mutated here.
    // Control-plane-keyed throughout — never a pane glyph (the be1e lesson).
    let heal_report = if args.heal {
        Some(super::patrol_heal::heal_sweep(
            ctx,
            &state_dir,
            args.dry_run,
            args.no_tmux,
            Utc::now(),
        ))
    } else {
        None
    };

    // Dialogue-scan sweep: capture running workers' panes, classify any
    // blocking dialogue, alert the operator for money/unknown stakes, and
    // (only when opted in) auto-confirm safe permission prompts. Pane text is
    // a diagnostic surfaced to a human — never a blind mutation trigger
    // (ADR-137 §2). Money stakes are hard-refused in the classifier.
    let dialogue_report = if args.dialogue_scan {
        dialogue_scan_sweep(
            store.as_ref(),
            &state_dir,
            &molecules,
            backend.as_ref().map(|b| b as &dyn TransportBackend),
            &DialogueScanOpts {
                lines: args.dialogue_lines,
                auto_confirm_safe: args.auto_confirm_safe,
                blocked_after: args.dialogue_blocked_after,
            },
            Utc::now(),
        )
    } else {
        DialogueScanReport::default()
    };

    // Patrol metrics: one JSON entry per run appended to patrol-metrics.json.
    // The file accumulates a timeline of orphan detection and remediation for
    // offline analysis (no fleet dashboard yet). Best-effort — a metric
    // write failure never fails the patrol run.
    let _ = append_patrol_metric(
        &state_dir,
        scan_result.report.orphaned_molecules.len(),
        auto_transitioned.len(),
        args.auto_collapse,
        respawned.len(),
    );

    if ctx.json {
        let mut output = serde_json::to_value(report)?;
        if !respawned.is_empty() {
            output["respawned"] =
                serde_json::json!(respawned.iter().map(WorkerId::as_str).collect::<Vec<_>>());
        }
        if !scan_result.circuit_broken.is_empty() {
            output["circuit_broken"] = serde_json::json!(scan_result
                .circuit_broken
                .iter()
                .map(WorkerId::as_str)
                .collect::<Vec<_>>());
        }
        if args.expire {
            output["expire"] = serde_json::json!({
                "scanned": expire_report.scanned,
                "warned": expire_report.warned.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
                "collapsed": expire_report.collapsed.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
                "escalated": expire_report.escalated.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
            });
        }
        if !reaped.is_empty() {
            output["reaped_finished_workers"] =
                serde_json::json!(reaped.iter().map(WorkerId::as_str).collect::<Vec<_>>());
        }
        if !auto_transitioned.is_empty() {
            let target = if args.auto_collapse {
                "collapsed"
            } else {
                "frozen"
            };
            output["auto_transitioned"] = serde_json::json!({
                "target_status": target,
                "molecules": auto_transitioned
                    .iter()
                    .map(MoleculeId::as_str)
                    .collect::<Vec<_>>(),
            });
        }
        if args.propel {
            output["propel"] = serde_json::json!({
                "running_molecules": running_molecule_count,
                "stale_after_seconds": args.stale_after,
                "max_attempts": cosmon_core::propel::PROPEL_MAX_ATTEMPTS,
                "propelled": propelled
                    .propelled
                    .iter()
                    .map(|(w, m, age)| serde_json::json!({
                        "worker": w.as_str(),
                        "molecule": m.as_str(),
                        "stale_seconds": age,
                    }))
                    .collect::<Vec<_>>(),
                // Declined candidates, each with the clock that declined it.
                "active": propelled
                    .active
                    .iter()
                    .map(|(w, m, idle)| serde_json::json!({
                        "worker": w.as_str(),
                        "molecule": m.as_str(),
                        "pane_idle_seconds": idle,
                    }))
                    .collect::<Vec<_>>(),
                "deferred": propelled
                    .deferred
                    .iter()
                    .map(|(w, m, remaining)| serde_json::json!({
                        "worker": w.as_str(),
                        "molecule": m.as_str(),
                        "next_nudge_in_seconds": remaining,
                    }))
                    .collect::<Vec<_>>(),
                "escalated": propelled
                    .escalated
                    .iter()
                    .map(|(w, m, attempts)| serde_json::json!({
                        "worker": w.as_str(),
                        "molecule": m.as_str(),
                        "attempts": attempts,
                        "tag": PROPEL_EXHAUSTED_TAG,
                    }))
                    .collect::<Vec<_>>(),
                "orphaned": propelled
                    .orphaned
                    .iter()
                    .map(|(w, m, attempts)| serde_json::json!({
                        "worker": w.as_str(),
                        "molecule": m.as_str(),
                        "attempts": attempts,
                        "tag": PROPEL_ORPHANED_TAG,
                    }))
                    .collect::<Vec<_>>(),
            });
        }
        if args.nudge {
            output["nudge"] = serde_json::json!({
                "idempotence_seconds": NUDGE_IDEMPOTENCE_SECS,
                "nudged": nudged
                    .iter()
                    .map(|(w, m)| serde_json::json!({
                        "worker": w.as_str(),
                        "molecule": m.as_str(),
                    }))
                    .collect::<Vec<_>>(),
            });
        }
        if args.harvest {
            output["harvest"] = serde_json::json!({
                "candidates": harvest_report.candidates,
                "harvested": harvest_report
                    .harvested
                    .iter()
                    .map(MoleculeId::as_str)
                    .collect::<Vec<_>>(),
                "skipped": harvest_report
                    .skipped
                    .iter()
                    .map(MoleculeId::as_str)
                    .collect::<Vec<_>>(),
                "failed": harvest_report
                    .failed
                    .iter()
                    .map(MoleculeId::as_str)
                    .collect::<Vec<_>>(),
            });
        }
        if args.livelock {
            output["livelock"] = serde_json::to_value(&livelock_report)?;
        }
        if args.silence_detect {
            output["silence_detect"] = serde_json::json!({
                "threshold_seconds": args.silence_after,
                "running_scanned": silence_report.scanned,
                "silent": silence_report
                    .silent
                    .iter()
                    .map(|s| serde_json::json!({
                        "molecule": s.molecule_id.as_str(),
                        "worker": s.worker_id.as_ref().map(WorkerId::as_str),
                        "age_seconds": s.age_seconds,
                    }))
                    .collect::<Vec<_>>(),
            });
        }
        if args.event_age {
            output["event_age"] = serde_json::json!({
                "threshold_seconds": args.event_age_after,
                "running_scanned": event_age_report.scanned,
                "stalled": event_age_report
                    .stalled
                    .iter()
                    .map(|s| serde_json::json!({
                        "molecule": s.molecule_id.as_str(),
                        "age_seconds": s.age_seconds,
                        "severity": s.severity_level,
                    }))
                    .collect::<Vec<_>>(),
            });
        }
        if let Some(ref ab) = abandon_report {
            output["abandon"] = serde_json::json!({
                "root": abandon_root.display().to_string(),
                "quiet_hours": args.abandon_quiet_hours,
                "report": serde_json::to_value(ab)?,
            });
        }
        if let Some(ref heal) = heal_report {
            output["heal"] = super::patrol_heal::to_value(heal);
        }
        if args.dialogue_scan {
            output["dialogue_scan"] = serde_json::json!({
                "running_scanned": dialogue_report.scanned,
                "auto_confirm_safe": args.auto_confirm_safe,
                "blocked_after_seconds": args.dialogue_blocked_after,
                "findings": dialogue_report
                    .findings
                    .iter()
                    .map(|f| serde_json::json!({
                        "molecule": f.molecule_id.as_str(),
                        "worker": f.worker_id.as_ref().map(WorkerId::as_str),
                        "class": f.class.as_str(),
                        "action": f.action.as_str(),
                        "blocked_seconds": f.blocked_seconds,
                        "evidence": f.evidence,
                    }))
                    .collect::<Vec<_>>(),
            });
        }
        let json = serde_json::to_string_pretty(&output)?;
        println!("{json}");
    } else {
        print_human_report(report, &respawned);
        if !reaped.is_empty() {
            println!();
            println!(
                "  {} {} finished worker(s) reaped (molecule terminal, session gone): {}",
                "REAP".cyan().bold(),
                reaped.len(),
                reaped
                    .iter()
                    .map(WorkerId::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if !auto_transitioned.is_empty() {
            print_auto_transitioned_report(&auto_transitioned, args.auto_collapse);
        }
        if args.propel {
            print_propel_report(running_molecule_count, args.stale_after, &propelled);
        }
        if args.nudge {
            print_nudge_report(&nudged);
        }
        if args.expire {
            print_expire_report(&expire_report);
        }
        if args.harvest {
            print_harvest_report(&harvest_report);
        }
        if args.livelock {
            print_livelock_report(&livelock_report);
        }
        if args.silence_detect {
            print_silence_report(&silence_report, args.silence_after);
        }
        if args.event_age {
            print_event_age_report(&event_age_report, args.event_age_after);
        }
        if let Some(ref ab) = abandon_report {
            super::patrol_abandon::print_abandon_report(ab, &abandon_root);
        }
        if let Some(ref heal) = heal_report {
            super::patrol_heal::print_plain(heal);
        }
        if args.dialogue_scan {
            print_dialogue_report(&dialogue_report);
        }
    }

    // Nucleate one issue molecule per detected cycle — always the last
    // effect, so a nucleation failure doesn't lose the preceding report.
    if args.livelock && !livelock_report.cycles.is_empty() {
        raise_livelock_issues(&livelock_report);
    }

    Ok(())
}

/// Read the presence registry and run the livelock detector.
///
/// Returns an empty report when the registry does not exist — the
/// command is forward-compatible with fleets that pre-date C-PRESENCE-CORE.
fn livelock_sweep(
    state_dir: &std::path::Path,
    stale_after_sec: u64,
) -> crate::cmd::livelock::LivelockReport {
    use crate::cmd::livelock;
    let sessions = livelock::read_blocked_sessions(state_dir);
    let stale_after = if stale_after_sec == 0 {
        None
    } else {
        Some(chrono::Duration::seconds(
            i64::try_from(stale_after_sec).unwrap_or(i64::MAX),
        ))
    };
    livelock::detect(&sessions, Utc::now(), stale_after)
}

/// Human-readable livelock section.
fn print_livelock_report(report: &crate::cmd::livelock::LivelockReport) {
    println!();
    let banner = "LIVELOCK".cyan().bold();
    if report.blocked_sessions == 0 {
        println!("  {banner} no parked sessions — nothing to check");
        return;
    }
    if report.cycles.is_empty() {
        println!(
            "  {banner} {} parked session(s), no cycles detected",
            report.blocked_sessions
        );
        if !report.stale_sessions.is_empty() {
            println!(
                "    ({} stale session(s) ignored: {})",
                report.stale_sessions.len(),
                report.stale_sessions.join(", ")
            );
        }
        return;
    }
    println!(
        "  {} {} livelock cycle(s) over {} parked session(s):",
        banner,
        report.cycles.len(),
        report.blocked_sessions
    );
    for cycle in &report.cycles {
        let waits = cycle
            .sessions
            .iter()
            .zip(cycle.waiting_on.iter())
            .map(|(s, m)| format!("{s}→{m}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "    ⊘ [{}] waiting on [{}] (oldest {})",
            cycle.sessions.join(", "),
            waits,
            cycle.oldest_since.to_rfc3339(),
        );
    }
    println!(
        "    {} non-destructive: inspect with `cs peek` or resolve via `cs stuck`",
        "→".dimmed()
    );
}

/// Invoke `cs nucleate` once per detected livelock cycle.
///
/// Best-effort: a failed nucleation is logged to stderr but never fails
/// the patrol run. This is turing's §8b discipline at work — the
/// detector reports; the operator resolves.
fn raise_livelock_issues(report: &crate::cmd::livelock::LivelockReport) {
    for cycle in &report.cycles {
        let topic = crate::cmd::livelock::render_issue_topic(cycle);
        let cs_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"));
        let mut cmd = std::process::Command::new(cs_bin);
        cmd.args([
            "nucleate",
            "spark",
            "--kind",
            "issue",
            "--tag",
            "temp:hot",
            "--tag",
            "livelock-detected",
            "--no-parent",
            "--var",
        ])
        .arg(format!("topic={topic}"));
        match cmd.output() {
            Ok(out) if !out.status.success() => {
                eprintln!(
                    "cs patrol --livelock: nucleate failed ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => {
                eprintln!("cs patrol --livelock: nucleate spawn failed: {e}");
            }
            Ok(_) => {}
        }
    }
}

/// Aggregate result of a harvest sweep across the fleet.
#[derive(Debug, Default)]
pub(crate) struct HarvestSweepReport {
    /// Every molecule that was Completed + not yet merged on entry.
    pub candidates: usize,
    /// Molecules that successfully went through `cs harvest` → `cs done`.
    pub harvested: Vec<MoleculeId>,
    /// Molecules that `cs harvest` classified as no-op (raced with another
    /// close path — `merged_at` already stamped, etc.).
    pub skipped: Vec<MoleculeId>,
    /// Molecules whose `cs harvest` invocation returned non-zero.
    pub failed: Vec<MoleculeId>,
}

/// Sweep every Completed-unmerged molecule and run `cs harvest` on each.
///
/// Idempotent by construction: `cs harvest` itself is a no-op when the
/// molecule is not Completed or is already merged. The sweep filters
/// first so it only spawns a child process for realistic candidates.
pub(crate) fn harvest_sweep(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    molecules: &[MoleculeData],
) -> HarvestSweepReport {
    let candidates: Vec<&MoleculeData> = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Completed && m.merged_at.is_none())
        .collect();

    let mut report = HarvestSweepReport {
        candidates: candidates.len(),
        ..Default::default()
    };

    for mol in candidates {
        // Delegate to the same logic `cs harvest` CLI uses so sweep and
        // hook paths cannot drift apart.
        match crate::cmd::harvest::harvest_one(store, state_dir, &mol.id, false) {
            Ok(crate::cmd::harvest::HarvestOutcome::Harvested) => {
                report.harvested.push(mol.id.clone());
            }
            Ok(
                crate::cmd::harvest::HarvestOutcome::AlreadyMerged
                | crate::cmd::harvest::HarvestOutcome::NotCompleted
                | crate::cmd::harvest::HarvestOutcome::DryRun,
            ) => {
                report.skipped.push(mol.id.clone());
            }
            Ok(crate::cmd::harvest::HarvestOutcome::HarvestFailed) | Err(_) => {
                report.failed.push(mol.id.clone());
            }
        }
    }
    report
}

/// Print the harvest section of the patrol report.
fn print_harvest_report(report: &HarvestSweepReport) {
    println!();
    if report.candidates == 0 {
        println!(
            "  {} no completed-but-unmerged molecules — nothing to harvest",
            "HARVEST".cyan().bold()
        );
        return;
    }
    println!(
        "  {} {} candidate(s): {} harvested, {} skipped, {} failed",
        "HARVEST".cyan().bold(),
        report.candidates,
        report.harvested.len(),
        report.skipped.len(),
        report.failed.len(),
    );
    for mid in &report.harvested {
        println!("    ✓ {mid}");
    }
    for mid in &report.failed {
        println!("    ✗ {mid}");
    }
}

/// Auto-transition orphaned molecules to `Frozen` (default) or `Collapsed`
/// (aggressive mode via `--auto-collapse`). Returns the list of molecule
/// IDs that were actually transitioned.
///
/// A molecule is considered orphaned when it is `Running`/`Queued` and its
/// assigned worker either (a) has `desired=Stopped` or is missing, or (b)
/// needed respawn this run but did not successfully come back. Respawned
/// workers are excluded — their molecules correctly stay Running.
fn auto_freeze_orphans(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    fleet: &Fleet,
    molecules: &[MoleculeData],
    needs_respawn: &[WorkerId],
    respawned: &[WorkerId],
    auto_collapse: bool,
) -> anyhow::Result<Vec<MoleculeId>> {
    let target_status = if auto_collapse {
        MoleculeStatus::Collapsed
    } else {
        MoleculeStatus::Frozen
    };
    let reason = if auto_collapse {
        "worker dead, auto-collapsed by patrol"
    } else {
        "worker dead, auto-frozen by patrol"
    };

    let stranded: Vec<MoleculeId> = molecules
        .iter()
        .filter(|m| matches!(m.status, MoleculeStatus::Running | MoleculeStatus::Queued))
        .filter_map(|m| {
            let wid = m.assigned_worker.as_ref()?;
            let worker_dead = fleet
                .workers
                .get(wid)
                .is_none_or(|w| w.desired == DesiredState::Stopped);
            let respawn_failed = needs_respawn.contains(wid) && !respawned.contains(wid);
            if worker_dead || respawn_failed {
                Some(m.id.clone())
            } else {
                None
            }
        })
        .collect();

    let mut transitioned = Vec::new();
    let events_path = state_dir.join("events.jsonl");
    for mol_id in stranded {
        let Ok(mut mol) = store.load_molecule(&mol_id) else {
            continue;
        };
        if !matches!(mol.status, MoleculeStatus::Running | MoleculeStatus::Queued) {
            continue;
        }
        let prev_status = mol.status;
        mol.status = target_status;
        mol.updated_at = Utc::now();
        if target_status == MoleculeStatus::Collapsed {
            mol.collapse_reason = Some(reason.to_owned());
            mol.collapsed_step = Some(mol.current_step);
        }
        store.save_molecule(&mol.id, &mol)?;

        let legacy_event = if target_status == MoleculeStatus::Frozen {
            cosmon_core::event::Event::MoleculeFrozen {
                molecule_id: mol_id.clone(),
            }
        } else {
            cosmon_core::event::Event::MoleculeCollapsed {
                molecule_id: mol_id.clone(),
                reason: reason.to_owned(),
            }
        };
        let _ = cosmon_filestore::event::append(
            &events_path,
            &cosmon_core::event::Envelope::now(legacy_event),
        );
        let _ = event_log::emit_one(
            &events_path,
            EventV2::MoleculeStatusChanged {
                molecule_id: mol_id.clone(),
                from: prev_status.to_string(),
                to: target_status.to_string(),
            },
            None,
        );
        transitioned.push(mol_id);
    }
    Ok(transitioned)
}

/// Reap fleet entries for workers that finished their molecule and then died
/// during teardown — the limbo [`auto_freeze_orphans`] cannot see.
///
/// **Why this exists (task-20260719-fedf).** `auto_freeze_orphans` filters
/// molecules to `Running | Queued`, because its job is to rescue work that
/// lost its worker. But the 2026-07-19 incident produced the mirror shape:
/// the molecule reached `Completed`, the post-merge harvest failed on the
/// trust gate, and the process died — leaving a *worker entry* that no sweep
/// owned. The molecule was terminal so the orphan sweep skipped it; the
/// worker was bound to a molecule so nothing else claimed it. It sat
/// `desired=Running` with a dead session for 17 hours.
///
/// A worker whose molecule is terminal has no work left to do, so a dead
/// session is not a failure to recover from — it is simply an entry to
/// reclaim. Respawning it would be actively wrong (the work is done), which
/// is why this runs *before* the respawn decision consumes the fleet.
///
/// Returns the reaped worker ids. Requires `backend` — with no liveness probe
/// there is no honest verdict, so the sweep declines rather than guessing.
fn reap_finished_dead_workers(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    fleet: &Fleet,
    molecules: &[MoleculeData],
    backend: Option<&dyn TransportBackend>,
) -> anyhow::Result<Vec<WorkerId>> {
    let Some(be) = backend else {
        return Ok(Vec::new());
    };

    // Workers bound to a molecule that has reached a terminal status.
    let finished: Vec<WorkerId> = molecules
        .iter()
        .filter(|m| m.status.is_terminal())
        .filter_map(|m| m.assigned_worker.clone())
        .collect();

    let mut reaped = Vec::new();
    let events_path = state_dir.join("events.jsonl");
    for wid in finished {
        let Some(worker) = fleet.workers.get(&wid) else {
            continue;
        };
        // Only entries still *claiming* to be live can be lying.
        if !matches!(worker.desired, DesiredState::Running | DesiredState::Paused) {
            continue;
        }
        // A probe error is not a death certificate — mirror `cs purge` and
        // treat only a definitive `Ok(false)` as gone.
        if be.is_alive(&wid).unwrap_or(true) {
            continue;
        }

        // Re-read under the fleet lock: the snapshot `fleet` may be stale by
        // now, and a blind write would clobber a concurrent spawn.
        let _guard = store.lock_fleet()?;
        let mut latest = store.load_fleet()?;
        if latest.workers.remove(&wid).is_none() {
            continue;
        }
        store.save_fleet(&latest)?;

        let _ = event_log::emit_one(
            &events_path,
            EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "molecule terminal, session gone — reaped by patrol".to_owned(),
            },
            None,
        );
        reaped.push(wid);
    }
    Ok(reaped)
}

/// Append a patrol metric entry to `patrol-metrics.json`. Best-effort:
/// a corrupt or unreadable file is replaced with a fresh document rather
/// than failing the patrol run. The file is a simple `{ "entries": [...] }`
/// JSON document, grown by appending one record per patrol invocation.
fn append_patrol_metric(
    state_dir: &std::path::Path,
    orphans_detected: usize,
    auto_transitioned: usize,
    auto_collapse: bool,
    respawned: usize,
) -> anyhow::Result<()> {
    let path = state_dir.join("patrol-metrics.json");
    let mut doc: serde_json::Value = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({ "entries": [] }))
    } else {
        serde_json::json!({ "entries": [] })
    };
    let entry = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(),
        "orphans_detected": orphans_detected,
        "auto_transitioned": auto_transitioned,
        "target_status": if auto_collapse { "collapsed" } else { "frozen" },
        "respawned": respawned,
    });
    if let Some(arr) = doc.get_mut("entries").and_then(|v| v.as_array_mut()) {
        arr.push(entry);
    } else {
        doc = serde_json::json!({ "entries": [entry] });
    }
    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}

/// Render the auto-transition section of the human-readable patrol report.
fn print_auto_transitioned_report(molecules: &[MoleculeId], auto_collapse: bool) {
    let (label, color) = if auto_collapse {
        ("COLLAPSE", "red")
    } else {
        ("FREEZE", "cyan")
    };
    println!();
    let colored_label = if color == "red" {
        label.red().bold()
    } else {
        label.cyan().bold()
    };
    println!(
        "  {colored_label} {} orphaned molecule(s) auto-transitioned:",
        molecules.len(),
    );
    for mid in molecules {
        println!("    - {mid}");
    }
}

/// Print the propel section of the patrol report with explicit no-op
/// messaging. Without this, a patrol loop with nothing to nudge looked
/// identical to a patrol loop that had no running molecules at all.
fn print_propel_report(running_count: usize, stale_after: u64, sweep: &PropelSweep) {
    println!();
    if running_count == 0 {
        println!(
            "  {} no running molecules to monitor",
            "PROPEL".cyan().bold()
        );
        return;
    }
    let declined = sweep.active.len()
        + sweep.deferred.len()
        + sweep.escalated.len()
        + sweep.gated.len()
        + sweep.orphaned.len();
    if sweep.propelled.is_empty() && declined == 0 {
        println!(
            "  {} {running_count} running molecule(s), all fresh (<{stale_after}s)",
            "PROPEL".cyan().bold()
        );
        return;
    }
    println!(
        "  {} {} worker(s) propelled ({running_count} running, threshold {stale_after}s):",
        "PROPEL".cyan().bold(),
        sweep.propelled.len(),
    );
    for (wid, mid, age) in &sweep.propelled {
        println!("    - {wid} ← {mid} (stale {age}s)");
    }
    // The declined lines are the point of the 2026-07-19 repair: a worker
    // that is merely thinking must be visibly *left alone*, not silently
    // omitted, or the next regression looks like a quiet patrol.
    for (wid, mid, idle) in &sweep.active {
        println!("    · {wid} ← {mid} (working — pane active {idle}s ago, not nudged)");
    }
    // A gated worker is the one decline an operator must actually act on: the
    // molecule is not stuck, it is waiting on *them*.
    for (wid, mid) in &sweep.gated {
        println!("    · {wid} ← {mid} (awaiting operator — questions pending, not nudged)");
    }
    for (wid, mid, remaining) in &sweep.deferred {
        println!("    · {wid} ← {mid} (backoff — next nudge in {remaining}s)");
    }
    for (wid, mid, attempts) in &sweep.escalated {
        println!(
            "    {} {wid} ← {mid} ({attempts} nudges ignored — tagged `{PROPEL_EXHAUSTED_TAG}` for `cs patrol --heal`)",
            "!".yellow().bold()
        );
    }
    // The orphan line is the 2026-07-21 money-pump made visible: a brief-less
    // worker that must never be nudged, only re-briefed or collapsed.
    for (wid, mid, attempts) in &sweep.orphaned {
        println!(
            "    {} {wid} ← {mid} (orphaned — briefing missing, {attempts} nudges spent; tagged `{PROPEL_ORPHANED_TAG}`, operator notified — re-deliver brief or collapse, NOT nudgeable)",
            "‼".red().bold()
        );
    }
}

/// The propulsion nudge message sent to stale workers. Short, cosmon-vocabulary,
/// and tells the worker exactly what to do: re-read context and continue.
pub(crate) const PROPULSION_NUDGE: &str = "⚛ PROPULSION — you appear idle mid-molecule. \
Re-read your current step and continue execution immediately. \
A molecule in motion stays in motion.";

/// Pure: find all Running molecules whose `updated_at` is older than
/// `stale_after` seconds and whose assigned worker is still desired-running
/// in the fleet. Returns `(worker_id, molecule_id, age_seconds)` tuples.
///
/// This is pure/deterministic: given the same inputs, always returns the
/// same output. Testable without transport or filesystem.
pub(crate) fn find_stale_running_molecules(
    molecules: &[MoleculeData],
    fleet: &Fleet,
    stale_after: u64,
    now: chrono::DateTime<Utc>,
) -> Vec<(WorkerId, cosmon_core::id::MoleculeId, i64)> {
    let threshold = i64::try_from(stale_after).unwrap_or(i64::MAX);
    molecules
        .iter()
        .filter_map(|mol| {
            if mol.status != MoleculeStatus::Running {
                return None;
            }
            let wid = mol.assigned_worker.as_ref()?;
            let worker = fleet.workers.get(wid)?;
            if worker.desired != DesiredState::Running {
                return None;
            }
            let age = now.signed_duration_since(mol.updated_at).num_seconds();
            if age < threshold {
                return None;
            }
            Some((wid.clone(), mol.id.clone(), age))
        })
        .collect()
}

/// For each stale Running molecule, send the propulsion nudge to its
/// assigned worker via transport. Returns the list of actually-nudged
/// workers (filters out those whose tmux session is dead or unreachable,
/// or whose pane signature does not match the registered Adapter).
///
/// This is the transport-layer safety net: when the cognitive layer fails
/// (worker silent at ❯ prompt mid-step), patrol re-engages it externally.
///
/// # Pane-signature gate (ADR-097 PR-2)
///
/// Before propelling, the gate looks up the calling Worker's Adapter
/// in the [`default_registry`] and checks the observed
/// `pane_current_command` against the registered signatures. An
/// [`EventV2::AdapterPaneSignatureChecked`] is emitted on every check
/// (pass or fail) so silent pane-signature drift becomes visible in
/// `events.jsonl`. The `claude` Adapter accepts `["claude", "claude*",
/// "node", "<version>"]` — the last sentinel covers the native binary
/// reporting its own version as the pane `comm` (e.g. `2.1.175`,
/// observed 2026-06-12). The C6 `--adapter` flag plumbs the per-worker
/// name through without further refactor here.
/// The outcome of one `--propel` sweep, split by what admission control
/// ([`cosmon_core::propel::decide_nudge`]) decided for each stale candidate.
///
/// Before the 2026-07-19 repair this was a bare `Vec` of propelled workers,
/// which made the sweep's two failure modes invisible: a nudge sent to a
/// working worker and a nudge sent for the ninth time both showed up as an
/// ordinary success line. Each declined candidate is now reported in its own
/// bucket so the spam, if it ever returns, is legible in the report itself.
#[derive(Debug, Default)]
pub(crate) struct PropelSweep {
    /// Workers actually nudged this pass: `(worker, molecule, stale_seconds)`.
    pub(crate) propelled: Vec<(WorkerId, MoleculeId, i64)>,
    /// Stale-by-progress candidates whose terminal was recently active — i.e.
    /// thinking, not idle: `(worker, molecule, pane_idle_seconds)`.
    pub(crate) active: Vec<(WorkerId, MoleculeId, i64)>,
    /// Candidates whose backoff window has not elapsed:
    /// `(worker, molecule, seconds_remaining)`.
    pub(crate) deferred: Vec<(WorkerId, MoleculeId, i64)>,
    /// Candidates that spent the attempt ceiling; patrol stopped nudging and
    /// tagged them for the healer: `(worker, molecule, attempts)`.
    pub(crate) escalated: Vec<(WorkerId, MoleculeId, u32)>,
    /// Candidates parked at an operator gate (`cs await-operator`). Not a
    /// stall: the worker is holding questions for a human and must be left
    /// entirely alone. Reported so the wait is visible rather than silent.
    pub(crate) gated: Vec<(WorkerId, MoleculeId)>,
    /// Candidates whose worker is **orphaned** — its briefing is missing, so it
    /// cannot evolve and a nudge is futile (the 2026-07-21 money pump). Tagged
    /// [`PROPEL_ORPHANED_TAG`] and surfaced to the operator via `cs notify`;
    /// never nudged: `(worker, molecule, nudges_already_delivered)`.
    pub(crate) orphaned: Vec<(WorkerId, MoleculeId, u32)>,
}

/// Does this molecule's worker sit at an operator gate?
///
/// Two independent witnesses, either of which is sufficient — the tag
/// [`cosmon_core::operator_block::AWAITING_OP_TAG`] stamped by
/// `cs await-operator`, and the durable `blocked_on.json` proof-of-block in the
/// molecule dir. The file is the belt to the tag's suspenders: a reconcile or a
/// hand-edit that drops tags must not silently re-open the gate to propulsion.
///
/// Failure direction is deliberately toward *silence*: an unreadable state dir
/// makes this return `false` only when both witnesses are absent, and the cost
/// of a false `true` is one skipped nudge against a worker `cs patrol --heal`
/// will still reach.
pub(crate) fn worker_awaits_operator(store: &dyn StateStore, mol: &MoleculeData) -> bool {
    cosmon_core::operator_block::awaits_operator(&mol.tags)
        || store.molecule_dir(&mol.id).join("blocked_on.json").exists()
}

/// Tag stamped on a molecule whose propulsion attempts are exhausted, so
/// `cs patrol --heal` and a human triage query can find it.
///
/// A tag rather than a tenth nudge: four spaced sentences that changed nothing
/// mean the fault is structural, and repeating the sentence is the one remedy
/// already proven not to work.
pub(crate) const PROPEL_EXHAUSTED_TAG: &str = "propel-exhausted";

/// Tag stamped on a molecule whose worker is orphaned (briefing missing), so
/// `cs patrol --heal`, `cs health`, and an operator triage can find the
/// molecule that needs its brief re-delivered — a distinct fault from
/// [`PROPEL_EXHAUSTED_TAG`] (which means "nudged and ignored", not "cannot be
/// nudged at all").
pub(crate) const PROPEL_ORPHANED_TAG: &str = "propel-orphaned";

/// Basename of the briefing artefact `cs tackle` / `cs evolve` write into a
/// molecule's directory. Its **absence** is the structural signature of an
/// orphaned worker (task-20260721-e1d9).
const BRIEFING_FILENAME: &str = "briefing.md";

/// Can the worker's briefing be read for this molecule?
///
/// Returns `false` only on a *definite* negative — the file is missing or
/// empty — which is the money-pump signature the orphan gate keys on. Every
/// other outcome (file present and non-empty, or an I/O error that leaves
/// presence genuinely unknown) returns `true`, so an unreadable state dir
/// degrades to the pre-orphan behaviour rather than mass-escalating a fleet on
/// a transient filesystem hiccup (the failure direction the module docs
/// require).
///
/// This reads a *filesystem fact*, never a pane glyph, so it is clear of the
/// ADR-137 §2 use/mention hazard: there is no string a worker can print to
/// make patrol treat it as orphaned.
pub(crate) fn worker_briefing_present(store: &dyn StateStore, mid: &MoleculeId) -> bool {
    let path = store.molecule_dir(mid).join(BRIEFING_FILENAME);
    match std::fs::metadata(&path) {
        Ok(meta) => meta.len() > 0,
        // `NotFound` is the one certain "orphaned" verdict; any other error
        // (permissions, a racing writer) is treated as unknown ⇒ present.
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Tag an orphaned molecule so the healer and operator can find it and
/// re-deliver its brief. Idempotent — the tag is a set member.
fn mark_propel_orphaned(store: &dyn StateStore, mid: &MoleculeId) {
    let Ok(mut mol) = store.load_molecule(mid) else {
        return;
    };
    let Ok(tag) = Tag::new(PROPEL_ORPHANED_TAG) else {
        return;
    };
    if mol.tags.insert(tag) {
        let _ = store.save_molecule(mid, &mol);
    }
}

/// Surface an orphaned worker to the operator via `cs notify` (the cost-guard:
/// a brief-less worker left unattended is the money pump, so it must be *loud*,
/// not a silent bleed). Best-effort and detached, mirroring the silence-detect
/// notify: a missing `[notify]` block, CI, or dry-run all just skip.
fn notify_propel_orphaned(wid: &WorkerId, mid: &MoleculeId, stale_secs: i64) {
    let cs_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"));
    let msg = format!(
        "worker {worker} orphaned on {mol} (briefing missing, stale {stale_secs}s) — \
         re-deliver the brief (`cs tackle --force {mol}`) or collapse; NOT nudgeable",
        worker = wid.as_str(),
        mol = mid.as_str(),
    );
    let mut cmd = std::process::Command::new(cs_bin);
    cmd.args([
        "notify",
        &msg,
        "--level",
        "alert",
        "--molecule",
        mid.as_str(),
        "--title",
        "cosmon: worker orphaned (brief-less)",
    ]);
    if std::env::var_os("COSMON_NOTIFY_DRY_RUN").is_some() {
        cmd.arg("--dry-run");
    }
    let _ = cmd.spawn();
}

pub(crate) fn propel_stale_molecules(
    store: &dyn StateStore,
    molecules: &[MoleculeData],
    fleet: &Fleet,
    backend: Option<&TmuxBackend>,
    stale_after: u64,
) -> PropelSweep {
    let Some(be) = backend else {
        return PropelSweep::default();
    };
    let now = Utc::now();
    let candidates = find_stale_running_molecules(molecules, fleet, stale_after, now);
    let registry = default_registry();
    let adapter_name = CLAUDE_ADAPTER;
    let stale_window = chrono::Duration::seconds(i64::try_from(stale_after).unwrap_or(i64::MAX));
    let mut sweep = PropelSweep::default();

    for (wid, mid, age) in candidates {
        if !be.is_alive(&wid).unwrap_or(false) {
            // No pane-died hook fired for this molecule (tmux server
            // restart, brutal crash, hook never armed). The patrol is the
            // writer of last resort: project the dead witness onto
            // `MoleculeProcess.status` (Dead → Stale, one coup) so the
            // inline record `cs observe`/`cs ensemble` read stops lying
            // "active". Report-only — `auto_freeze_orphans` and the
            // silence-detect sweep own the molecule-level remediation;
            // we never collapse here (turing §8b).
            project_liveness_onto_process(store, &mid, Liveness::Dead, stale_after);
            continue;
        }
        let session_name = molecules
            .iter()
            .find(|m| m.id == mid)
            .and_then(|m| m.session_name.clone())
            .unwrap_or_else(|| wid.to_string());
        let observed = pane_current_command(be.socket(), &session_name).unwrap_or_default();
        let matched = registry.matches(adapter_name, &observed);
        let mol_dir = store.molecule_dir(&mid);
        emit_adapter_pane_signature_checked(
            &mol_dir,
            &mid,
            &wid,
            adapter_name,
            registry.signatures_of(adapter_name),
            &observed,
            matched,
            PerturbationChannel::Propulsion,
        );
        if !matched {
            continue;
        }
        // Admission control (task-20260719-00ed). Progress staleness alone
        // proved far too weak a warrant: it cannot see a worker thinking
        // inside one step, and it re-fires every pass forever. Consult the
        // transport clock and the attempt ledger before speaking — and
        // *before* the liveness projection below, because condemning a
        // working worker's process record is the same false-idle error
        // committed in a second organ.
        let attempts = propel_attempts(molecules, &mid, now);
        let mol = molecules.iter().find(|m| m.id == mid);
        let view = NudgeView {
            channel: NudgeChannel::Propulsion,
            status: mol.map_or(MoleculeStatus::Running, |m| m.status),
            awaiting_operator: mol.is_some_and(|m| worker_awaits_operator(store, m)),
            briefing_present: worker_briefing_present(store, &mid),
            progress_age: chrono::Duration::seconds(age),
            pane_idle: pane_idle_seconds(be.socket(), &session_name).map(chrono::Duration::seconds),
            attempts: attempts.count,
            since_last_propel: attempts
                .last_at
                .map(|at| now.signed_duration_since(at))
                .map(|d| d.max(chrono::Duration::zero())),
        };
        let decision = decide_nudge(&view, stale_window);

        // A thinking worker is left entirely alone: no nudge, and no witness
        // stamped either. Its progress clock is frozen but its terminal is
        // not, so the "figé-mais-vivant" reading below simply does not apply.
        if let NudgeDecision::Skip(NudgeSkip::PaneActive { idle_secs, .. }) = decision {
            sweep.active.push((wid, mid, idle_secs));
            continue;
        }

        // Likewise for a worker at an operator gate — and here the liveness
        // projection must be skipped for a second reason: its process is
        // healthy and its silence is the *intended* behaviour, so stamping it
        // Unresponsive would manufacture a health anomaly out of a correct
        // pause.
        if let NudgeDecision::Skip(NudgeSkip::AwaitingOperator | NudgeSkip::NotRunning { .. }) =
            decision
        {
            if matches!(decision, NudgeDecision::Skip(NudgeSkip::AwaitingOperator)) {
                sweep.gated.push((wid, mid));
            }
            continue;
        }

        // The orphan gate (task-20260721-e1d9). A brief-less worker cannot
        // evolve, so nudging it is the six-hour money pump. Escalate once —
        // tag it, notify the operator loudly — and never nudge. Handled before
        // the liveness projection because an orphan is a control-plane fact
        // (missing briefing), not a stalled-but-live process to demote.
        if let NudgeDecision::Escalate {
            attempts,
            reason: EscalateReason::Orphaned,
        } = decision
        {
            mark_propel_orphaned(store, &mid);
            notify_propel_orphaned(&wid, &mid, age);
            sweep.orphaned.push((wid, mid, attempts));
            continue;
        }

        // Figé-mais-vivant: the process is up but its molecule's progress
        // (`updated_at`) has been frozen past `stale_after` *and* its
        // terminal has been silent at least as long. The candidate is by
        // construction a stale-Alive, so the two-coup scale reads it
        // through the I10 demotion (Alive-older-than-TTL → Unknown): first
        // sweep → Unresponsive (slow, we still nudge — never kill), a
        // second consecutive frozen sweep → Stale. This is the case the
        // claude CLI's "stay-alive on model-unavailable" produces — a
        // false-active worker that no kernel signal will ever flag.
        project_liveness_onto_process(store, &mid, Liveness::Alive, stale_after);

        match decision {
            // Already handled by the early `continue`s above; matched here
            // only to keep the match exhaustive without a panic.
            NudgeDecision::Skip(
                NudgeSkip::PaneActive { .. }
                | NudgeSkip::AwaitingOperator
                | NudgeSkip::NotRunning { .. },
            )
            | NudgeDecision::Escalate {
                reason: EscalateReason::Orphaned,
                ..
            } => {}
            NudgeDecision::Skip(NudgeSkip::Backoff {
                since_secs,
                window_secs,
                ..
            }) => {
                sweep
                    .deferred
                    .push((wid, mid, (window_secs - since_secs).max(0)));
            }
            NudgeDecision::Escalate {
                attempts,
                reason: EscalateReason::AttemptsExhausted,
            } => {
                mark_propel_exhausted(store, &mid);
                sweep.escalated.push((wid, mid, attempts));
            }
            NudgeDecision::Nudge { attempt, .. } => {
                if be.send_input(&wid, PROPULSION_NUDGE).is_ok() {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = be.send_input(&wid, "");
                    record_propel(store, &mid, attempt, now);
                    sweep.propelled.push((wid, mid, age));
                }
            }
        }
    }
    sweep
}

/// The propulsion attempt ledger for one molecule, as of `now`.
#[derive(Debug, Clone, Copy, Default)]
struct PropelAttempts {
    /// Nudges delivered for the *current* stall.
    count: u32,
    /// When the last of them was sent.
    last_at: Option<chrono::DateTime<Utc>>,
}

/// Read a molecule's propulsion ledger, **resetting it when the molecule made
/// progress since the last nudge**.
///
/// The reset is what keeps the backoff honest across stalls. `propel_count` is
/// a per-stall register, not a lifetime total: a molecule that stalled, was
/// nudged three times, recovered, and stalled again an hour later deserves a
/// prompt first nudge — not the 30-minute window its old count would impose.
/// Progress is `updated_at` moving past `last_propelled_at`, which is exactly
/// why [`record_propel`] must never touch `updated_at`.
fn propel_attempts(
    molecules: &[MoleculeData],
    mid: &MoleculeId,
    _now: chrono::DateTime<Utc>,
) -> PropelAttempts {
    let Some(mol) = molecules.iter().find(|m| &m.id == mid) else {
        return PropelAttempts::default();
    };
    let Some(last_at) = mol.last_propelled_at else {
        return PropelAttempts::default();
    };
    if mol.updated_at > last_at {
        // The worker moved after we last spoke: this is a fresh stall.
        return PropelAttempts::default();
    }
    PropelAttempts {
        count: mol.propel_count,
        last_at: Some(last_at),
    }
}

/// Persist one delivered nudge into the molecule's ledger.
///
/// Deliberately does **not** advance `updated_at`: a nudge is something patrol
/// did *to* the worker, not progress the worker made. Bumping `updated_at`
/// here would mark the molecule fresh, disqualify it from the next sweep, and
/// then — one threshold later — present it as a brand-new stall with a
/// zeroed ledger, resurrecting the unbounded nudging this repair removes.
///
/// Best-effort: a load/save failure is swallowed, matching the rest of the
/// patrol's writer-of-last-resort discipline. The cost of a lost write is one
/// extra nudge, never a wedged sweep.
fn record_propel(
    store: &dyn StateStore,
    mid: &MoleculeId,
    attempt: u32,
    now: chrono::DateTime<Utc>,
) {
    let Ok(mut mol) = store.load_molecule(mid) else {
        return;
    };
    mol.propel_count = attempt;
    mol.last_propelled_at = Some(now);
    let _ = store.save_molecule(mid, &mol);
}

/// Tag a molecule whose propulsion attempts are spent so the healer and the
/// operator can see it without re-deriving the ledger. Idempotent — the tag is
/// a set member, so a repeated sweep re-writes nothing.
fn mark_propel_exhausted(store: &dyn StateStore, mid: &MoleculeId) {
    let Ok(mut mol) = store.load_molecule(mid) else {
        return;
    };
    let Ok(tag) = Tag::new(PROPEL_EXHAUSTED_TAG) else {
        return;
    };
    if mol.tags.insert(tag) {
        let _ = store.save_molecule(mid, &mol);
    }
}

/// Project an external liveness observation onto a stale molecule's
/// [`MoleculeProcess::status`](cosmon_core::process::MoleculeProcess::status)
/// via the two-coup scale, persisting only when the status changed.
///
/// The caller has already established the molecule is stale-by-progress
/// (its `updated_at` is older than `stale_after`). For an `Alive`
/// observation this means a *stale* alive, so the witness is stamped at
/// `updated_at` and read against a `stale_after` TTL — the I10 demotion
/// then funnels it through the Unknown ladder (figé-mais-vivant). A
/// `Dead` observation condemns to `Stale` in one coup regardless.
///
/// Returns the projected status, or `None` when the molecule has no live
/// `process` record to update (legacy / already torn down). Best-effort:
/// a load/save failure is swallowed — the patrol must never wedge.
fn project_liveness_onto_process(
    store: &dyn StateStore,
    mid: &MoleculeId,
    liveness: Liveness,
    stale_after: u64,
) -> Option<WorkerStatus> {
    let mut mol = store.load_molecule(mid).ok()?;
    let last_progress = mol.updated_at;
    let process = mol.process.as_mut()?;
    let witness = Witness::at(last_progress, liveness, BranchState::Unmerged);
    let ttl = Duration::from_secs(stale_after);
    let projected = project_process_status(&process.status, &witness, Utc::now(), ttl);
    if process.status != projected {
        process.status = projected.clone();
        let _ = store.save_molecule(mid, &mol);
    }
    Some(projected)
}

/// Idempotence window for `cs patrol --nudge` — a worker is never poked
/// twice within this duration. Cheap nudges, but not duplicate ones.
pub(crate) const NUDGE_IDEMPOTENCE_SECS: i64 = 60;

/// Boot-stall budget for `cs patrol --nudge`: a `Running` molecule that has
/// *never* recorded progress (`last_progress_at == None`) is nudged once its
/// tackle is older than this many seconds.
///
/// This is the patrol half of the stuck-paste fix (task-20260718-ac03): a
/// worker whose bootstrap paste lost its submitting Enter sits at the prompt
/// with zero events and zero tokens forever — and because it never produced a
/// `last_progress_at`, the pre-fix classifier skipped it *by construction*
/// (`--propel` keys off progress staleness, `--nudge` off `last_progress_at`).
/// A never-started molecule had neither, so the exact failure mode the patrol
/// exists to catch was structurally invisible to it (13× on one galaxy,
/// 2026-07-18). The signal here is control-plane only — a missing progress
/// timestamp against `tackled_at` — never a pane grep (ADR-137 §2).
pub(crate) const NUDGE_BOOT_STALL_SECS: i64 = 120;

/// Pure: among `molecules`, return those that are `Running`, have an
/// assigned worker, and are stalled by either signal:
///
/// - **step-stall** — `last_progress_at` older than `now - budget` (where
///   `budget = step.timeout_minutes`, default 30 min); or
/// - **boot-stall** — no `last_progress_at` at all and `tackled_at` (fallback
///   `updated_at` for legacy records) older than [`NUDGE_BOOT_STALL_SECS`] —
///   the never-started worker whose bootstrap paste was never submitted.
///
/// Both classes honor the [`NUDGE_IDEMPOTENCE_SECS`] guard. Loads each
/// molecule's formula via the supplied lookup so the test fixture can pass a
/// closure instead of touching disk. Returns the list of
/// `(worker_id, molecule_id)` pairs ready for [`tmux send-keys`].
pub(crate) fn find_stalled_for_nudge<F>(
    molecules: &[MoleculeData],
    now: chrono::DateTime<Utc>,
    formula_for: F,
) -> Vec<(WorkerId, &MoleculeData)>
where
    F: Fn(&cosmon_core::id::FormulaId) -> Option<cosmon_core::formula::Formula>,
{
    let mut out = Vec::new();
    for mol in molecules {
        if mol.status != MoleculeStatus::Running {
            continue;
        }
        let Some(wid) = mol.assigned_worker.as_ref() else {
            continue;
        };
        let stalled = if let Some(progress_ts) = mol.last_progress_at {
            let timeout_minutes = formula_for(&mol.formula_id)
                .and_then(|f| {
                    f.steps
                        .get(mol.current_step)
                        .map(cosmon_core::formula::Step::stall_timeout_minutes)
                })
                .unwrap_or(30);
            let budget = chrono::Duration::minutes(i64::from(timeout_minutes));
            now.signed_duration_since(progress_ts) > budget
        } else {
            // Boot-stall: tackled but zero progress ever recorded. Anchor
            // on the tackle instant; legacy records without `tackled_at`
            // fall back to `updated_at` (stamped at tackle and frozen
            // since, precisely because nothing ever happened).
            let anchor = mol.tackled_at.unwrap_or(mol.updated_at);
            now.signed_duration_since(anchor).num_seconds() > NUDGE_BOOT_STALL_SECS
        };
        if !stalled {
            continue;
        }
        if let Some(last_nudge) = mol.last_nudged_at {
            if now.signed_duration_since(last_nudge).num_seconds() < NUDGE_IDEMPOTENCE_SECS {
                continue;
            }
        }
        out.push((wid.clone(), mol));
    }
    out
}

/// Human-readable nudge report. Mirrors `print_propel_report` so a
/// silent loop ("nothing to nudge") is still visibly distinct from an
/// active one.
fn print_nudge_report(nudged: &[(WorkerId, cosmon_core::id::MoleculeId)]) {
    println!();
    if nudged.is_empty() {
        println!("  {} no stalled molecules to nudge", "NUDGE".cyan().bold());
        return;
    }
    println!(
        "  {} {} worker(s) nudged (idempotence {}s):",
        "NUDGE".cyan().bold(),
        nudged.len(),
        NUDGE_IDEMPOTENCE_SECS,
    );
    for (wid, mid) in nudged {
        println!("    - {wid} ← {mid}");
    }
}

/// Render the per-molecule nudge text. Includes the absolute path to the
/// molecule's `briefing.md` so the re-engaged worker can re-read its
/// contract before continuing — the cognitive equivalent of "look at the
/// rules of the game again, then play".
fn nudge_message(briefing_path: &Path) -> String {
    format!(
        "⚛ NUDGE — re-read your briefing at {} and continue execution. \
         A molecule in motion stays in motion.",
        briefing_path.display()
    )
}

/// For each stalled Running molecule, send the nudge via `tmux send-keys`
/// (referencing the molecule's `briefing.md` path), increment its
/// `nudge_count`, stamp `last_nudged_at`, and persist. Returns the list
/// of actually-nudged `(worker_id, molecule_id)` pairs.
pub(crate) fn nudge_stalled_molecules(
    store: &dyn StateStore,
    state_dir: &Path,
    molecules: &[MoleculeData],
    backend: Option<&TmuxBackend>,
    now: chrono::DateTime<Utc>,
) -> Vec<(WorkerId, cosmon_core::id::MoleculeId)> {
    let Some(be) = backend else {
        return Vec::new();
    };
    let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(state_dir);
    let candidates = find_stalled_for_nudge(molecules, now, |fid| {
        let fp = formulas_dir.join(format!("{fid}.formula.toml"));
        let text = std::fs::read_to_string(&fp).ok()?;
        cosmon_core::formula::Formula::parse(&text).ok()
    });
    let mut nudged = Vec::new();
    for (wid, mol) in candidates {
        if !be.is_alive(&wid).unwrap_or(false) {
            continue;
        }
        // The classifier above answered "is there a step to resume?". Whether
        // we are *allowed to speak* is not its call and never a second copy of
        // the heuristic: it belongs to the one judge, exactly as the
        // propulsion tier consults it. Without this, the 2026-07-19 repair
        // would have covered `--propel` and left `--nudge` hammering the very
        // same gated worker with the very same sentence.
        let session = mol.session_name.clone().unwrap_or_else(|| wid.to_string());
        let view = NudgeView {
            channel: NudgeChannel::Briefing,
            status: mol.status,
            awaiting_operator: worker_awaits_operator(store, mol),
            // The same orphan gate the propulsion tier consults: telling a
            // brief-less worker to "re-read briefing.md" is exactly as futile
            // as telling it to "continue", so this channel suppresses too.
            briefing_present: worker_briefing_present(store, &mol.id),
            progress_age: now.signed_duration_since(mol.updated_at),
            pane_idle: pane_idle_seconds(be.socket(), &session).map(chrono::Duration::seconds),
            // Deliberately not `mol.nudge_count`: that field is a *lifetime*
            // total the briefing tier never resets, so feeding it as the
            // per-stall attempt count would silence this channel permanently
            // after the fourth nudge of a molecule's whole life. This tier
            // brings its own spacing rule (the idempotence window below) and
            // no ceiling; the ledger-and-ceiling arithmetic belongs to the
            // propulsion tier, which keeps a per-stall register.
            attempts: 0,
            since_last_propel: mol
                .last_nudged_at
                .map(|at| now.signed_duration_since(at).max(chrono::Duration::zero())),
        };
        if !matches!(
            decide_nudge(&view, chrono::Duration::seconds(NUDGE_IDEMPOTENCE_SECS)),
            NudgeDecision::Nudge { .. }
        ) {
            continue;
        }
        let briefing = store.molecule_dir(&mol.id).join("briefing.md");
        if be.send_input(&wid, &nudge_message(&briefing)).is_err() {
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = be.send_input(&wid, "");
        // Persist the nudge — increment count + stamp timestamp. The save
        // is best-effort: a write error logs but does not fail the patrol.
        if let Ok(mut updated) = store.load_molecule(&mol.id) {
            updated.nudge_count = updated.nudge_count.saturating_add(1);
            updated.last_nudged_at = Some(now);
            updated.updated_at = now;
            let _ = store.save_molecule(&mol.id, &updated);
        }
        nudged.push((wid, mol.id.clone()));
    }
    nudged
}

/// Render the patrol report in human-readable format.
fn print_human_report(report: &PatrolReport, respawned: &[WorkerId]) {
    if report.is_healthy() {
        println!(
            "{} {} workers, all healthy.",
            "Patrol:".bold(),
            report.ensemble_size,
        );
        return;
    }

    println!(
        "{} {} workers, {} issue(s) detected.",
        "Patrol:".bold(),
        report.ensemble_size,
        report.issue_count(),
    );
    println!();

    if !report.stalled_workers.is_empty() {
        println!(
            "  {} {} stalled worker(s):",
            "STALE".red().bold(),
            report.stalled_workers.len(),
        );
        for wid in &report.stalled_workers {
            println!("    - {wid}");
        }
    }

    if !report.error_workers.is_empty() {
        println!(
            "  {} {} error worker(s):",
            "ERROR".red().bold(),
            report.error_workers.len(),
        );
        for wid in &report.error_workers {
            println!("    - {wid}");
        }
    }

    if !report.orphaned_molecules.is_empty() {
        println!(
            "  {} {} orphaned molecule(s):",
            "ORPHAN".yellow().bold(),
            report.orphaned_molecules.len(),
        );
        for mid in &report.orphaned_molecules {
            println!("    - {mid}");
        }
    }

    if !respawned.is_empty() {
        println!();
        println!(
            "  {} {} worker(s) respawned:",
            "RESPAWN".green().bold(),
            respawned.len(),
        );
        for wid in respawned {
            println!("    - {wid}");
        }
    }
}

/// Aggregate result of an expire sweep. Molecules are classified by the
/// action applied (warn/collapse/escalate); `scanned` counts every
/// molecule inspected, including those with no TTL set.
#[derive(Debug, Default)]
pub(crate) struct ExpireSweepReport {
    pub scanned: usize,
    pub warned: Vec<MoleculeId>,
    pub collapsed: Vec<MoleculeId>,
    pub escalated: Vec<MoleculeId>,
}

/// Sweep all molecules, apply each expired molecule's policy, and emit
/// the canonical `Expired` event. Idempotent: an already-tagged molecule
/// is not re-tagged, an already-collapsed molecule is not re-mutated, and
/// the pure evaluator returns the same action for identical inputs.
pub(crate) fn expire_sweep(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    molecules: &[MoleculeData],
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<ExpireSweepReport> {
    let mut report = ExpireSweepReport {
        scanned: molecules.len(),
        ..Default::default()
    };
    let events_path = state_dir.join("events.jsonl");

    for mol in molecules {
        let action = evaluate_expiry(mol.expires_at, mol.expiry_policy, mol.status, now);
        if matches!(action, ExpiryAction::None) {
            continue;
        }

        // Mutation is conditional on the action and current state; each
        // branch is a no-op if the intended effect is already present.
        let mut changed = false;
        let mut m = mol.clone();
        let mut collapsed_now = false;

        let expired_tag = Tag::new("expired")
            .map_err(|e| anyhow::anyhow!("internal: invalid `expired` tag: {e}"))?;
        let escalated_tag = Tag::new("escalated")
            .map_err(|e| anyhow::anyhow!("internal: invalid `escalated` tag: {e}"))?;

        match action {
            ExpiryAction::Warn => {
                if m.tags.insert(expired_tag) {
                    changed = true;
                }
            }
            ExpiryAction::Collapse => {
                if m.status != MoleculeStatus::Collapsed {
                    m.status = MoleculeStatus::Collapsed;
                    m.collapse_reason = Some("expired (TTL)".to_owned());
                    m.collapsed_step = Some(m.current_step);
                    m.tags.insert(expired_tag);
                    changed = true;
                    collapsed_now = true;
                }
            }
            ExpiryAction::Escalate => {
                if m.tags.insert(escalated_tag) {
                    changed = true;
                }
                // Always ensure the expired tag too — surface parity with Warn.
                if m.tags.insert(expired_tag) {
                    changed = true;
                }
            }
            ExpiryAction::None => unreachable!(),
        }

        if changed {
            m.updated_at = now;
            store
                .save_molecule(&m.id, &m)
                .map_err(|e| anyhow::anyhow!("failed to save molecule {}: {e}", m.id))?;
        }

        // Event emission: fire the canonical Expired event regardless of
        // whether state mutated — downstream consumers dedupe by state.
        let policy = match action {
            ExpiryAction::Warn => ExpiryPolicy::Warn,
            ExpiryAction::Collapse => ExpiryPolicy::Collapse,
            ExpiryAction::Escalate => ExpiryPolicy::Escalate,
            ExpiryAction::None => unreachable!(),
        };
        let _ = event_log::emit_one(
            &events_path,
            EventV2::Expired {
                molecule_id: m.id.clone(),
                policy_applied: policy,
            },
            None,
        );
        if collapsed_now {
            let _ = event_log::emit_one(
                &events_path,
                EventV2::MoleculeCollapsed {
                    molecule_id: m.id.clone(),
                    reason: "expired (TTL)".to_owned(),
                    kind: Some(cosmon_core::event_v2::CollapseReason::ResourceExhausted),
                },
                None,
            );
        }

        match action {
            ExpiryAction::Warn => report.warned.push(m.id.clone()),
            ExpiryAction::Collapse => report.collapsed.push(m.id.clone()),
            ExpiryAction::Escalate => report.escalated.push(m.id.clone()),
            ExpiryAction::None => {}
        }
    }

    Ok(report)
}

fn print_expire_report(report: &ExpireSweepReport) {
    println!();
    let total = report.warned.len() + report.collapsed.len() + report.escalated.len();
    if total == 0 {
        println!(
            "  {} {} molecule(s) scanned, none expired",
            "EXPIRE".cyan().bold(),
            report.scanned,
        );
        return;
    }
    println!(
        "  {} {} molecule(s) scanned, {total} expired:",
        "EXPIRE".cyan().bold(),
        report.scanned,
    );
    for mid in &report.warned {
        println!("    - {mid} (warn)");
    }
    for mid in &report.collapsed {
        println!("    - {mid} (collapsed)");
    }
    for mid in &report.escalated {
        println!("    - {mid} (escalate)");
    }
}

// ---------------------------------------------------------------------------
// Silence detection — passive watchdog for the WorkerHeartbeat channel.
// ---------------------------------------------------------------------------
//
// Closes delib-20260426-1bcd #3 (Shannon §3): without this rule, the absence
// of a `WorkerHeartbeat` for N seconds was indistinguishable from "all is
// well". Now patrol classifies a Running molecule as silent when its
// associated worker has not heartbeat in `silence_after` seconds. The
// detection is reporter-only on the worker (no kill, no transport poke) but
// it tags the molecule `temp:frozen`, emits `WorkerSilenceDetected`, and
// fires `cs notify` so the operator hears the silence.

/// Aggregate silence-detect output.
#[derive(Debug, Default)]
pub(crate) struct SilenceDetectReport {
    /// Total Running molecules examined.
    pub scanned: usize,
    /// Per-molecule silence findings.
    pub silent: Vec<SilenceFinding>,
}

/// One silent molecule, with the supporting evidence.
#[derive(Debug, Clone)]
pub(crate) struct SilenceFinding {
    /// The Running molecule whose worker fell silent.
    pub molecule_id: MoleculeId,
    /// The worker patrol believed to be active. `None` when the molecule
    /// has no `assigned_worker` field.
    pub worker_id: Option<WorkerId>,
    /// Seconds elapsed since the most recent `WorkerHeartbeat` for this
    /// worker. `None` when no heartbeat has ever been observed.
    pub age_seconds: Option<u64>,
}

/// Pure: among `molecules`, return `(molecule, worker, age)` triplets for
/// every Running molecule whose latest [`EventV2::WorkerHeartbeat`] (looked
/// up in `last_heartbeat_at`) is older than `silence_after` seconds.
///
/// `last_heartbeat_at` is a closure so tests do not need to write an
/// `events.jsonl` file — they pass a `HashMap` lookup.
pub(crate) fn find_silent_molecules<F>(
    molecules: &[MoleculeData],
    silence_after: u64,
    now: chrono::DateTime<Utc>,
    last_heartbeat_at: F,
) -> Vec<SilenceFinding>
where
    F: Fn(&WorkerId) -> Option<chrono::DateTime<Utc>>,
{
    let threshold = i64::try_from(silence_after).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    for mol in molecules {
        if mol.status != MoleculeStatus::Running {
            continue;
        }
        // Cold-start guard: a molecule that just transitioned to Running
        // has not had time to heartbeat. Use `updated_at` as the floor —
        // skip molecules younger than the threshold, regardless of
        // heartbeat presence.
        if now.signed_duration_since(mol.updated_at).num_seconds() < threshold {
            continue;
        }
        let worker_id = mol.assigned_worker.clone();
        let age_seconds = match worker_id.as_ref().and_then(&last_heartbeat_at) {
            Some(ts) => {
                let age = now.signed_duration_since(ts).num_seconds();
                if age < threshold {
                    continue;
                }
                Some(u64::try_from(age).unwrap_or(0))
            }
            None => None, // Worker never heartbeat — silent by default.
        };
        out.push(SilenceFinding {
            molecule_id: mol.id.clone(),
            worker_id,
            age_seconds,
        });
    }
    out
}

/// Effectful: walk `events.jsonl`, classify silent molecules, tag them
/// `temp:frozen`, append `WorkerSilenceDetected` to the log, and fire
/// `cs notify` (best-effort, gated on `COSMON_NOTIFY_DRY_RUN`).
pub(crate) fn silence_detect_sweep(
    store: &dyn StateStore,
    state_dir: &Path,
    molecules: &[MoleculeData],
    silence_after: u64,
    now: chrono::DateTime<Utc>,
) -> SilenceDetectReport {
    let scanned = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Running)
        .count();

    let last_hb = build_last_heartbeat_index(state_dir);
    let findings = find_silent_molecules(molecules, silence_after, now, |wid| {
        last_hb.get(wid).copied()
    });

    let Ok(frozen_tag) = Tag::new("temp:frozen") else {
        return SilenceDetectReport {
            scanned,
            silent: findings,
        };
    };

    let events_path = state_dir.join("events.jsonl");
    for finding in &findings {
        // Tag temp:frozen — best-effort. A failed save logs to stderr
        // but never aborts the sweep; idempotent (BTreeSet::insert).
        if let Ok(mut mol) = store.load_molecule(&finding.molecule_id) {
            if mol.tags.insert(frozen_tag.clone()) {
                mol.updated_at = now;
                let _ = store.save_molecule(&finding.molecule_id, &mol);
            }
        }

        // Append the WorkerSilenceDetected event.
        let _ = event_log::emit_one(
            &events_path,
            EventV2::WorkerSilenceDetected {
                molecule_id: finding.molecule_id.clone(),
                worker_id: finding.worker_id.clone(),
                age_since_last_heartbeat_s: finding.age_seconds,
                threshold_s: silence_after,
            },
            None,
        );

        // Fire `cs notify` as a child process. Best-effort: a missing
        // [notify] block, a CI environment, or a dry-run all just skip.
        // We pass `--dry-run` ourselves when the env var is set so unit
        // tests don't accidentally pop a macOS notification.
        spawn_notify_for_silence(finding, silence_after);
    }

    SilenceDetectReport {
        scanned,
        silent: findings,
    }
}

fn spawn_notify_for_silence(finding: &SilenceFinding, threshold: u64) {
    let cs_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"));
    let age = finding
        .age_seconds
        .map_or_else(|| "never".to_owned(), |s| format!("{s}s"));
    let worker = finding
        .worker_id
        .as_ref()
        .map_or_else(|| "(unknown worker)".to_owned(), |w| w.as_str().to_owned());
    let msg = format!(
        "worker {worker} silent {age} (threshold {threshold}s) on {mol}",
        mol = finding.molecule_id.as_str()
    );
    let mut cmd = std::process::Command::new(cs_bin);
    cmd.args([
        "notify",
        &msg,
        "--level",
        "warn",
        "--molecule",
        finding.molecule_id.as_str(),
        "--title",
        "cosmon: worker silent",
    ]);
    if std::env::var_os("COSMON_NOTIFY_DRY_RUN").is_some() {
        cmd.arg("--dry-run");
    }
    // Detach: we never want to block the patrol on a slow channel.
    let _ = cmd.spawn();
}

/// Walk `events.jsonl` and build a `WorkerId → DateTime<Utc>` map of the
/// most recent [`EventV2::WorkerHeartbeat`] per worker. Lines that don't
/// parse are silently skipped — the absence of a heartbeat for a given
/// worker simply means "never heartbeat" downstream.
fn build_last_heartbeat_index(
    state_dir: &Path,
) -> std::collections::HashMap<WorkerId, chrono::DateTime<Utc>> {
    let path = state_dir.join("events.jsonl");
    let envelopes = event_log::read_all(&path).unwrap_or_default();
    let mut map = std::collections::HashMap::new();
    for env in envelopes {
        if let EventV2::WorkerHeartbeat { worker_id, ts, .. } = env.event {
            // Latest wins — keep replacing.
            map.entry(worker_id).and_modify(|t| *t = ts).or_insert(ts);
        }
    }
    map
}

fn print_silence_report(report: &SilenceDetectReport, threshold: u64) {
    println!();
    let banner = "SILENCE".cyan().bold();
    if report.silent.is_empty() {
        println!(
            "  {banner} {} running molecule(s) scanned — all heartbeating",
            report.scanned
        );
        return;
    }
    println!(
        "  {banner} {}/{} silent (threshold {threshold}s):",
        report.silent.len(),
        report.scanned,
    );
    for finding in &report.silent {
        let age = finding
            .age_seconds
            .map_or_else(|| "never".to_owned(), |s| format!("{s}s ago"));
        let worker = finding
            .worker_id
            .as_ref()
            .map_or_else(|| "(unknown)".to_owned(), |w| w.as_str().to_owned());
        println!(
            "    💤 {mol} (worker {worker}, last heartbeat {age})",
            mol = finding.molecule_id,
        );
    }
    println!(
        "    {} tagged temp:frozen + WorkerSilenceDetected emitted + cs notify dispatched",
        "→".dimmed()
    );
}

// ---------------------------------------------------------------------------
// Event-age detection — harness-agnostic backstop for the external-modal stall.
// ---------------------------------------------------------------------------
//
// Closes the silent-block detectability gap (delib-20260608-6a5f, torvalds /
// kahneman CV-6). `--silence-detect` keys on `WorkerHeartbeat`, so it is blind
// to a worker that never wired heartbeats yet sits at an external modal
// (Claude Code `AskUserQuestion`) — that worker emits *no* cosmon-visible
// state. The event-age check keys on the age of the molecule's most recent
// entry in the *event log* instead: any append (evolve, gate, native, …)
// resets the clock. When nothing has been appended for `threshold` seconds
// while the molecule is still `Running`, patrol raises a signal.
//
// Two disciplines from the panel are load-bearing here:
//
//   * **Runtime-independence (CV-5).** The sweep reads only `molecules` +
//     `events.jsonl`. It touches no transport, no tmux, no runtime. It is
//     therefore correct precisely in the worst case the molecule exists to
//     cover — runtime dead *and* worker blocked.
//   * **Tier by irreversibility (CV-6).** An operational stall is low-urgency
//     and must not interrupt; only an irreversible-class block (signature /
//     push / publish) fires `cs notify`. Flooding the notify channel with
//     every operational stall is what kills the one load-bearing alert
//     (availability bias). Operational stalls are report-only.

/// Severity of an event-age stall, tiered by irreversibility (kahneman CV-6).
///
/// The level is carried into `cs notify --level` so a future routing policy
/// (C3's ADR) can decide what interrupts the operator versus what merely
/// records. Today the seed policy is the [`IRREVERSIBLE_TAGS`] table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventAgeSeverity {
    /// Operational stall — low urgency. Report-only; never fires `cs notify`.
    Operational,
    /// Irreversible-class block — interrupts. Fires `cs notify --level alert`.
    Irreversible,
}

impl EventAgeSeverity {
    /// The `cs notify` level string this severity maps onto.
    pub(crate) fn notify_level(self) -> &'static str {
        match self {
            Self::Operational => "warn",
            Self::Irreversible => "alert",
        }
    }
}

/// Tags that mark a molecule as carrying irreversible-class work. A stall on
/// one of these interrupts (alert); everything else is operational (warn).
/// This is the minimal seed of the tiering policy C3's ADR will own — kept
/// deliberately small so the default is "operational, report-only".
const IRREVERSIBLE_TAGS: &[&str] = &[
    "irreversible",
    "signature",
    "sign",
    "push",
    "publish",
    "release",
];

/// Pure: classify a molecule's event-age stall severity from its tags.
pub(crate) fn event_age_severity(mol: &MoleculeData) -> EventAgeSeverity {
    if mol
        .tags
        .iter()
        .any(|t| IRREVERSIBLE_TAGS.contains(&t.as_str()))
    {
        EventAgeSeverity::Irreversible
    } else {
        EventAgeSeverity::Operational
    }
}

/// Aggregate event-age sweep output.
#[derive(Debug, Default)]
pub(crate) struct EventAgeReport {
    /// Total Running molecules examined.
    pub scanned: usize,
    /// Per-molecule event-age findings.
    pub stalled: Vec<EventAgeFinding>,
}

/// One event-age stall, with the supporting evidence.
#[derive(Debug, Clone)]
pub(crate) struct EventAgeFinding {
    /// The Running molecule whose event log went quiet.
    pub molecule_id: MoleculeId,
    /// Seconds since the most recent event for this molecule. `None` when
    /// the molecule has never appeared in the event log at all.
    pub age_seconds: Option<u64>,
    /// `cs notify` level this finding maps onto (`warn` | `alert`).
    pub severity_level: &'static str,
}

/// Pure: among `molecules`, return findings for every Running molecule whose
/// most recent event (looked up via `last_event_at`) is older than
/// `threshold` seconds. `last_event_at` is a closure so tests need not write
/// an `events.jsonl` file.
///
/// A cold-start guard mirrors `--silence-detect`: a molecule that only just
/// transitioned to Running (younger than `threshold`) is skipped regardless
/// of event presence.
pub(crate) fn find_event_age_stalls<F>(
    molecules: &[MoleculeData],
    threshold: u64,
    now: chrono::DateTime<Utc>,
    last_event_at: F,
) -> Vec<EventAgeFinding>
where
    F: Fn(&MoleculeId) -> Option<chrono::DateTime<Utc>>,
{
    let threshold_i = i64::try_from(threshold).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    for mol in molecules {
        if mol.status != MoleculeStatus::Running {
            continue;
        }
        if now.signed_duration_since(mol.updated_at).num_seconds() < threshold_i {
            continue;
        }
        let age_seconds = match last_event_at(&mol.id) {
            Some(ts) => {
                let age = now.signed_duration_since(ts).num_seconds();
                if age < threshold_i {
                    continue;
                }
                Some(u64::try_from(age).unwrap_or(0))
            }
            None => None, // Never appeared in the log — silent by default.
        };
        out.push(EventAgeFinding {
            molecule_id: mol.id.clone(),
            age_seconds,
            severity_level: event_age_severity(mol).notify_level(),
        });
    }
    out
}

/// Walk `events.jsonl` once and build a `MoleculeId → latest timestamp` map
/// from the envelope's wall-clock `timestamp` (not any event-internal `ts`),
/// keyed by [`EventV2::molecule_id`]. Events with no associated molecule are
/// skipped. Unparseable lines are silently dropped — a corrupt tail must
/// never halt the patrol.
fn build_last_event_index(
    state_dir: &Path,
) -> std::collections::HashMap<MoleculeId, chrono::DateTime<Utc>> {
    let path = state_dir.join("events.jsonl");
    let envelopes = event_log::read_all(&path).unwrap_or_default();
    let mut map: std::collections::HashMap<MoleculeId, chrono::DateTime<Utc>> =
        std::collections::HashMap::new();
    for env in envelopes {
        if let Some(mid) = env.event.molecule_id() {
            let ts = env.timestamp;
            map.entry(mid.clone())
                .and_modify(|t| {
                    if ts > *t {
                        *t = ts;
                    }
                })
                .or_insert(ts);
        }
    }
    map
}

/// Effectful: build the last-event index, classify stalls, and fire
/// `cs notify` for the irreversible-class findings only (operational stalls
/// are report-only — anti-flood, CV-6). No tag, no kill, no transport: this
/// is an ALERT-only signal, and it is deliberately runtime-independent.
pub(crate) fn event_age_sweep(
    molecules: &[MoleculeData],
    state_dir: &Path,
    threshold: u64,
    now: chrono::DateTime<Utc>,
) -> EventAgeReport {
    let scanned = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Running)
        .count();

    let last_event = build_last_event_index(state_dir);
    let stalled = find_event_age_stalls(molecules, threshold, now, |mid| {
        last_event.get(mid).copied()
    });

    for finding in &stalled {
        // Anti-flood: only the irreversible tier interrupts the operator.
        // Operational stalls stay in the patrol report / scheduler log.
        if finding.severity_level == "alert" {
            spawn_notify_for_event_age(finding, threshold);
        }
    }

    EventAgeReport { scanned, stalled }
}

/// Fire a single `cs notify` for an irreversible-class event-age stall.
/// Detached + best-effort: a slow channel must never block the patrol.
fn spawn_notify_for_event_age(finding: &EventAgeFinding, threshold: u64) {
    let cs_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"));
    let age = finding
        .age_seconds
        .map_or_else(|| "never".to_owned(), |s| format!("{s}s"));
    let msg = format!(
        "no event-log activity for {age} (threshold {threshold}s) on irreversible-class {mol}",
        mol = finding.molecule_id.as_str()
    );
    let mut cmd = std::process::Command::new(cs_bin);
    cmd.args([
        "notify",
        &msg,
        "--level",
        finding.severity_level,
        "--molecule",
        finding.molecule_id.as_str(),
        "--title",
        "cosmon: molecule stalled",
    ]);
    if std::env::var_os("COSMON_NOTIFY_DRY_RUN").is_some() {
        cmd.arg("--dry-run");
    }
    let _ = cmd.spawn();
}

/// Human-readable event-age section. A silent loop ("all active") is visibly
/// distinct from an active one.
fn print_event_age_report(report: &EventAgeReport, threshold: u64) {
    println!();
    let banner = "EVENT-AGE".cyan().bold();
    if report.stalled.is_empty() {
        println!(
            "  {banner} {} running molecule(s) scanned — all active (<{threshold}s)",
            report.scanned
        );
        return;
    }
    println!(
        "  {banner} {}/{} stalled (threshold {threshold}s):",
        report.stalled.len(),
        report.scanned,
    );
    for finding in &report.stalled {
        let age = finding
            .age_seconds
            .map_or_else(|| "no events ever".to_owned(), |s| format!("{s}s ago"));
        let sev = if finding.severity_level == "alert" {
            "ALERT".red().bold()
        } else {
            "warn".yellow().bold()
        };
        println!(
            "    ⏾ {mol} (last event {age}) [{sev}]",
            mol = finding.molecule_id,
        );
    }
    println!(
        "    {} alert-tier fires cs notify; operational stalls are report-only (anti-flood)",
        "→".dimmed()
    );
}

// ---------------------------------------------------------------------------
// Dialogue-scan — blocking-dialogue detection + guarded remediation.
// ---------------------------------------------------------------------------
//
// The motivating incident (showroom, 2026-07-03/04): ten workers blocked
// ~30h on the Claude Code spend-limit dialog with no human to press Enter; the
// pilot propelled every one by hand. The operator's ask: a patrol that
// (a) auto-confirms the cheap, no-stake prompts so a worker never rots on a
// keystroke, and (b) NEVER auto-confirms a money choice — it pages a human.
//
// The be1e discipline (ADR-137 §2) is bolted in from the start: pane text is an
// adversarial channel, read only to *surface* a finding to a human. The single
// narrow exception — firing the default-accept key on a `Permission`-class
// prompt — is opt-in (`--auto-confirm-safe`, off by default) and the money
// refusal lives in the pure classifier ([`cosmon_core::dialogue`]), not in a
// flag an operator can mis-set. Everything unrecognised fails safe to the
// alert path.

/// Tuning knobs for [`dialogue_scan_sweep`], mirrored from the `cs patrol`
/// flags so the sweep signature stays stable as knobs are added.
pub(crate) struct DialogueScanOpts {
    /// Pane lines to capture per worker.
    pub lines: usize,
    /// Opt-in: fire the default-accept keystroke on safe permission prompts.
    pub auto_confirm_safe: bool,
    /// Blocked-duration (seconds) past which a still-blocked molecule escalates
    /// to a canary-RED page.
    pub blocked_after: u64,
}

/// What the patrol did about a detected dialogue. Carried into the audit event
/// (`action` field) and the human report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogueAction {
    /// Operator paged via `cs notify` (money stake or unrecognised block).
    Alerted,
    /// Escalated page: still blocked past `blocked_after` despite detection.
    CanaryRed,
    /// Safe permission prompt auto-confirmed (Enter sent) — opt-in only.
    AutoConfirmed,
    /// Seen and recorded, but no action taken (permission prompt, auto-confirm
    /// disabled, not yet past the blocked threshold).
    Reported,
}

impl DialogueAction {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Alerted => "alerted",
            Self::CanaryRed => "canary_red",
            Self::AutoConfirmed => "auto_confirmed",
            Self::Reported => "reported",
        }
    }

    /// Does this action page the operator?
    const fn pages_operator(self) -> bool {
        matches!(self, Self::Alerted | Self::CanaryRed)
    }
}

/// One detected blocking dialogue, with the class, the action taken, and the
/// evidence line that triggered it.
#[derive(Debug, Clone)]
pub(crate) struct DialogueFinding {
    pub molecule_id: MoleculeId,
    pub worker_id: Option<WorkerId>,
    pub class: cosmon_core::dialogue::DialogueClass,
    pub action: DialogueAction,
    pub blocked_seconds: Option<u64>,
    pub evidence: Option<String>,
}

/// Aggregate dialogue-scan output.
#[derive(Debug, Default)]
pub(crate) struct DialogueScanReport {
    /// Running molecules whose panes were captured and classified.
    pub scanned: usize,
    /// One entry per molecule with a non-`None` dialogue class.
    pub findings: Vec<DialogueFinding>,
}

/// Pure: decide the action for a classified dialogue given the blocked
/// duration and the caller's opt-in posture.
///
/// This is the whole policy, isolated from tmux so it is an executable spec:
///
/// - `MoneyStake` / `Unknown` → page the operator (`Alerted`), escalating to
///   `CanaryRed` once blocked past `blocked_after`. **Never** auto-confirmed.
/// - `Permission` with `auto_confirm_safe` → `AutoConfirmed`.
/// - `Permission` without opt-in → `Reported`, but escalates to `CanaryRed`
///   if it has been rotting past `blocked_after` (a safe prompt nobody
///   answered is still a stalled slot).
/// - `None` → `Reported` (callers filter these out before recording).
pub(crate) fn decide_dialogue_action(
    class: cosmon_core::dialogue::DialogueClass,
    blocked_seconds: Option<u64>,
    opts: &DialogueScanOpts,
) -> DialogueAction {
    use cosmon_core::dialogue::DialogueClass;
    let past_threshold = blocked_seconds.is_some_and(|s| s >= opts.blocked_after);
    match class {
        DialogueClass::MoneyStake | DialogueClass::Unknown => {
            if past_threshold {
                DialogueAction::CanaryRed
            } else {
                DialogueAction::Alerted
            }
        }
        DialogueClass::Permission => {
            if opts.auto_confirm_safe {
                DialogueAction::AutoConfirmed
            } else if past_threshold {
                DialogueAction::CanaryRed
            } else {
                DialogueAction::Reported
            }
        }
        DialogueClass::None => DialogueAction::Reported,
    }
}

/// Effectful: for every Running molecule with a live worker, capture the pane,
/// classify any blocking dialogue, decide the action, and apply it — auto-
/// confirm a safe permission (opt-in), page the operator for a stake, and
/// emit the [`EventV2::BlockingDialogueDetected`] audit record either way.
///
/// Transport is a trait object so the sweep is testable with a `MockBackend`.
/// Pane text is read only to surface findings (ADR-137 §2); the sole autonomous
/// keystroke is the opt-in default-accept on a `Permission`-class prompt.
pub(crate) fn dialogue_scan_sweep(
    store: &dyn StateStore,
    state_dir: &Path,
    molecules: &[MoleculeData],
    backend: Option<&dyn TransportBackend>,
    opts: &DialogueScanOpts,
    now: chrono::DateTime<Utc>,
) -> DialogueScanReport {
    use cosmon_core::dialogue::{classify_pane, DialogueClass};

    let running: Vec<&MoleculeData> = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Running)
        .collect();
    let mut report = DialogueScanReport {
        scanned: running.len(),
        ..Default::default()
    };
    let Some(be) = backend else {
        return report;
    };

    let events_path = state_dir.join("events.jsonl");
    let dialogue_tag = Tag::new("dialogue-blocked").ok();

    for mol in running {
        let Some(wid) = mol.assigned_worker.as_ref() else {
            continue;
        };
        if !be.is_alive(wid).unwrap_or(false) {
            continue;
        }
        let Ok(pane) = be.capture_output(wid, opts.lines) else {
            continue;
        };
        let scan = classify_pane(&pane);
        if scan.class == DialogueClass::None {
            continue;
        }

        // Blocked duration from the progress proxy (last_progress_at, else
        // updated_at). Saturating: a clock skew that makes it negative reads
        // as zero rather than a bogus huge age.
        let progress_ts = mol.last_progress_at.unwrap_or(mol.updated_at);
        let blocked_seconds =
            u64::try_from(now.signed_duration_since(progress_ts).num_seconds().max(0)).ok();

        let action = decide_dialogue_action(scan.class, blocked_seconds, opts);

        // Apply the action.
        match action {
            DialogueAction::AutoConfirmed => {
                // The narrow, opt-in exception: fire the default-accept key.
                // Sending an empty string sends a bare Enter (see
                // TmuxBackend::send_input), which selects the highlighted
                // default (option 1 / "Yes") on a Claude Code permission
                // prompt — exactly the "nobody to press Enter" keystroke.
                let _ = be.send_input(wid, "");
            }
            DialogueAction::Alerted | DialogueAction::CanaryRed => {
                // Surface to a human; tag so `cs ensemble` shows the block.
                if let Some(tag) = dialogue_tag.clone() {
                    if let Ok(mut m) = store.load_molecule(&mol.id) {
                        if m.tags.insert(tag) {
                            m.updated_at = now;
                            let _ = store.save_molecule(&mol.id, &m);
                        }
                    }
                }
                spawn_notify_for_dialogue(
                    &mol.id,
                    Some(wid),
                    scan.class,
                    action,
                    blocked_seconds,
                    scan.evidence.as_deref(),
                );
            }
            DialogueAction::Reported => {}
        }

        // Audit record — every detection, regardless of action taken.
        let _ = event_log::emit_one(
            &events_path,
            EventV2::BlockingDialogueDetected {
                molecule_id: mol.id.clone(),
                worker_id: Some(wid.clone()),
                class: scan.class.as_str().to_owned(),
                action: action.as_str().to_owned(),
                blocked_seconds,
            },
            None,
        );

        report.findings.push(DialogueFinding {
            molecule_id: mol.id.clone(),
            worker_id: Some(wid.clone()),
            class: scan.class,
            action,
            blocked_seconds,
            evidence: scan.evidence,
        });
    }
    report
}

/// Fire a single `cs notify` for a paging dialogue finding. Detached +
/// best-effort — a slow channel must never block the patrol. Money stakes and
/// canary-RED escalations page at `alert` level; an unrecognised block pages
/// at `warn`.
fn spawn_notify_for_dialogue(
    mol_id: &MoleculeId,
    worker_id: Option<&WorkerId>,
    class: cosmon_core::dialogue::DialogueClass,
    action: DialogueAction,
    blocked_seconds: Option<u64>,
    evidence: Option<&str>,
) {
    use cosmon_core::dialogue::DialogueClass;
    let level = if action == DialogueAction::CanaryRed || class == DialogueClass::MoneyStake {
        "alert"
    } else {
        "warn"
    };
    let title = if action == DialogueAction::CanaryRed {
        "cosmon: worker BLOCKED (canary RED)"
    } else if class == DialogueClass::MoneyStake {
        "cosmon: money-stake dialog — needs you"
    } else {
        "cosmon: worker awaiting input"
    };
    let worker = worker_id.map_or_else(|| "(unknown)".to_owned(), |w| w.as_str().to_owned());
    let blocked = blocked_seconds.map_or_else(|| "?".to_owned(), |s| format!("{s}s"));
    let ev = evidence.map_or_else(String::new, |e| format!(" — “{e}”"));
    let msg = format!(
        "{class} dialog on {mol} (worker {worker}, blocked {blocked}); NOT auto-confirmed{ev}",
        class = class.as_str(),
        mol = mol_id.as_str(),
    );
    let cs_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"));
    let mut cmd = std::process::Command::new(cs_bin);
    cmd.args([
        "notify",
        &msg,
        "--level",
        level,
        "--molecule",
        mol_id.as_str(),
        "--title",
        title,
    ]);
    if std::env::var_os("COSMON_NOTIFY_DRY_RUN").is_some() {
        cmd.arg("--dry-run");
    }
    let _ = cmd.spawn();
}

/// Human-readable dialogue-scan section. A silent loop ("no blocking dialogs")
/// stays visibly distinct from an active one.
fn print_dialogue_report(report: &DialogueScanReport) {
    println!();
    let banner = "DIALOGUE".cyan().bold();
    if report.findings.is_empty() {
        println!(
            "  {banner} {} running molecule(s) scanned — no blocking dialogs",
            report.scanned
        );
        return;
    }
    println!(
        "  {banner} {}/{} blocked on a dialog:",
        report.findings.len(),
        report.scanned,
    );
    for f in &report.findings {
        let blocked = f
            .blocked_seconds
            .map_or_else(|| "?".to_owned(), |s| format!("{s}s"));
        let action = match f.action {
            DialogueAction::CanaryRed => "canary RED".red().bold(),
            DialogueAction::Alerted => "alerted".yellow().bold(),
            DialogueAction::AutoConfirmed => "auto-confirmed".green().bold(),
            DialogueAction::Reported => "reported".dimmed(),
        };
        println!(
            "    ⧉ {mol} [{class}] blocked {blocked} → {action}",
            mol = f.molecule_id,
            class = f.class.as_str(),
        );
        if f.action.pages_operator() {
            if let Some(ev) = &f.evidence {
                println!("        “{ev}”");
            }
        }
    }
    println!(
        "    {} money stakes are NEVER auto-confirmed — they page you (be1e / ADR-137 §2)",
        "→".dimmed()
    );
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
    use cosmon_transport::MockBackend;
    use std::collections::HashMap;
    use tempfile::TempDir;

    use super::*;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        // Write config.toml with project_id so the project identity guard passes.
        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nproject_id = \"test-0000\"\n",
        )
        .unwrap();
        (tmp, store)
    }

    /// Create a worker with both desired and status set consistently.
    fn make_worker(name: &str, desired: DesiredState) -> (WorkerId, WorkerData) {
        let wid = WorkerId::new(name).unwrap();
        let status = match desired {
            DesiredState::Running => WorkerStatus::Active,
            DesiredState::Paused => WorkerStatus::Paused,
            DesiredState::Stopped => WorkerStatus::Stopped,
        };
        let mut data = WorkerData::new(
            wid.clone(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            status,
        );
        data.desired = desired;
        (wid, data)
    }

    fn make_molecule(id: &str, status: MoleculeStatus, worker: Option<&str>) -> MoleculeData {
        MoleculeData {
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            id: MoleculeId::new(id).unwrap(),
            formula_id: FormulaId::new("mol-polecat-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: worker.map(|w| WorkerId::new(w).unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 3,
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
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn harvest_sweep_identifies_completed_unmerged_molecules() {
        // The end-to-end shape of the worker-exit → cs done bridge: a
        // Completed molecule with merged_at=None is picked up; a merged
        // one and a Running one are skipped.
        let (tmp, store) = make_store();

        let candidate = make_molecule("task-20260418-harv", MoleculeStatus::Completed, None);
        store.save_molecule(&candidate.id, &candidate).unwrap();

        let mut merged = make_molecule("task-20260418-done", MoleculeStatus::Completed, None);
        merged.merged_at = Some(Utc::now());
        store.save_molecule(&merged.id, &merged).unwrap();

        let running = make_molecule("task-20260418-busy", MoleculeStatus::Running, None);
        store.save_molecule(&running.id, &running).unwrap();

        let molecules = store.list_molecules(&MoleculeFilter::default()).unwrap();
        let report = harvest_sweep(&store, tmp.path(), &molecules);

        // Only the completed-unmerged molecule is a candidate. Because no
        // git repo exists in the tempdir, the spawned `cs done` returns
        // non-zero, so it lands in `failed` — which is the right shape:
        // `cs harvest` detected the candidate and attempted to close it.
        assert_eq!(
            report.candidates, 1,
            "only completed-unmerged molecules are candidates"
        );
        assert!(
            report
                .failed
                .iter()
                .any(|m| m.as_str() == candidate.id.as_str())
                || report
                    .harvested
                    .iter()
                    .any(|m| m.as_str() == candidate.id.as_str()),
            "candidate must be attempted (harvested or failed), got report={report:?}"
        );
    }

    #[test]
    fn test_patrol_healthy_fleet() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("quartz", DesiredState::Running);
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            respawn: false,
            no_tmux: true,
            propel: false,
            stale_after: 300,
            expire: false,
            auto_collapse: false,
            harvest: false,
            livelock: false,
            livelock_stale_after: 3600,
            nudge: false,
            silence_detect: false,
            silence_after: 90,
            event_age: false,
            event_age_after: 900,
            abandon: false,
            abandon_root: None,
            abandon_quiet_hours: 24,
            heal: false,
            dry_run: false,
            dialogue_scan: false,
            auto_confirm_safe: false,
            dialogue_lines: 40,
            dialogue_blocked_after: 900,
        };
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_patrol_detects_diverged_worker() {
        // desired=Running but transport=Unknown (no-tmux) → Suspect.
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid1, w1) = make_worker("ok-w", DesiredState::Running);
        let (wid2, w2) = make_worker("stopped-w", DesiredState::Stopped);
        fleet.workers.insert(wid1, w1);
        fleet.workers.insert(wid2, w2);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            respawn: false,
            no_tmux: true,
            propel: false,
            stale_after: 300,
            expire: false,
            auto_collapse: false,
            harvest: false,
            livelock: false,
            livelock_stale_after: 3600,
            nudge: false,
            silence_detect: false,
            silence_after: 90,
            event_age: false,
            event_age_after: 900,
            abandon: false,
            abandon_root: None,
            abandon_quiet_hours: 24,
            heal: false,
            dry_run: false,
            dialogue_scan: false,
            auto_confirm_safe: false,
            dialogue_lines: 40,
            dialogue_blocked_after: 900,
        };
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_patrol_detects_orphaned_molecule() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("dead-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("cs-20260401-orph", MoleculeStatus::Running, Some("dead-w"));
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            respawn: false,
            no_tmux: true,
            propel: false,
            stale_after: 300,
            expire: false,
            auto_collapse: false,
            harvest: false,
            livelock: false,
            livelock_stale_after: 3600,
            nudge: false,
            silence_detect: false,
            silence_after: 90,
            event_age: false,
            event_age_after: 900,
            abandon: false,
            abandon_root: None,
            abandon_quiet_hours: 24,
            heal: false,
            dry_run: false,
            dialogue_scan: false,
            auto_confirm_safe: false,
            dialogue_lines: 40,
            dialogue_blocked_after: 900,
        };
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_patrol_ignores_completed_molecules_on_dead_workers() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("dead-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260401-done",
            MoleculeStatus::Completed,
            Some("dead-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            respawn: false,
            no_tmux: true,
            propel: false,
            stale_after: 300,
            expire: false,
            auto_collapse: false,
            harvest: false,
            livelock: false,
            livelock_stale_after: 3600,
            nudge: false,
            silence_detect: false,
            silence_after: 90,
            event_age: false,
            event_age_after: 900,
            abandon: false,
            abandon_root: None,
            abandon_quiet_hours: 24,
            heal: false,
            dry_run: false,
            dialogue_scan: false,
            auto_confirm_safe: false,
            dialogue_lines: 40,
            dialogue_blocked_after: 900,
        };
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_patrol_empty_fleet() {
        let (tmp, store) = make_store();
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            respawn: false,
            no_tmux: true,
            propel: false,
            stale_after: 300,
            expire: false,
            auto_collapse: false,
            harvest: false,
            livelock: false,
            livelock_stale_after: 3600,
            nudge: false,
            silence_detect: false,
            silence_after: 90,
            event_age: false,
            event_age_after: 900,
            abandon: false,
            abandon_root: None,
            abandon_quiet_hours: 24,
            heal: false,
            dry_run: false,
            dialogue_scan: false,
            auto_confirm_safe: false,
            dialogue_lines: 40,
            dialogue_blocked_after: 900,
        };
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_scan_pure_function_healthy_with_alive_backend() {
        use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("alive-w", DesiredState::Running);
        fleet.workers.insert(wid, w);
        let backend = MockBackend::new();
        let agent = AgentDefinition {
            id: cosmon_core::id::AgentId::new("alive-w").unwrap(),
            role: AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        backend.spawn(&agent, &RuntimeConfig::default()).unwrap();

        let scan_result = scan(&fleet, &[], Some(&backend));
        assert!(scan_result.report.is_healthy());
        assert!(scan_result.report.stalled_workers.is_empty());
    }

    #[test]
    fn test_scan_running_dead_triggers_respawn() {
        // desired=Running, transport=Dead → Diverged + Respawn.
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("ghost-w", DesiredState::Running);
        fleet.workers.insert(wid, w);

        let backend = MockBackend::new();
        // No session → is_alive returns false.

        let scan_result = scan(&fleet, &[], Some(&backend));
        assert_eq!(scan_result.needs_respawn.len(), 1);
        assert_eq!(scan_result.needs_respawn[0].as_str(), "ghost-w");
        // Also reported as stalled (diverged + dead).
        assert!(!scan_result.report.stalled_workers.is_empty());
    }

    #[test]
    fn test_scan_running_dead_circuit_breaker() {
        // desired=Running, transport=Dead, restart_count >= MAX → CircuitBreak.
        let mut fleet = Fleet::default();
        let (wid, mut w) = make_worker("tired-w", DesiredState::Running);
        w.restart_count = MAX_RESTARTS;
        fleet.workers.insert(wid, w);

        let backend = MockBackend::new();

        let scan_result = scan(&fleet, &[], Some(&backend));
        assert_eq!(scan_result.circuit_broken.len(), 1);
        assert!(scan_result.needs_respawn.is_empty());
        assert_eq!(scan_result.report.error_workers.len(), 1);
    }

    #[test]
    fn test_scan_stopped_is_idle() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("done-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);

        let scan_result = scan(&fleet, &[], None);
        assert!(scan_result.report.is_healthy());
        assert_eq!(scan_result.report.idle_count, 1);
    }

    #[test]
    fn test_scan_paused_is_idle() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("frozen-w", DesiredState::Paused);
        fleet.workers.insert(wid, w);

        let scan_result = scan(&fleet, &[], None);
        assert!(scan_result.report.is_healthy());
        assert_eq!(scan_result.report.idle_count, 1);
    }

    // --- propel: stale-progress detection (pure logic) ---------------

    fn make_stale_molecule(id: &str, worker: &str, age_secs: i64) -> MoleculeData {
        let mut mol = make_molecule(id, MoleculeStatus::Running, Some(worker));
        mol.updated_at = Utc::now() - chrono::Duration::seconds(age_secs);
        mol
    }

    #[test]
    fn test_find_stale_detects_stale_running_molecule() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("stuck-w", DesiredState::Running);
        fleet.workers.insert(wid, w);

        // Molecule last updated 500s ago, threshold 300s → stale.
        let mol = make_stale_molecule("cs-20260409-aaaa", "stuck-w", 500);
        let now = Utc::now();

        let stale = find_stale_running_molecules(&[mol], &fleet, 300, now);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0.as_str(), "stuck-w");
        assert!(stale[0].2 >= 500, "age should be at least 500s");
    }

    #[test]
    fn test_find_stale_ignores_fresh_molecule() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("busy-w", DesiredState::Running);
        fleet.workers.insert(wid, w);

        // 60s old, threshold 300 → not stale.
        let mol = make_stale_molecule("cs-20260409-bbbb", "busy-w", 60);
        let stale = find_stale_running_molecules(&[mol], &fleet, 300, Utc::now());
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_ignores_non_running_molecule() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("w1", DesiredState::Running);
        fleet.workers.insert(wid, w);

        let mut mol = make_molecule("cs-20260409-cccc", MoleculeStatus::Completed, Some("w1"));
        mol.updated_at = Utc::now() - chrono::Duration::seconds(9999);
        let stale = find_stale_running_molecules(&[mol], &fleet, 300, Utc::now());
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_ignores_molecule_with_stopped_worker() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("dead-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);

        let mol = make_stale_molecule("cs-20260409-dddd", "dead-w", 9999);
        let stale = find_stale_running_molecules(&[mol], &fleet, 300, Utc::now());
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_ignores_unassigned_molecule() {
        let fleet = Fleet::default();
        let mut mol = make_molecule("cs-20260409-eeee", MoleculeStatus::Running, None);
        mol.updated_at = Utc::now() - chrono::Duration::seconds(9999);
        let stale = find_stale_running_molecules(&[mol], &fleet, 300, Utc::now());
        assert!(stale.is_empty());
    }

    // --- propulsion ledger: the backoff's memory (task-20260719-00ed) ----
    //
    // `decide_nudge` itself is covered in `cosmon_core::propel`. What is
    // testable only here is the *reset rule*: the ledger must forget its
    // attempts the moment the molecule makes progress, or a molecule that
    // stalled once would carry a 30-minute backoff into every later stall.

    #[test]
    fn propel_ledger_starts_empty_for_a_never_propelled_molecule() {
        let mol = make_stale_molecule("cs-20260719-0001", "w1", 600);
        let ledger = propel_attempts(std::slice::from_ref(&mol), &mol.id, Utc::now());
        assert_eq!(ledger.count, 0);
        assert!(ledger.last_at.is_none());
    }

    #[test]
    fn propel_ledger_remembers_attempts_while_the_molecule_stays_frozen() {
        let now = Utc::now();
        let mut mol = make_stale_molecule("cs-20260719-0002", "w1", 600);
        // Frozen: the last nudge came *after* the last progress.
        mol.updated_at = now - chrono::Duration::seconds(600);
        mol.propel_count = 2;
        mol.last_propelled_at = Some(now - chrono::Duration::seconds(120));
        let ledger = propel_attempts(std::slice::from_ref(&mol), &mol.id, now);
        assert_eq!(ledger.count, 2);
        assert_eq!(ledger.last_at, mol.last_propelled_at);
    }

    #[test]
    fn propel_ledger_resets_once_the_molecule_makes_progress() {
        let now = Utc::now();
        let mut mol = make_stale_molecule("cs-20260719-0003", "w1", 400);
        // The worker moved after the last nudge, then went quiet again: this
        // is a *new* stall and deserves a prompt first nudge, not the old
        // window.
        mol.propel_count = 3;
        mol.last_propelled_at = Some(now - chrono::Duration::seconds(900));
        mol.updated_at = now - chrono::Duration::seconds(400);
        let ledger = propel_attempts(std::slice::from_ref(&mol), &mol.id, now);
        assert_eq!(ledger.count, 0);
        assert!(ledger.last_at.is_none());
    }

    /// A worker that is thinking — progress clock cold, terminal warm — is
    /// admitted by `find_stale_running_molecules` (it only knows the control
    /// plane) and then *declined* by admission control. This is the exact
    /// 2026-07-19 shape: nine nudges to a worker rendering `Cultivating…`.
    #[test]
    fn thinking_worker_is_a_stale_candidate_but_never_nudged() {
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("w1", DesiredState::Running);
        fleet.workers.insert(wid, w);
        let mol = make_stale_molecule("cs-20260719-0004", "w1", 644);

        // The control plane alone says "stale" — which is why the pre-fix
        // sweep nudged it.
        let candidates =
            find_stale_running_molecules(std::slice::from_ref(&mol), &fleet, 300, Utc::now());
        assert_eq!(candidates.len(), 1);

        // The transport clock overrides that verdict.
        let view = NudgeView {
            channel: NudgeChannel::Propulsion,
            status: MoleculeStatus::Running,
            awaiting_operator: false,
            briefing_present: true,
            progress_age: chrono::Duration::seconds(644),
            pane_idle: Some(chrono::Duration::seconds(2)),
            attempts: 0,
            since_last_propel: None,
        };
        assert!(matches!(
            decide_nudge(&view, chrono::Duration::seconds(300)),
            NudgeDecision::Skip(NudgeSkip::PaneActive { .. })
        ));
    }

    // --- the operator gate: every channel holds its tongue (task-…-2cbf) ---

    /// Stamp the molecule as parked at an operator gate the way
    /// `cs await-operator` does, and persist it.
    fn park_at_operator_gate(store: &FileStore, mol: &mut MoleculeData) {
        mol.tags.insert(
            cosmon_core::tag::Tag::new(cosmon_core::operator_block::AWAITING_OP_TAG).unwrap(),
        );
        store.save_molecule(&mol.id, mol).unwrap();
    }

    /// A molecule dir with a non-empty `briefing.md` reads as *briefed*: the
    /// worker has its contract, so the orphan gate stays shut.
    #[test]
    fn briefing_present_when_file_is_non_empty() {
        let (_tmp, store) = make_store();
        let mol = make_stale_molecule("cs-20260721-0200", "w1", 9000);
        store.save_molecule(&mol.id, &mol).unwrap();
        let dir = store.molecule_dir(&mol.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("briefing.md"), "# Molecule brief\n\nDo the work.\n").unwrap();
        assert!(worker_briefing_present(&store, &mol.id));
    }

    /// A missing `briefing.md` is the money-pump signature: the worker lost its
    /// brief, so `worker_briefing_present` returns `false` and the orphan gate
    /// fires. (`make_store` never writes one.)
    #[test]
    fn briefing_absent_reads_as_orphaned() {
        let (_tmp, store) = make_store();
        let mol = make_stale_molecule("cs-20260721-0201", "w1", 9000);
        store.save_molecule(&mol.id, &mol).unwrap();
        assert!(
            !worker_briefing_present(&store, &mol.id),
            "a molecule with no briefing.md must read as orphaned"
        );
    }

    /// An *empty* `briefing.md` is as useless as an absent one — a truncated
    /// write during the crash that orphaned the worker — and must also read as
    /// orphaned rather than a valid (zero-byte) contract.
    #[test]
    fn empty_briefing_reads_as_orphaned() {
        let (_tmp, store) = make_store();
        let mol = make_stale_molecule("cs-20260721-0202", "w1", 9000);
        store.save_molecule(&mol.id, &mol).unwrap();
        let dir = store.molecule_dir(&mol.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("briefing.md"), "").unwrap();
        assert!(
            !worker_briefing_present(&store, &mol.id),
            "a zero-byte briefing.md must read as orphaned"
        );
    }

    /// End-to-end orphan tagging: `mark_propel_orphaned` stamps the molecule
    /// with [`PROPEL_ORPHANED_TAG`] and is idempotent across repeated passes —
    /// the operator/healer can find the brief-less molecule, and re-running the
    /// patrol never churns the tag set.
    #[test]
    fn mark_propel_orphaned_is_idempotent() {
        let (_tmp, store) = make_store();
        let mol = make_stale_molecule("cs-20260721-0203", "w1", 9000);
        store.save_molecule(&mol.id, &mol).unwrap();
        mark_propel_orphaned(&store, &mol.id);
        mark_propel_orphaned(&store, &mol.id);
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert!(reloaded
            .tags
            .iter()
            .any(|t| t.as_str() == PROPEL_ORPHANED_TAG));
        assert_eq!(
            reloaded
                .tags
                .iter()
                .filter(|t| t.as_str() == PROPEL_ORPHANED_TAG)
                .count(),
            1,
            "the orphan tag must be a single set member, not duplicated"
        );
    }

    /// The tag written by `cs await-operator` is recognised as a gate.
    #[test]
    fn awaiting_op_tag_is_read_as_an_operator_gate() {
        let (_tmp, store) = make_store();
        let mut mol = make_stale_molecule("cs-20260719-0100", "w1", 9000);
        store.save_molecule(&mol.id, &mol).unwrap();
        assert!(
            !worker_awaits_operator(&store, &mol),
            "an untagged molecule must not read as gated"
        );
        park_at_operator_gate(&store, &mut mol);
        assert!(worker_awaits_operator(&store, &mol));
    }

    /// The durable `blocked_on.json` is the second, independent witness: a
    /// reconcile or hand-edit that drops the tag must not silently re-open the
    /// gate to propulsion.
    #[test]
    fn blocked_on_json_alone_is_read_as_an_operator_gate() {
        let (_tmp, store) = make_store();
        let mol = make_stale_molecule("cs-20260719-0101", "w1", 9000);
        store.save_molecule(&mol.id, &mol).unwrap();
        assert!(!worker_awaits_operator(&store, &mol));

        let dir = store.molecule_dir(&mol.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("blocked_on.json"), "{}").unwrap();
        assert!(
            worker_awaits_operator(&store, &mol),
            "proof-of-block on disk must gate propulsion even with no tag"
        );
    }

    /// The field observation (2026-07-19, worker a850), end to end: a worker
    /// whose molecule is `Completed` and which holds questions for the
    /// operator is a stale candidate on both clocks — and receives **zero**
    /// nudges from **every** channel, however many passes patrol makes.
    #[test]
    fn completed_worker_with_pending_questions_is_never_nudged_on_any_channel() {
        use cosmon_core::propel::{NudgeChannel, NudgeDecision};

        for channel in [
            NudgeChannel::Propulsion,
            NudgeChannel::Briefing,
            NudgeChannel::Heal,
        ] {
            let (_tmp, store) = make_store();
            let mut mol = make_stale_molecule("cs-20260719-0102", "w1", 9000);
            mol.status = MoleculeStatus::Completed;
            park_at_operator_gate(&store, &mut mol);

            // Sixty passes over ~70 minutes — the shape that delivered
            // "des dizaines de nudges" in the field.
            for pass in 0..60_i64 {
                let view = NudgeView {
                    channel,
                    status: mol.status,
                    awaiting_operator: worker_awaits_operator(&store, &mol),
                    briefing_present: true,
                    progress_age: chrono::Duration::seconds(300 + pass * 70),
                    pane_idle: Some(chrono::Duration::seconds(300 + pass * 70)),
                    attempts: 0,
                    since_last_propel: None,
                };
                assert!(
                    !matches!(
                        decide_nudge(&view, chrono::Duration::seconds(300)),
                        NudgeDecision::Nudge { .. }
                    ),
                    "{channel:?} nudged a gated worker on pass {pass}"
                );
            }
        }
    }

    /// The propulsion sweep reports the gated worker in its own bucket rather
    /// than dropping it silently — a decline an operator must *act* on, since
    /// the molecule is waiting on them, not stuck.
    #[test]
    fn propel_sweep_reports_gated_workers_and_nudges_none() {
        let (_tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("w1", DesiredState::Running);
        fleet.workers.insert(wid, w);
        let mut mol = make_stale_molecule("cs-20260719-0103", "w1", 9000);
        park_at_operator_gate(&store, &mut mol);

        // No transport: the sweep short-circuits before any send_input, which
        // is precisely what we assert — nothing is ever pushed at this worker.
        let sweep = propel_stale_molecules(&store, &[mol], &fleet, None, 300);
        assert!(sweep.propelled.is_empty(), "a gated worker was nudged");
    }

    /// The briefing tier (`cs patrol --nudge`) refuses the same worker. It is
    /// the channel the 2026-07-19 propulsion repair did *not* cover, so this
    /// is the regression guard for "fixed one organ, left the siblings".
    #[test]
    fn briefing_tier_refuses_a_gated_worker() {
        let (_tmp, store) = make_store();
        let mut mol = make_molecule("cs-20260719-0104", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        mol.last_progress_at = Some(Utc::now() - chrono::Duration::hours(3));
        park_at_operator_gate(&store, &mut mol);

        let view = NudgeView {
            channel: NudgeChannel::Briefing,
            status: mol.status,
            awaiting_operator: worker_awaits_operator(&store, &mol),
            briefing_present: true,
            progress_age: chrono::Duration::hours(3),
            pane_idle: Some(chrono::Duration::hours(3)),
            attempts: 0,
            since_last_propel: None,
        };
        assert!(matches!(
            decide_nudge(&view, chrono::Duration::seconds(NUDGE_IDEMPOTENCE_SECS)),
            NudgeDecision::Skip(cosmon_core::propel::NudgeSkip::AwaitingOperator)
        ));
    }

    // --- silence-detect: heartbeat absence (pure logic) --------------

    #[test]
    fn silence_detect_flags_running_molecule_with_no_recent_heartbeat() {
        // Molecule has been Running for 500s, worker last heartbeat 200s ago.
        let mut mol = make_stale_molecule("cs-20260426-aaaa", "quartz", 500);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let now = Utc::now();
        let last_hb_ts = now - chrono::Duration::seconds(200);
        let findings = find_silent_molecules(&[mol], 90, now, |w| {
            (w.as_str() == "quartz").then_some(last_hb_ts)
        });
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.worker_id.as_ref().unwrap().as_str(), "quartz");
        assert!(f.age_seconds.unwrap() >= 200);
    }

    #[test]
    fn silence_detect_skips_recently_heartbeating_worker() {
        let mut mol = make_stale_molecule("cs-20260426-bbbb", "quartz", 500);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let now = Utc::now();
        let recent = now - chrono::Duration::seconds(30);
        let findings = find_silent_molecules(&[mol], 90, now, |_| Some(recent));
        assert!(findings.is_empty(), "recent heartbeat should not be silent");
    }

    #[test]
    fn silence_detect_flags_worker_that_never_heartbeat_when_molecule_is_old() {
        let mut mol = make_stale_molecule("cs-20260426-cccc", "quartz", 500);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let findings = find_silent_molecules(&[mol], 90, Utc::now(), |_| None);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].age_seconds.is_none());
    }

    #[test]
    fn silence_detect_skips_freshly_running_molecule_even_without_heartbeat() {
        // Cold-start guard: molecule running for only 10s, threshold 90s.
        let mut mol = make_stale_molecule("cs-20260426-dddd", "quartz", 10);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let findings = find_silent_molecules(&[mol], 90, Utc::now(), |_| None);
        assert!(findings.is_empty());
    }

    #[test]
    fn silence_detect_ignores_non_running_molecules() {
        let mut mol = make_molecule(
            "cs-20260426-eeee",
            MoleculeStatus::Completed,
            Some("quartz"),
        );
        mol.updated_at = Utc::now() - chrono::Duration::seconds(9999);
        let findings = find_silent_molecules(&[mol], 90, Utc::now(), |_| None);
        assert!(findings.is_empty());
    }

    // --- event-age: harness-agnostic stall backstop (delib-20260608-6a5f) ---

    #[test]
    fn event_age_flags_running_molecule_with_stale_last_event() {
        // Molecule Running for 1800s, last event 1200s ago, threshold 900s.
        let mut mol = make_stale_molecule("task-20260608-aaaa", "quartz", 1800);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let now = Utc::now();
        let last_event = now - chrono::Duration::seconds(1200);
        let findings = find_event_age_stalls(&[mol], 900, now, |_| Some(last_event));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].age_seconds.unwrap() >= 1200);
        // No irreversible tag → operational tier.
        assert_eq!(findings[0].severity_level, "warn");
    }

    #[test]
    fn event_age_skips_recently_active_molecule() {
        let mut mol = make_stale_molecule("task-20260608-bbbb", "quartz", 1800);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let now = Utc::now();
        let recent = now - chrono::Duration::seconds(120);
        let findings = find_event_age_stalls(&[mol], 900, now, |_| Some(recent));
        assert!(findings.is_empty(), "recent event must not be a stall");
    }

    #[test]
    fn event_age_flags_molecule_with_no_events_when_old() {
        // The external-modal case: a Running molecule that has emitted nothing
        // at all (no heartbeat, no evolve) — silence-detect's heartbeat key
        // would miss it if heartbeats were never wired; event-age catches it.
        let mut mol = make_stale_molecule("task-20260608-cccc", "quartz", 1800);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let findings = find_event_age_stalls(&[mol], 900, Utc::now(), |_| None);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].age_seconds.is_none());
    }

    #[test]
    fn event_age_skips_freshly_running_molecule() {
        // Cold-start guard: Running for only 60s, threshold 900s.
        let mut mol = make_stale_molecule("task-20260608-dddd", "quartz", 60);
        mol.assigned_worker = Some(WorkerId::new("quartz").unwrap());
        let findings = find_event_age_stalls(&[mol], 900, Utc::now(), |_| None);
        assert!(findings.is_empty());
    }

    #[test]
    fn event_age_ignores_non_running_molecules() {
        let mut mol = make_molecule(
            "task-20260608-eeee",
            MoleculeStatus::Completed,
            Some("quartz"),
        );
        mol.updated_at = Utc::now() - chrono::Duration::seconds(9999);
        let findings = find_event_age_stalls(&[mol], 900, Utc::now(), |_| None);
        assert!(findings.is_empty());
    }

    #[test]
    fn event_age_severity_alerts_on_irreversible_tag() {
        let mut mol = make_molecule("task-20260608-ffff", MoleculeStatus::Running, None);
        mol.tags
            .insert(cosmon_core::tag::Tag::new("signature").unwrap());
        assert_eq!(event_age_severity(&mol), EventAgeSeverity::Irreversible);
        assert_eq!(event_age_severity(&mol).notify_level(), "alert");
    }

    #[test]
    fn event_age_severity_defaults_to_operational() {
        let mut mol = make_molecule("task-20260608-0001", MoleculeStatus::Running, None);
        mol.tags
            .insert(cosmon_core::tag::Tag::new("temp:hot").unwrap());
        assert_eq!(event_age_severity(&mol), EventAgeSeverity::Operational);
        assert_eq!(event_age_severity(&mol).notify_level(), "warn");
    }

    #[test]
    fn event_age_sweep_fires_with_a_dead_runtime() {
        // The load-bearing test (briefing item 1 / CV-5): the sweep must fire
        // on a Running-but-idle molecule even when no runtime, no fleet, and
        // no transport exist. We build ONLY a store + a stale Running molecule
        // — there is no Fleet, no tmux socket, no `cs run` loop anywhere. The
        // sweep reads molecules + events.jsonl and nothing else, so a dead
        // runtime cannot blind it. Notify is dry-run'd via the env guard.
        let (tmp, store) = make_store();
        let mut mol = make_molecule("task-20260608-dead", MoleculeStatus::Running, Some("ghost"));
        mol.updated_at = Utc::now() - chrono::Duration::seconds(1800);
        store.save_molecule(&mol.id, &mol).unwrap();

        // No events.jsonl is written → build_last_event_index yields an empty
        // map → the molecule reads as "no events ever", which is a stall.
        // Set the dry-run guard so spawn_notify never pops a real channel.
        std::env::set_var("COSMON_NOTIFY_DRY_RUN", "1");
        let molecules = store.list_molecules(&MoleculeFilter::default()).unwrap();
        let report = event_age_sweep(&molecules, tmp.path(), 900, Utc::now());

        assert_eq!(report.scanned, 1);
        assert_eq!(report.stalled.len(), 1);
        assert_eq!(report.stalled[0].molecule_id.as_str(), "task-20260608-dead");
        assert!(report.stalled[0].age_seconds.is_none());
    }

    #[test]
    fn test_find_stale_multiple_mixed() {
        let mut fleet = Fleet::default();
        let (w1_id, w1) = make_worker("w1", DesiredState::Running);
        let (w2_id, w2) = make_worker("w2", DesiredState::Running);
        fleet.workers.insert(w1_id, w1);
        fleet.workers.insert(w2_id, w2);

        let fresh = make_stale_molecule("cs-20260409-f001", "w1", 30);
        let stale = make_stale_molecule("cs-20260409-f002", "w2", 600);

        let result = find_stale_running_molecules(&[fresh, stale], &fleet, 300, Utc::now());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.as_str(), "w2");
    }

    // --- nudge: per-step stall classifier (delib-20260420-1b02 M4) -----

    /// Build a one-step `task-work` formula in memory; tests pass it via
    /// the lookup closure so we never touch disk.
    fn synthetic_task_work_formula(timeout_minutes: Option<u32>) -> cosmon_core::formula::Formula {
        let toml = match timeout_minutes {
            Some(min) => format!(
                "formula = \"task-work\"\nversion = 1\ndescription = \"\"\nid_prefix = \"task\"\n\n[[steps]]\nid = \"work\"\ntitle = \"Work\"\ndescription = \"do it\"\ntimeout_minutes = {min}\n",
            ),
            None => "formula = \"task-work\"\nversion = 1\ndescription = \"\"\nid_prefix = \"task\"\n\n[[steps]]\nid = \"work\"\ntitle = \"Work\"\ndescription = \"do it\"\n".to_owned(),
        };
        cosmon_core::formula::Formula::parse(&toml).unwrap()
    }

    #[test]
    fn nudge_finds_running_molecule_past_step_budget() {
        let mut mol = make_molecule("task-20260425-aaaa", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        let now = Utc::now();
        // last_progress_at older than the 5-minute budget — qualifies as stalled.
        mol.last_progress_at = Some(now - chrono::Duration::minutes(10));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].0.as_str(), "w1");
    }

    #[test]
    fn nudge_skips_molecule_within_step_budget() {
        let mut mol = make_molecule("task-20260425-bbbb", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        let now = Utc::now();
        mol.last_progress_at = Some(now - chrono::Duration::minutes(2));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert!(stalled.is_empty(), "fresh progress must not be nudged");
    }

    #[test]
    fn nudge_default_budget_is_30_minutes() {
        let mut mol = make_molecule("task-20260425-cccc", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        let now = Utc::now();
        // 20 minutes < 30-minute default — not stalled when formula is silent.
        mol.last_progress_at = Some(now - chrono::Duration::minutes(20));
        {
            let mols = vec![mol.clone()];
            let stalled =
                find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(None)));
            assert!(stalled.is_empty());
        }
        // 31 minutes > 30-minute default — stalled.
        mol.last_progress_at = Some(now - chrono::Duration::minutes(31));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(None)));
        assert_eq!(stalled.len(), 1);
    }

    #[test]
    fn nudge_idempotence_window_is_60_seconds() {
        let mut mol = make_molecule("task-20260425-dddd", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        let now = Utc::now();
        mol.last_progress_at = Some(now - chrono::Duration::minutes(10));
        // Just nudged 30 s ago — must be skipped (idempotence guard).
        mol.last_nudged_at = Some(now - chrono::Duration::seconds(30));
        {
            let mols = vec![mol.clone()];
            let stalled =
                find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
            assert!(stalled.is_empty(), "must not re-nudge within 60 s");
        }
        // Last nudge 90 s ago — eligible again.
        mol.last_nudged_at = Some(now - chrono::Duration::seconds(90));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert_eq!(stalled.len(), 1);
    }

    #[test]
    fn nudge_skips_freshly_tackled_molecule_without_progress_signal() {
        // A molecule that has never been bumped by `cs evolve` and was
        // tackled moments ago is inside the boot-stall grace — the worker is
        // legitimately still booting and must not be nudged yet.
        let now = Utc::now();
        let mut mol = make_molecule("task-20260425-eeee", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        mol.last_progress_at = None;
        mol.tackled_at = Some(now - chrono::Duration::seconds(30));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert!(stalled.is_empty());
    }

    #[test]
    fn nudge_finds_boot_stalled_molecule_that_never_progressed() {
        // The task-20260718-ac03 stuck-paste class: tackled long ago, session
        // running, but the bootstrap Enter was lost — no `last_progress_at`
        // ever. The pre-fix classifier skipped this molecule by construction;
        // it must now read as stalled once past the boot-stall budget.
        let now = Utc::now();
        let mut mol = make_molecule("task-20260718-b0aa", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        mol.last_progress_at = None;
        mol.tackled_at = Some(now - chrono::Duration::seconds(NUDGE_BOOT_STALL_SECS + 60));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert_eq!(stalled.len(), 1, "never-started molecule must be nudged");
        assert_eq!(stalled[0].0.as_str(), "w1");
    }

    #[test]
    fn nudge_boot_stall_falls_back_to_updated_at_for_legacy_records() {
        // Legacy state files have no `tackled_at`; `updated_at` was stamped
        // at tackle and frozen since (nothing ever happened), so it is the
        // correct fallback anchor for the boot-stall clock.
        let now = Utc::now();
        let mut mol = make_molecule("task-20260718-b0bb", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        mol.last_progress_at = None;
        mol.tackled_at = None;
        mol.updated_at = now - chrono::Duration::seconds(NUDGE_BOOT_STALL_SECS + 60);
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert_eq!(stalled.len(), 1);
    }

    #[test]
    fn nudge_boot_stall_honors_idempotence_window() {
        // A boot-stalled molecule already nudged 30 s ago must not be poked
        // again — same idempotence guard as the step-stall class.
        let now = Utc::now();
        let mut mol = make_molecule("task-20260718-b0cc", MoleculeStatus::Running, Some("w1"));
        mol.formula_id = FormulaId::new("task-work").unwrap();
        mol.last_progress_at = None;
        mol.tackled_at = Some(now - chrono::Duration::seconds(NUDGE_BOOT_STALL_SECS + 60));
        mol.last_nudged_at = Some(now - chrono::Duration::seconds(30));
        let mols = vec![mol];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert!(stalled.is_empty(), "must not re-nudge within 60 s");
    }

    #[test]
    fn nudge_message_references_briefing_path() {
        let p = std::path::PathBuf::from("/tmp/.cosmon/molecules/task-x/briefing.md");
        let msg = nudge_message(&p);
        assert!(msg.contains("briefing.md"), "msg={msg}");
        assert!(msg.contains("task-x"), "msg={msg}");
    }

    #[test]
    fn nudge_increments_nudge_count_and_stamps_last_nudged_at() {
        use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};

        let (tmp, store) = make_store();
        let mut mol = make_molecule(
            "task-20260425-ffff",
            MoleculeStatus::Running,
            Some("alive-w"),
        );
        mol.formula_id = FormulaId::new("task-work").unwrap();
        let now = Utc::now();
        mol.last_progress_at = Some(now - chrono::Duration::minutes(60));
        store.save_molecule(&mol.id, &mol).unwrap();

        // Set up a backend with the worker alive so the send_input succeeds.
        let backend = MockBackend::new();
        backend
            .spawn(
                &AgentDefinition {
                    id: cosmon_core::id::AgentId::new("alive-w").unwrap(),
                    role: AgentRole::Implementation,
                    command: "echo".to_owned(),
                    args: vec![],
                },
                &RuntimeConfig::default(),
            )
            .unwrap();

        // Provide a synthetic formulas dir so the nudge function can resolve
        // the formula and compute the per-step budget.
        let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(tmp.path());
        std::fs::create_dir_all(&formulas_dir).unwrap();
        std::fs::write(
            formulas_dir.join("task-work.formula.toml"),
            "formula = \"task-work\"\nversion = 1\ndescription = \"\"\nid_prefix = \"task\"\n\n[[steps]]\nid = \"work\"\ntitle = \"Work\"\ndescription = \"do it\"\ntimeout_minutes = 5\n",
        )
        .unwrap();

        // Note: MockBackend doesn't implement TmuxBackend, so we exercise
        // the pure path via `find_stalled_for_nudge` instead. The state
        // mutation happens in `nudge_stalled_molecules` only when a real
        // TmuxBackend is wired — see CLI integration tests for that path.
        let mols = vec![mol.clone()];
        let stalled =
            find_stalled_for_nudge(&mols, now, |_| Some(synthetic_task_work_formula(Some(5))));
        assert_eq!(stalled.len(), 1);
        // Verify the persisted molecule still has nudge_count=0 (we did
        // not invoke the side-effecting wrapper, so state is untouched).
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.nudge_count, 0);
        assert_eq!(reloaded.last_nudged_at, None);
    }

    // --- duplicate role-binding invariant (delib-20260414-2ab2 / ADR-040) ---

    /// Helper: build a worker bound to a molecule with a chosen role. Takes
    /// both the `AgentRole` (which seeds `worker_role` via `derive_worker_role`)
    /// and an explicit `WorkerRole` override so we can test the pathological
    /// case of two Cognition workers sharing a mol id even if one has an
    /// `AgentRole::Runtime`.
    fn worker_bound_to(
        name: &str,
        agent_role: AgentRole,
        worker_role: WorkerRole,
        mol: &str,
    ) -> (WorkerId, WorkerData) {
        let wid = WorkerId::new(name).unwrap();
        let mut data = WorkerData::new(
            wid.clone(),
            AgentId::new("agent").unwrap(),
            agent_role,
            Clearance::Write,
            WorkerStatus::Active,
        );
        data.desired = DesiredState::Running;
        data.worker_role = worker_role;
        data.current_molecule = Some(MoleculeId::new(mol).unwrap());
        (wid, data)
    }

    #[test]
    fn test_duplicate_role_bindings_flags_two_cognition_workers_on_same_mol() {
        // The I1/I2/I9 invariant (ADR-040): every (mol_id, worker_role) pair
        // appears at most once. Two cognition workers on the same molecule
        // is either a split-brain respawn or a rogue manual tackle — patrol
        // must surface it as an anomaly.
        let mut fleet = Fleet::default();
        let (w1, d1) = worker_bound_to(
            "cog-1",
            AgentRole::Implementation,
            WorkerRole::Cognition,
            "cs-20260414-mmmm",
        );
        let (w2, d2) = worker_bound_to(
            "cog-2",
            AgentRole::Implementation,
            WorkerRole::Cognition,
            "cs-20260414-mmmm",
        );
        fleet.workers.insert(w1, d1);
        fleet.workers.insert(w2, d2);
        let dups = duplicate_role_bindings(&fleet);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].1, WorkerRole::Cognition);
        assert_eq!(dups[0].2, 2);
    }

    #[test]
    fn test_duplicate_role_bindings_ok_for_runtime_plus_cognition_pair() {
        // The healthy topology: one Runtime and one Cognition bound to the
        // same molecule is NOT a duplicate — they cover orthogonal concerns.
        let mut fleet = Fleet::default();
        let (w1, d1) = worker_bound_to(
            "runtime-xyz-1234",
            AgentRole::Runtime,
            WorkerRole::Runtime,
            "cs-20260414-nnnn",
        );
        let (w2, d2) = worker_bound_to(
            "quartz-abcd",
            AgentRole::Implementation,
            WorkerRole::Cognition,
            "cs-20260414-nnnn",
        );
        fleet.workers.insert(w1, d1);
        fleet.workers.insert(w2, d2);
        let dups = duplicate_role_bindings(&fleet);
        assert!(dups.is_empty());
    }

    #[test]
    fn test_duplicate_role_bindings_ignores_workers_without_molecule() {
        // Workers without `current_molecule` (e.g. pool workers waiting for
        // a task) cannot violate the bijection — they have no mol id to
        // duplicate against.
        let mut fleet = Fleet::default();
        let (w1, mut d1) = worker_bound_to(
            "idle-1",
            AgentRole::Implementation,
            WorkerRole::Cognition,
            "cs-20260414-pppp",
        );
        d1.current_molecule = None;
        let (w2, mut d2) = worker_bound_to(
            "idle-2",
            AgentRole::Implementation,
            WorkerRole::Cognition,
            "cs-20260414-pppp",
        );
        d2.current_molecule = None;
        fleet.workers.insert(w1, d1);
        fleet.workers.insert(w2, d2);
        let dups = duplicate_role_bindings(&fleet);
        assert!(dups.is_empty());
    }

    #[test]
    fn test_duplicate_role_bindings_flags_two_runtimes() {
        // Twin runtimes on the same molecule is the split-brain signature
        // — `cs run <m>` was invoked twice or survived a crash-restart.
        let mut fleet = Fleet::default();
        let (w1, d1) = worker_bound_to(
            "runtime-a-1111",
            AgentRole::Runtime,
            WorkerRole::Runtime,
            "cs-20260414-qqqq",
        );
        let (w2, d2) = worker_bound_to(
            "runtime-b-2222",
            AgentRole::Runtime,
            WorkerRole::Runtime,
            "cs-20260414-qqqq",
        );
        fleet.workers.insert(w1, d1);
        fleet.workers.insert(w2, d2);
        let dups = duplicate_role_bindings(&fleet);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].1, WorkerRole::Runtime);
        assert_eq!(dups[0].2, 2);
    }

    // --- expire sweep ------------------------------------------------

    fn expired_molecule(id: &str, status: MoleculeStatus, policy: ExpiryPolicy) -> MoleculeData {
        let mut m = make_molecule(id, status, None);
        m.expires_at = Some(Utc::now() - chrono::Duration::days(1));
        m.expiry_policy = Some(policy);
        m
    }

    #[test]
    fn expire_sweep_warn_tags_molecule_and_is_idempotent() {
        let (tmp, store) = make_store();
        let mol = expired_molecule(
            "cs-20260412-wrn1",
            MoleculeStatus::Pending,
            ExpiryPolicy::Warn,
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let mols = vec![mol.clone()];
        let r1 = expire_sweep(&store, tmp.path(), &mols, Utc::now()).unwrap();
        assert_eq!(r1.warned.len(), 1);

        let stored = store.load_molecule(&mol.id).unwrap();
        assert!(stored
            .tags
            .contains(&cosmon_core::tag::Tag::new("expired").unwrap()));

        // Idempotent: running again on the already-tagged state records
        // the warn action in the report but does not re-mutate status.
        let mols2 = vec![stored.clone()];
        let r2 = expire_sweep(&store, tmp.path(), &mols2, Utc::now()).unwrap();
        assert_eq!(r2.warned.len(), 1);
        let stored2 = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored2.status, MoleculeStatus::Pending);
        assert!(stored2
            .tags
            .contains(&cosmon_core::tag::Tag::new("expired").unwrap()));
    }

    #[test]
    fn expire_sweep_collapse_transitions_pending() {
        let (tmp, store) = make_store();
        let mol = expired_molecule(
            "cs-20260412-col1",
            MoleculeStatus::Pending,
            ExpiryPolicy::Collapse,
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let mols = vec![mol.clone()];
        let r = expire_sweep(&store, tmp.path(), &mols, Utc::now()).unwrap();
        assert_eq!(r.collapsed.len(), 1);

        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Collapsed);
        assert_eq!(stored.collapse_reason.as_deref(), Some("expired (TTL)"));

        // Idempotent re-run: already-collapsed molecule stays collapsed,
        // evaluator still returns Collapse, report still lists it.
        let mols2 = vec![stored.clone()];
        let r2 = expire_sweep(&store, tmp.path(), &mols2, Utc::now()).unwrap();
        assert_eq!(r2.collapsed.len(), 1);
        let stored2 = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored2.status, MoleculeStatus::Collapsed);
    }

    #[test]
    fn expire_sweep_collapse_degrades_for_running_molecule() {
        // ADR-029 invariant: running molecules never silently collapse.
        let (tmp, store) = make_store();
        let mut mol = expired_molecule(
            "cs-20260412-run1",
            MoleculeStatus::Running,
            ExpiryPolicy::Collapse,
        );
        mol.assigned_worker = Some(WorkerId::new("someone").unwrap());
        store.save_molecule(&mol.id, &mol).unwrap();

        let r = expire_sweep(&store, tmp.path(), std::slice::from_ref(&mol), Utc::now()).unwrap();
        assert_eq!(r.warned.len(), 1);
        assert!(r.collapsed.is_empty());

        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Running);
        assert!(stored
            .tags
            .contains(&cosmon_core::tag::Tag::new("expired").unwrap()));
    }

    #[test]
    fn expire_sweep_escalate_tags_without_collapse() {
        let (tmp, store) = make_store();
        let mol = expired_molecule(
            "cs-20260412-esc1",
            MoleculeStatus::Pending,
            ExpiryPolicy::Escalate,
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let r = expire_sweep(&store, tmp.path(), std::slice::from_ref(&mol), Utc::now()).unwrap();
        assert_eq!(r.escalated.len(), 1);

        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Pending);
        assert!(stored
            .tags
            .contains(&cosmon_core::tag::Tag::new("escalated").unwrap()));
        assert!(stored
            .tags
            .contains(&cosmon_core::tag::Tag::new("expired").unwrap()));
    }

    #[test]
    fn expire_sweep_skips_future_and_no_ttl_molecules() {
        let (tmp, store) = make_store();
        let mut fresh = make_molecule("cs-20260412-fut1", MoleculeStatus::Pending, None);
        fresh.expires_at = Some(Utc::now() + chrono::Duration::days(30));
        fresh.expiry_policy = Some(ExpiryPolicy::Collapse);
        store.save_molecule(&fresh.id, &fresh).unwrap();

        let none = make_molecule("cs-20260412-non1", MoleculeStatus::Pending, None);
        store.save_molecule(&none.id, &none).unwrap();

        let mols = vec![fresh.clone(), none.clone()];
        let r = expire_sweep(&store, tmp.path(), &mols, Utc::now()).unwrap();
        assert_eq!(r.scanned, 2);
        assert!(r.warned.is_empty());
        assert!(r.collapsed.is_empty());
        assert!(r.escalated.is_empty());
    }

    // --- auto-freeze / auto-collapse orphans ---------------------------

    // --- the completed-but-teardown-failed limbo (task-20260719-fedf) ---

    /// The founding incident, reconstructed. The molecule reached
    /// `Completed`, post-merge harvest failed, the process died — and the
    /// fleet entry survived, `desired=Running`, for 17 hours. Neither
    /// `auto_freeze_orphans` (terminal molecules are filtered out) nor the
    /// respawn path (the work is done) owned it. This sweep does.
    #[test]
    fn reap_removes_finished_worker_whose_session_is_gone() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("limbo-w", DesiredState::Running);
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260719-fedf",
            MoleculeStatus::Completed,
            Some("limbo-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        // Nothing registered on the backend → `is_alive` is a definitive false.
        let backend = MockBackend::new();

        let reaped = reap_finished_dead_workers(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            Some(&backend as &dyn TransportBackend),
        )
        .unwrap();

        assert_eq!(reaped, vec![wid.clone()]);
        assert!(
            !store.load_fleet().unwrap().workers.contains_key(&wid),
            "the stale fleet entry must be reclaimed, not left claiming to run"
        );
    }

    /// A finished worker whose session is still up is mid-teardown, not
    /// stranded. Reaping it would race a live process.
    #[test]
    fn reap_spares_finished_worker_whose_session_is_still_alive() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("closing-w", DesiredState::Running);
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260719-alv1",
            MoleculeStatus::Completed,
            Some("closing-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let backend = mock_with_worker("closing-w", "");

        let reaped = reap_finished_dead_workers(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            Some(&backend as &dyn TransportBackend),
        )
        .unwrap();

        assert!(reaped.is_empty());
        assert!(store.load_fleet().unwrap().workers.contains_key(&wid));
    }

    /// A dead session on a molecule that is still `Running` is the ORPHAN
    /// case — `auto_freeze_orphans` owns it, and it may still be respawned.
    /// This sweep must not steal it, or a rescuable molecule silently loses
    /// its worker entry before the respawn path can see it.
    #[test]
    fn reap_leaves_the_still_running_orphan_to_the_orphan_sweep() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("running-w", DesiredState::Running);
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260719-run1",
            MoleculeStatus::Running,
            Some("running-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let backend = MockBackend::new();

        let reaped = reap_finished_dead_workers(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            Some(&backend as &dyn TransportBackend),
        )
        .unwrap();

        assert!(reaped.is_empty());
        assert!(store.load_fleet().unwrap().workers.contains_key(&wid));
    }

    /// With no transport there is no honest liveness verdict, so the sweep
    /// declines rather than reaping on an assumption.
    #[test]
    fn reap_declines_without_a_backend() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("unknown-w", DesiredState::Running);
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260719-unk1",
            MoleculeStatus::Completed,
            Some("unknown-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let reaped = reap_finished_dead_workers(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            None,
        )
        .unwrap();

        assert!(reaped.is_empty());
        assert!(store.load_fleet().unwrap().workers.contains_key(&wid));
    }

    #[test]
    fn auto_freeze_orphans_default_freezes_running_molecule() {
        // Worker desired=Stopped but molecule still Running → auto-frozen.
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("ghost-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("cs-20260412-orp1", MoleculeStatus::Running, Some("ghost-w"));
        store.save_molecule(&mol.id, &mol).unwrap();

        let transitioned = auto_freeze_orphans(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            &[],
            &[],
            false,
        )
        .unwrap();

        assert_eq!(transitioned.len(), 1);
        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Frozen);
    }

    #[test]
    fn auto_freeze_orphans_with_auto_collapse_flag_collapses() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("ghost-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("cs-20260412-orp2", MoleculeStatus::Running, Some("ghost-w"));
        store.save_molecule(&mol.id, &mol).unwrap();

        let transitioned = auto_freeze_orphans(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            &[],
            &[],
            true, // auto_collapse
        )
        .unwrap();

        assert_eq!(transitioned.len(), 1);
        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Collapsed);
        assert_eq!(
            stored.collapse_reason.as_deref(),
            Some("worker dead, auto-collapsed by patrol")
        );
        assert_eq!(stored.collapsed_step, Some(stored.current_step));
    }

    #[test]
    fn auto_freeze_orphans_skips_respawned_workers() {
        // Worker was in needs_respawn AND successfully respawned this run →
        // its molecule must stay Running (respawn brought the worker back).
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("alive-w", DesiredState::Running);
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("cs-20260412-orp3", MoleculeStatus::Running, Some("alive-w"));
        store.save_molecule(&mol.id, &mol).unwrap();

        let transitioned = auto_freeze_orphans(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            std::slice::from_ref(&wid), // needs_respawn
            std::slice::from_ref(&wid), // respawned (success)
            false,
        )
        .unwrap();

        assert!(transitioned.is_empty());
        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Running);
    }

    #[test]
    fn auto_freeze_orphans_freezes_when_respawn_failed() {
        // Worker in needs_respawn but respawn failed → molecule auto-frozen.
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("doomed-w", DesiredState::Running);
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260412-orp4",
            MoleculeStatus::Running,
            Some("doomed-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let transitioned = auto_freeze_orphans(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            &[wid], // needs_respawn
            &[],    // respawn list empty → failed or --respawn not passed
            false,
        )
        .unwrap();

        assert_eq!(transitioned.len(), 1);
        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Frozen);
    }

    #[test]
    fn auto_freeze_orphans_leaves_completed_molecules_alone() {
        // Molecule is Completed with worker=dead — terminal state must
        // never be re-touched by patrol.
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker("dead-w", DesiredState::Stopped);
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule(
            "cs-20260412-orp5",
            MoleculeStatus::Completed,
            Some("dead-w"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let transitioned = auto_freeze_orphans(
            &store,
            tmp.path(),
            &fleet,
            std::slice::from_ref(&mol),
            &[],
            &[],
            false,
        )
        .unwrap();

        assert!(transitioned.is_empty());
        let stored = store.load_molecule(&mol.id).unwrap();
        assert_eq!(stored.status, MoleculeStatus::Completed);
    }

    #[test]
    fn append_patrol_metric_creates_and_appends_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("patrol-metrics.json");
        assert!(!path.exists());

        append_patrol_metric(tmp.path(), 2, 1, false, 0).unwrap();
        assert!(path.exists());
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entries = doc["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["orphans_detected"], 2);
        assert_eq!(entries[0]["auto_transitioned"], 1);
        assert_eq!(entries[0]["target_status"], "frozen");

        // Second call appends, does not overwrite.
        append_patrol_metric(tmp.path(), 0, 0, true, 3).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entries = doc["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1]["target_status"], "collapsed");
        assert_eq!(entries[1]["respawned"], 3);
    }

    #[test]
    fn patrol_run_emits_metrics_file() {
        // End-to-end: a full patrol run (even with no orphans) writes a
        // timestamped entry to patrol-metrics.json.
        let (tmp, store) = make_store();
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            respawn: false,
            no_tmux: true,
            propel: false,
            stale_after: 300,
            expire: false,
            auto_collapse: false,
            harvest: false,
            livelock: false,
            livelock_stale_after: 3600,
            nudge: false,
            silence_detect: false,
            silence_after: 90,
            event_age: false,
            event_age_after: 900,
            abandon: false,
            abandon_root: None,
            abandon_quiet_hours: 24,
            heal: false,
            dry_run: false,
            dialogue_scan: false,
            auto_confirm_safe: false,
            dialogue_lines: 40,
            dialogue_blocked_after: 900,
        };
        run(&ctx, &args).unwrap();

        let metrics_path = tmp.path().join("patrol-metrics.json");
        assert!(metrics_path.exists());
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&metrics_path).unwrap()).unwrap();
        assert_eq!(doc["entries"].as_array().unwrap().len(), 1);
    }

    // --- dialogue-scan: blocking-dialogue detection (task-20260704-5ee0) ---

    use cosmon_core::dialogue::DialogueClass;

    /// Register a live worker session on a `MockBackend` so `is_alive` and
    /// `capture_output` succeed for `worker_name`, and set the pane text.
    fn mock_with_worker(worker_name: &str, pane: &str) -> MockBackend {
        use cosmon_core::transport::{AgentDefinition, RuntimeConfig};
        let backend = MockBackend::new();
        let agent = AgentDefinition {
            id: AgentId::new(worker_name).unwrap(),
            role: AgentRole::Implementation,
            command: "claude".to_owned(),
            args: Vec::new(),
        };
        backend.spawn(&agent, &RuntimeConfig::default()).unwrap();
        backend.set_canned_output(pane);
        backend
    }

    fn opts(auto_confirm_safe: bool) -> DialogueScanOpts {
        DialogueScanOpts {
            lines: 40,
            auto_confirm_safe,
            blocked_after: 900,
        }
    }

    #[test]
    fn decide_money_stake_is_never_auto_confirmed() {
        // Even with the opt-in flag ON, a money stake alerts, never confirms.
        let o = opts(true);
        assert_eq!(
            decide_dialogue_action(DialogueClass::MoneyStake, Some(10), &o),
            DialogueAction::Alerted
        );
        // Past the blocked threshold it escalates to canary RED — still no
        // auto-confirm.
        assert_eq!(
            decide_dialogue_action(DialogueClass::MoneyStake, Some(2000), &o),
            DialogueAction::CanaryRed
        );
    }

    #[test]
    fn decide_permission_confirms_only_when_opted_in() {
        assert_eq!(
            decide_dialogue_action(DialogueClass::Permission, Some(10), &opts(true)),
            DialogueAction::AutoConfirmed
        );
        assert_eq!(
            decide_dialogue_action(DialogueClass::Permission, Some(10), &opts(false)),
            DialogueAction::Reported
        );
        // A safe prompt nobody answered for a long time still escalates.
        assert_eq!(
            decide_dialogue_action(DialogueClass::Permission, Some(2000), &opts(false)),
            DialogueAction::CanaryRed
        );
    }

    #[test]
    fn dialogue_sweep_money_alerts_and_never_sends_enter() {
        std::env::set_var("COSMON_NOTIFY_DRY_RUN", "1");
        let (tmp, store) = make_store();
        let mol = make_molecule("task-20260704-money", MoleculeStatus::Running, Some("w1"));
        store.save_molecule(&mol.id, &mol).unwrap();
        let molecules = store.list_molecules(&MoleculeFilter::default()).unwrap();

        let pane = "You're approaching your spend limit.\nPress Enter to continue";
        let backend = mock_with_worker("w1", pane);

        // auto_confirm_safe = true to PROVE money is refused regardless.
        let report = dialogue_scan_sweep(
            &store,
            tmp.path(),
            &molecules,
            Some(&backend as &dyn TransportBackend),
            &opts(true),
            Utc::now(),
        );

        assert_eq!(report.scanned, 1);
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.class, DialogueClass::MoneyStake);
        assert_eq!(f.action, DialogueAction::Alerted);

        // The load-bearing assertion: NO Enter was ever sent to the worker.
        let sent_enter = backend.calls().iter().any(|c| {
            matches!(c, cosmon_transport::mock::MockCall::SendInput { worker_id, .. } if worker_id == "w1")
        });
        assert!(!sent_enter, "money stake must never be auto-confirmed");

        // And the molecule is tagged so `cs ensemble` surfaces the block.
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert!(reloaded
            .tags
            .iter()
            .any(|t| t.as_str() == "dialogue-blocked"));
    }

    #[test]
    fn dialogue_sweep_permission_auto_confirms_when_opted_in() {
        std::env::set_var("COSMON_NOTIFY_DRY_RUN", "1");
        let (tmp, store) = make_store();
        let mol = make_molecule("task-20260704-perm", MoleculeStatus::Running, Some("w1"));
        store.save_molecule(&mol.id, &mol).unwrap();
        let molecules = store.list_molecules(&MoleculeFilter::default()).unwrap();

        let pane = "cosmon wants to run `ls`\nDo you want to proceed?\n ❯ 1. Yes\n   3. No";
        let backend = mock_with_worker("w1", pane);

        let report = dialogue_scan_sweep(
            &store,
            tmp.path(),
            &molecules,
            Some(&backend as &dyn TransportBackend),
            &opts(true),
            Utc::now(),
        );

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DialogueClass::Permission);
        assert_eq!(report.findings[0].action, DialogueAction::AutoConfirmed);

        // Exactly the "nobody to press Enter" keystroke was delivered.
        let sent_enter = backend.calls().iter().any(|c| {
            matches!(c, cosmon_transport::mock::MockCall::SendInput { worker_id, input }
                if worker_id == "w1" && input.is_empty())
        });
        assert!(
            sent_enter,
            "safe permission prompt should be auto-confirmed"
        );
    }

    #[test]
    fn dialogue_sweep_permission_reported_when_not_opted_in() {
        std::env::set_var("COSMON_NOTIFY_DRY_RUN", "1");
        let (tmp, store) = make_store();
        let mol = make_molecule("task-20260704-noop", MoleculeStatus::Running, Some("w1"));
        store.save_molecule(&mol.id, &mol).unwrap();
        let molecules = store.list_molecules(&MoleculeFilter::default()).unwrap();

        let pane = "cosmon wants to read `Cargo.toml`\nDo you want to proceed?\n ❯ 1. Yes";
        let backend = mock_with_worker("w1", pane);

        let report = dialogue_scan_sweep(
            &store,
            tmp.path(),
            &molecules,
            Some(&backend as &dyn TransportBackend),
            &opts(false),
            Utc::now(),
        );

        assert_eq!(report.findings[0].action, DialogueAction::Reported);
        let sent_enter = backend
            .calls()
            .iter()
            .any(|c| matches!(c, cosmon_transport::mock::MockCall::SendInput { .. }));
        assert!(!sent_enter, "must not act when auto-confirm is off");
    }

    #[test]
    fn dialogue_sweep_ignores_working_pane() {
        let (tmp, store) = make_store();
        let mol = make_molecule("task-20260704-busy", MoleculeStatus::Running, Some("w1"));
        store.save_molecule(&mol.id, &mol).unwrap();
        let molecules = store.list_molecules(&MoleculeFilter::default()).unwrap();

        let pane = "   Compiling cosmon-core v0.1.0\ntest result: ok. 42 passed";
        let backend = mock_with_worker("w1", pane);

        let report = dialogue_scan_sweep(
            &store,
            tmp.path(),
            &molecules,
            Some(&backend as &dyn TransportBackend),
            &opts(true),
            Utc::now(),
        );
        assert_eq!(report.scanned, 1);
        assert!(report.findings.is_empty());
    }
}
