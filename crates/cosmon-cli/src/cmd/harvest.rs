// SPDX-License-Identifier: AGPL-3.0-only

//! `cs harvest` — **deprecated** alias for `cs done --if-completed` (ADR-052 §D3).
//!
//! A molecule reaches `Completed`, the worker's tmux pane dies, and… no one
//! listens. `cs done` is not-the-worker (see
//! `docs/architectural-invariants.md`), so the worker cannot invoke it from
//! inside its own worktree. A transport watchdog must. `cs harvest` was that
//! watchdog, spelled as a CLI verb so any caller — a tmux `pane-died` hook,
//! a periodic `cs patrol --harvest` sweep, or a human running it manually —
//! could close the loop without duplicating merge logic.
//!
//! ADR-052 collapses the verb: `cs done` grows a `--if-completed` flag that
//! provides the same "silent no-op when not-Completed" semantics. This
//! module is kept as a thin wrapper for one release cycle so pre-existing
//! tmux hooks and scripts keep working; it emits a stderr deprecation
//! notice on every invocation. Internal callers (patrol sweeps) keep using
//! [`harvest_one`] directly to avoid process re-exec overhead.
//!
//! # Semantics
//!
//! For a given molecule, `cs harvest` inspects state and takes one of three
//! paths:
//!
//! 1. **Status ≠ `Completed`** → no-op. The molecule is still in flight or
//!    has collapsed — neither case is ours to close.
//! 2. **Status = `Completed` AND (`merged_at = Some` OR `archived = true`)** →
//!    no-op. Already harvested. Idempotent by design: multiple hooks firing
//!    (pane-died + patrol sweep) must not race. The `archived` disjunct closes
//!    the `no_branch` molecule's loop (delib / drainage worker / empty-branch
//!    task): such a molecule never stamps `merged_at` — there is no branch to
//!    land — yet `cs done` flips `archived = true`. Keying the no-op only on
//!    `merged_at` would re-exec `cs done` on every sweep forever
//!    (task-20260626-eb65).
//! 3. **Status = `Completed` AND `merged_at = None` AND `archived = false`** →
//!    exec `cs done` (merge the branch when present, teardown tmux/worktree,
//!    rewrite the frontier projection, archive).
//!
//! Emits a `Harvested` event for every non-trivial path so the audit trail
//! survives — a future reviewer can answer *who* closed the loop.

use std::path::PathBuf;

use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::process::project_process_status;
use cosmon_core::run_state::{BranchState, Liveness, Witness};
use cosmon_state::{event_log, StateStore};

use super::Context;

/// Arguments for the `harvest` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule to harvest. Required — `cs harvest` operates on one molecule
    /// per invocation. Use `cs patrol --harvest` for the sweep variant.
    #[arg(long)]
    pub molecule: String,

    /// Print what `cs harvest` would do without exec'ing `cs done`.
    #[arg(long)]
    pub dry_run: bool,

    /// Flag the invocation as caused by a tmux `pane-died` hook.
    ///
    /// ADR-052 child #4 (I4 + I8 + I10): the probe must emit its
    /// observation before acting on it. When set, `cs harvest` first
    /// appends a [`EventV2::WorkerExited`] event to `events.jsonl`
    /// (reason = `pane_died`) and *then* runs the normal harvest logic.
    /// Absent this flag, no `WorkerExited` is emitted — periodic
    /// `cs patrol --harvest` sweeps observe via witness, not via the
    /// kernel-level pane-died channel.
    ///
    /// [`EventV2::WorkerExited`]: cosmon_core::event_v2::EventV2::WorkerExited
    #[arg(long)]
    pub from_pane_died: bool,

    /// Exit code reported by tmux `#{pane_dead_status}` when the pane died.
    ///
    /// Only meaningful together with `--from-pane-died`. Accepts any signed
    /// 32-bit integer so the wait-status (which may encode signals) survives
    /// round-trip. Strings that fail to parse are treated as "no information"
    /// (`None` in the emitted event).
    #[arg(long)]
    pub exit_code: Option<String>,
}

/// Outcome of a `cs harvest` invocation.
///
/// Named so the audit event, the JSON output, and the human-readable line
/// all speak the same vocabulary.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarvestOutcome {
    /// Status != Completed — not ours to close.
    NotCompleted,
    /// Already merged — `merged_at` is `Some`.
    AlreadyMerged,
    /// Would exec `cs done` but `--dry-run` was passed.
    DryRun,
    /// `cs done` was invoked and returned success.
    Harvested,
    /// `cs done` was invoked and returned non-zero.
    HarvestFailed,
}

impl HarvestOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotCompleted => "not_completed",
            Self::AlreadyMerged => "already_merged",
            Self::DryRun => "dry_run",
            Self::Harvested => "harvested",
            Self::HarvestFailed => "harvest_failed",
        }
    }
}

/// Execute the deprecated `harvest` command.
///
/// Emits a stderr deprecation notice, then runs the same logic as before.
/// Callers should migrate to `cs done --if-completed <mol>`, which carries
/// byte-identical semantics (ADR-052 §D3). The alias will be removed after
/// one release cycle.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    eprintln!(
        "cs harvest: deprecated — use `cs done --if-completed {}` instead (ADR-052 §D3). \
         This alias will be removed after one release cycle.",
        args.molecule
    );

    let mol_id = MoleculeId::new(&args.molecule)?;
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    if args.from_pane_died {
        record_pane_died(
            store.as_ref(),
            &state_dir,
            &mol_id,
            args.exit_code.as_deref(),
        );
    }

    let outcome = harvest_one(store.as_ref(), &state_dir, &mol_id, args.dry_run)?;

    if ctx.json {
        let payload = serde_json::json!({
            "molecule": mol_id.as_str(),
            "outcome": outcome.as_str(),
        });
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        println!("harvest {mol_id}: {}", outcome.as_str());
    }

    // Exit non-zero only on an explicit harvest_failed — every other
    // outcome is a legitimate no-op path.
    if matches!(outcome, HarvestOutcome::HarvestFailed) {
        return Err(anyhow::anyhow!(
            "cs done failed for {mol_id} — see event log"
        ));
    }
    Ok(())
}

/// Probe-side post-mortem for a tmux `pane-died` event. Three
/// best-effort effects, in I8 order
/// (observe → record → project):
///
/// 1. Persist the grand-child's exit trail to `<mol_dir>/worker.exit`
///    so a later forensic pass (C3) and `cs observe` can read the exit
///    code without re-running tmux. The code is the tmux
///    `#{pane_dead_status}` expansion the hook captured. This closes the
///    *"worker.stderr/exit PAS persistés"* gap diagnosed on 2026-06-12.
/// 2. Append the [`EventV2::WorkerExited`] ledger row (the existing
///    event-driven liveness witness).
/// 3. Project the hard-death witness onto `MoleculeProcess.status` via
///    the two-coup scale
///    ([`project_process_status`]):
///    a kernel-observed `Dead` condemns to
///    [`WorkerStatus::Stale`](cosmon_core::worker::WorkerStatus::Stale) in
///    one coup. This is the **missing writer** behind the "status stays
///    `active` ad vitam" bug — the watchdog now updates the state it
///    watches, co-located with the tenant `StateStore`.
///
/// Writer discipline (I2 — `SingleWriterPerField`): this runs from the
/// `pane-died` hook, which tmux fires as a **sibling** of the dead
/// worker (it inherits tmux's env, not the worker's worktree cwd). The
/// worker never writes its own liveness — the probe does.
///
/// All effects are best-effort: a full disk or a missing molecule must
/// never wedge the follow-on harvest / `cs done`.
pub(crate) fn record_pane_died(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    raw_exit_code: Option<&str>,
) {
    write_worker_exit(store, mol_id, raw_exit_code);
    emit_worker_exited(state_dir, mol_id, raw_exit_code);
    project_dead_onto_process_status(store, mol_id);
}

/// Persist the dead grand-child's exit code to `<mol_dir>/worker.exit`.
///
/// Writes the parsed integer when tmux expanded `#{pane_dead_status}`;
/// otherwise the sentinel `"unknown"` (so a forensic reader can tell
/// "tmux did not expand the format" apart from a genuine `exit 0`).
/// Best-effort — a write failure is swallowed.
fn write_worker_exit(store: &dyn StateStore, mol_id: &MoleculeId, raw_exit_code: Option<&str>) {
    let dir = store.molecule_dir(mol_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let body = raw_exit_code
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map_or_else(|| "unknown".to_owned(), |c| c.to_string());
    let _ = std::fs::write(dir.join("worker.exit"), format!("{body}\n"));
}

/// Project a hard-death [`Liveness::Dead`] witness onto the molecule's
/// [`MoleculeProcess::status`](cosmon_core::process::MoleculeProcess::status).
///
/// Loads the molecule, escalates `process.status` through the two-coup
/// scale (a hard `Dead` → `Stale` in one coup), and persists only when
/// the status actually changed. A molecule with no live `process` record
/// (legacy / already torn down) is a silent no-op. Best-effort.
fn project_dead_onto_process_status(store: &dyn StateStore, mol_id: &MoleculeId) {
    let Ok(mut mol) = store.load_molecule(mol_id) else {
        return;
    };
    let Some(process) = mol.process.as_mut() else {
        return;
    };
    let witness = Witness::new(Liveness::Dead, BranchState::Unmerged);
    let projected = project_process_status(
        &process.status,
        &witness,
        chrono::Utc::now(),
        // TTL is irrelevant for a hard Dead, but the scale requires one.
        std::time::Duration::from_secs(300),
    );
    if process.status == projected {
        return;
    }
    process.status = projected;
    let _ = store.save_molecule(mol_id, &mol);
}

/// Append a [`EventV2::WorkerExited`] event to the state's
/// `events.jsonl`, best-effort.
///
/// Called by the `--from-pane-died` path so the kernel-level witness
/// that the worker process has exited lands in the ledger *before*
/// anything else (harvest, `cs done`, surface projection) observes the
/// post-mortem state. ADR-052 invariants I4 + I8 + I10 — the probe
/// must emit before it acts.
///
/// `raw_exit_code` is the tmux `#{pane_dead_status}` expansion as
/// passed on the CLI. It may be:
///
/// * a signed integer — recorded verbatim on the event;
/// * an empty string, `None`, or the literal `#{pane_dead_status}`
///   format (tmux did not expand it) — recorded as `exit_code = None`.
///
/// Failures to write are swallowed: the pane-died hook must never
/// short-circuit follow-on cleanup because the disk was full.
pub(crate) fn emit_worker_exited(
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    raw_exit_code: Option<&str>,
) {
    let events_path = state_dir.join("events.jsonl");
    let exit_code = raw_exit_code.and_then(|s| s.trim().parse::<i32>().ok());
    let _ = event_log::emit_one(
        &events_path,
        EventV2::WorkerExited {
            molecule_id: mol_id.clone(),
            exit_code,
            reason: "pane_died".to_owned(),
        },
        None,
    );
}

/// Core harvest logic, separated for testing.
///
/// Returns the outcome; writes an event to `events.jsonl` when non-trivial.
pub(crate) fn harvest_one(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    dry_run: bool,
) -> anyhow::Result<HarvestOutcome> {
    let mol = store.load_molecule(mol_id)?;

    if mol.status != MoleculeStatus::Completed {
        return Ok(HarvestOutcome::NotCompleted);
    }
    // Already-terminal no-op. `merged_at` covers the merged-branch path;
    // `archived` covers the `no_branch` path (delib / drainage / empty-branch
    // task), which `cs done` archives WITHOUT ever stamping `merged_at`. Keying
    // only on `merged_at` re-execs `cs done` on every patrol sweep forever for
    // a no_branch molecule — the loop this disjunct closes (task-20260626-eb65).
    if mol.merged_at.is_some() || mol.archived {
        return Ok(HarvestOutcome::AlreadyMerged);
    }
    if dry_run {
        return Ok(HarvestOutcome::DryRun);
    }

    // Exec `cs done` via the running binary path when available, falling
    // back to `cs` on PATH. The walk-up state discovery done by `cs done`
    // picks up the repo from CWD — callers (tmux hook, patrol) are
    // responsible for chdir-ing into the main repo root first.
    let cs_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cs"));
    let mut cmd = std::process::Command::new(&cs_bin);
    cmd.arg("done").arg(mol_id.as_str());
    let status = cmd.status();

    let events_path = state_dir.join("events.jsonl");
    let success = status.as_ref().is_ok_and(std::process::ExitStatus::success);

    let _ = event_log::emit_one(
        &events_path,
        EventV2::Harvested {
            molecule_id: mol_id.clone(),
            success,
        },
        None,
    );

    if success {
        Ok(HarvestOutcome::Harvested)
    } else {
        Ok(HarvestOutcome::HarvestFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::MoleculeData;
    use std::collections::{BTreeSet, HashMap};

    fn sample_mol(id: &MoleculeId, status: MoleculeStatus, merged: bool) -> MoleculeData {
        let now = Utc::now();
        MoleculeData {
            id: id.clone(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 2,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: vec![],
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: if merged { Some(now) } else { None },
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

    fn setup(
        status: MoleculeStatus,
        merged: bool,
    ) -> (tempfile::TempDir, FileStore, MoleculeId, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let store = FileStore::new(&state_dir);
        let mid = MoleculeId::new("task-20260418-hrv1").unwrap();
        let mol = sample_mol(&mid, status, merged);
        store.save_molecule(&mid, &mol).unwrap();
        (tmp, store, mid, state_dir)
    }

    #[test]
    fn harvest_skips_non_completed() {
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        let r = harvest_one(&store, &state_dir, &mid, false).unwrap();
        assert!(matches!(r, HarvestOutcome::NotCompleted));
    }

    #[test]
    fn harvest_skips_already_merged() {
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Completed, true);
        let r = harvest_one(&store, &state_dir, &mid, false).unwrap();
        assert!(matches!(r, HarvestOutcome::AlreadyMerged));
    }

    /// A `no_branch` molecule never stamps `merged_at` — `cs done` archives it
    /// instead. Harvest must treat `archived == true` as already-harvested and
    /// NOT re-exec `cs done`, otherwise every patrol sweep re-runs a full
    /// teardown forever (task-20260626-eb65). `merged = false` here, `archived`
    /// set by hand, is exactly the no_branch shape.
    #[test]
    fn harvest_skips_already_archived_no_branch() {
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Completed, false);
        let mut mol = store.load_molecule(&mid).unwrap();
        mol.archived = true; // harvested via the no_branch path; merged_at stays None
        store.save_molecule(&mid, &mol).unwrap();
        let r = harvest_one(&store, &state_dir, &mid, false).unwrap();
        assert!(
            matches!(r, HarvestOutcome::AlreadyMerged),
            "an archived no_branch molecule must be a harvest no-op, not a re-exec"
        );
    }

    #[test]
    fn emit_worker_exited_appends_event_with_parsed_exit_code() {
        use cosmon_state::event_log::read_all;

        let (_tmp, _store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        emit_worker_exited(&state_dir, &mid, Some("139"));

        let envelopes = read_all(state_dir.join("events.jsonl")).unwrap();
        let found = envelopes
            .iter()
            .find_map(|env| match &env.event {
                EventV2::WorkerExited {
                    molecule_id,
                    exit_code,
                    reason,
                } => Some((molecule_id.clone(), *exit_code, reason.clone())),
                _ => None,
            })
            .expect("worker_exited event written");
        assert_eq!(found.0, mid);
        assert_eq!(found.1, Some(139));
        assert_eq!(found.2, "pane_died");
    }

    #[test]
    fn emit_worker_exited_treats_unparseable_as_no_exit_code() {
        use cosmon_state::event_log::read_all;

        let (_tmp, _store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        // tmux can fail to expand the format (old server, unusual pane),
        // leaving the literal placeholder in argv.
        emit_worker_exited(&state_dir, &mid, Some("#{pane_dead_status}"));
        emit_worker_exited(&state_dir, &mid, Some(""));
        emit_worker_exited(&state_dir, &mid, None);

        let envelopes = read_all(state_dir.join("events.jsonl")).unwrap();
        let worker_exited: Vec<_> = envelopes
            .iter()
            .filter_map(|env| match &env.event {
                EventV2::WorkerExited { exit_code, .. } => Some(*exit_code),
                _ => None,
            })
            .collect();
        assert_eq!(worker_exited, vec![None, None, None]);
    }

    #[test]
    fn harvest_dry_run_reports_what_would_happen() {
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Completed, false);
        let r = harvest_one(&store, &state_dir, &mid, true).unwrap();
        assert!(matches!(r, HarvestOutcome::DryRun));
    }

    #[test]
    fn record_pane_died_writes_exit_file_and_condemns_process_to_stale() {
        use cosmon_core::process::MoleculeProcess;
        use cosmon_core::worker::WorkerStatus;

        // A Running molecule with a live (Active) process record — the
        // shape after `cs tackle`.
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        let mut mol = store.load_molecule(&mid).unwrap();
        mol.process = Some(MoleculeProcess::new(
            cosmon_core::id::WorkerId::new("worker-stale-test").unwrap(),
            "task-20260418-hrv1",
        ));
        assert_eq!(
            mol.process.as_ref().unwrap().status,
            WorkerStatus::Active,
            "process starts Active"
        );
        store.save_molecule(&mid, &mol).unwrap();

        // The kill -9 path: tmux reports a non-zero wait-status.
        record_pane_died(&store, &state_dir, &mid, Some("137"));

        // (1) worker.exit was written with the non-zero code.
        let exit_path = store.molecule_dir(&mid).join("worker.exit");
        let body = std::fs::read_to_string(&exit_path).expect("worker.exit written");
        assert_eq!(body.trim(), "137", "worker.exit must carry the exit code");
        assert_ne!(body.trim(), "0", "kill -9 must record a non-zero exit");

        // (3) process.status was projected to Stale (Dead → Stale, one coup).
        let reloaded = store.load_molecule(&mid).unwrap();
        assert_eq!(
            reloaded.process.as_ref().unwrap().status,
            WorkerStatus::Stale,
            "a hard pane-death must transition MoleculeProcess.status to Stale"
        );
    }

    #[test]
    fn record_pane_died_writes_unknown_when_tmux_did_not_expand() {
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        record_pane_died(&store, &state_dir, &mid, Some("#{pane_dead_status}"));
        let body = std::fs::read_to_string(store.molecule_dir(&mid).join("worker.exit")).unwrap();
        assert_eq!(body.trim(), "unknown");
    }

    #[test]
    fn record_pane_died_no_process_is_silent_noop_on_status() {
        // Legacy molecule with no live process record: exit file is still
        // written, but there is no status to project — must not panic.
        let (_tmp, store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        record_pane_died(&store, &state_dir, &mid, Some("1"));
        assert!(store.molecule_dir(&mid).join("worker.exit").exists());
        assert!(store.load_molecule(&mid).unwrap().process.is_none());
    }
}
