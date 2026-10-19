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
    /// Expected launch supplied by the supervised pane hook; stale triggers are refused.
    #[arg(long, hide = true)]
    pub worker_launch: Option<String>,
    /// Exact terminal supplied by new unsupervised Exec hooks; never reconstructed from a name.
    #[arg(long, hide = true)]
    pub terminal_identity: Option<String>,
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
    /// A supervised launch has no validated terminal evidence; preserve all work.
    ReceiptUnknown,
}

impl HarvestOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotCompleted => "not_completed",
            Self::AlreadyMerged => "already_merged",
            Self::DryRun => "dry_run",
            Self::Harvested => "harvested",
            Self::HarvestFailed => "harvest_failed",
            Self::ReceiptUnknown => "receipt_unknown",
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

    let expected = args
        .worker_launch
        .as_deref()
        .map(serde_json::from_str::<cosmon_core::worker_exit::LaunchIdentity>)
        .transpose()?;

    if args.from_pane_died {
        let terminal = args
            .terminal_identity
            .as_deref()
            .map(serde_json::from_str::<cosmon_core::terminal::TerminalIdentity>)
            .transpose()?;
        record_pane_died_bound(
            store.as_ref(),
            &state_dir,
            &mol_id,
            args.exit_code.as_deref(),
            expected.as_ref(),
            terminal.as_ref(),
        )?;
        #[cfg(unix)]
        let legacy = supervised_observation(store.as_ref(), &mol_id)?.is_none();
        #[cfg(not(unix))]
        let legacy = true;
        if legacy {
            record_update_from_dead_pane(ctx, store.as_ref(), &mol_id)?;
        }
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

/// Inspect retained scrollback after the process exit and keep any observed
/// update or restart notice on the molecule. A missing session is an unknown
/// observation, not evidence of a different exit cause.
fn record_update_from_dead_pane(
    ctx: &Context,
    store: &dyn StateStore,
    mol_id: &MoleculeId,
) -> anyhow::Result<()> {
    let Ok(mol) = store.load_molecule(mol_id) else {
        return Ok(());
    };
    // The update/restart notice is a codex widget; a Claude pane's passive
    // footer with the same words must not tag the molecule (issue #161).
    if mol.process.as_ref().and_then(|p| p.adapter_name.as_deref()) != Some("codex") {
        return Ok(());
    }
    let Some(session) = mol.tmux_session() else {
        return Ok(());
    };
    let backend = cosmon_transport::TmuxBackend::for_process(
        mol.process.as_ref(),
        &super::tmux_socket_name(ctx),
    )?;
    let Ok(pane) = backend.capture_session_output(session, 80) else {
        return Ok(());
    };
    let kind = cosmon_core::dialogue::classify_codex_dialog(&pane);
    super::patrol::record_update_observation(store, mol_id, kind)
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
#[cfg(test)]
pub(crate) fn record_pane_died(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    raw_exit_code: Option<&str>,
) {
    let _ = record_pane_died_expected(store, state_dir, mol_id, raw_exit_code, None);
}

#[cfg(test)]
fn record_pane_died_expected(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    raw_exit_code: Option<&str>,
    expected: Option<&cosmon_core::worker_exit::LaunchIdentity>,
) -> anyhow::Result<()> {
    record_pane_died_bound(store, state_dir, mol_id, raw_exit_code, expected, None)
}

fn record_pane_died_bound(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    raw_exit_code: Option<&str>,
    expected: Option<&cosmon_core::worker_exit::LaunchIdentity>,
    terminal: Option<&cosmon_core::terminal::TerminalIdentity>,
) -> anyhow::Result<()> {
    // Dispatch B cannot replace A between receipt validation and status projection.
    let _guard = store.lock_fleet()?;
    let molecule = store.load_molecule(mol_id)?;
    if let Some(terminal) = terminal {
        anyhow::ensure!(
            molecule.process.as_ref().and_then(|p| p.terminal.as_ref()) == Some(terminal),
            "stale terminal exit trigger"
        );
    }

    if let Some(process) = molecule.process.as_ref().filter(|p| p.transport.is_some()) {
        if let Some(launch) = &process.worker_launch {
            anyhow::ensure!(
                expected == Some(launch),
                "stale or unbound supervised exit trigger"
            );
        } else {
            anyhow::ensure!(
                terminal.is_some() && terminal == process.terminal.as_ref(),
                "stale or unbound terminal exit trigger"
            );
        }
    }

    #[cfg(unix)]
    match supervised_observation(store, mol_id) {
        Ok(Some((identity, observation))) => {
            anyhow::ensure!(
                expected.is_none_or(|expected| expected == &identity)
                    && identity.molecule_id == *mol_id,
                "stale or foreign worker launch trigger"
            );
            record_supervised_observation(store, state_dir, mol_id, identity, observation)?;
            if observation.is_terminal() {
                project_dead_onto_process_status(store, mol_id);
            }
            return Ok(());
        }
        Err(error) => return Err(error),
        Ok(None) => {
            anyhow::ensure!(
                expected.is_none(),
                "supervised launch metadata missing; result unknown"
            );
        }
    }
    write_worker_exit(store, mol_id, raw_exit_code);
    emit_worker_exited(state_dir, mol_id, raw_exit_code);
    project_dead_onto_process_status(store, mol_id);
    Ok(())
}

#[cfg(unix)]
fn supervised_observation(
    store: &dyn StateStore,
    mol_id: &MoleculeId,
) -> anyhow::Result<
    Option<(
        cosmon_core::worker_exit::LaunchIdentity,
        cosmon_core::worker_exit::WorkerExitObservation,
    )>,
> {
    use cosmon_core::worker_exit::{WorkerExitObservation, WorkerExitStore as _};
    use cosmon_state::worker_exit::FileWorkerExitStore;
    let molecule = store.load_molecule(mol_id).ok();
    let expected = molecule
        .as_ref()
        .and_then(|molecule| molecule.process.as_ref())
        .and_then(|process| process.worker_launch.as_ref());
    match FileWorkerExitStore::current(&store.molecule_dir(mol_id)) {
        Ok(Some(receipts)) => {
            let identity = &receipts.request().identity;
            anyhow::ensure!(
                molecule
                    .as_ref()
                    .is_none_or(|molecule| molecule.process.is_none())
                    || expected.is_some(),
                "dispatch has not bound its supervised launch identity yet"
            );
            anyhow::ensure!(
                &identity.molecule_id == mol_id
                    && expected.is_none_or(|expected| expected == identity),
                "foreign or inconsistent supervised worker identity"
            );
            Ok(Some((identity.clone(), receipts.read())))
        }
        Ok(None) if expected.is_none() => Ok(None),
        Ok(None) | Err(_) if expected.is_some() => {
            Ok(expected.map(|identity| (identity.clone(), WorkerExitObservation::Unknown)))
        }
        Err(error) => Err(anyhow::Error::msg(error)),
        _ => anyhow::bail!("supervised worker result is unknown"),
    }
}

/// Serialize observation writers, then deduplicate using the ledger itself. A crash
/// after append has no separate marker window: the next reader finds that same event.
#[cfg(unix)]
fn record_supervised_observation(
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    mol_id: &MoleculeId,
    identity: cosmon_core::worker_exit::LaunchIdentity,
    observation: cosmon_core::worker_exit::WorkerExitObservation,
) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(
            store
                .molecule_dir(mol_id)
                .join("worker-exit-observation.lock"),
        )?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let events = state_dir.join("events.jsonl");
    let prior = match event_log::read_all(&events) {
        Ok(events) => events,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let found = prior.iter().any(|envelope| matches!(&envelope.event, EventV2::WorkerExitObserved { molecule_id, launch, observation: previous } if molecule_id == mol_id && launch == &identity && previous == &observation));
    if found {
        std::fs::File::open(&events)?.sync_all()?;
    } else {
        event_log::emit_one(
            &events,
            EventV2::WorkerExitObserved {
                molecule_id: mol_id.clone(),
                launch: identity,
                observation,
            },
            None,
        )?;
    }
    let projection = match observation {
        cosmon_core::worker_exit::WorkerExitObservation::Exited { code } => format!("{code}\n"),
        _ => "unknown\n".to_owned(),
    };
    std::fs::write(store.molecule_dir(mol_id).join("worker.exit"), projection)?;
    Ok(())
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
    // Keep a supervised incarnation stable across the subprocess handoff to done.
    // Tackle takes this same per-molecule lock before it can replace a dispatch.
    // Acquire it before fleet (the port's documented lock order).
    #[cfg(unix)]
    let dispatch_guard =
        if supervised_observation(store, mol_id).map_or(true, |value| value.is_some()) {
            Some(store.lock_molecule(mol_id)?)
        } else {
            None
        };
    let guard = store.lock_fleet()?;
    let mol = store.load_molecule(mol_id)?;

    if mol.status != MoleculeStatus::Completed {
        return Ok(HarvestOutcome::NotCompleted);
    }
    #[cfg(unix)]
    match supervised_observation(store, mol_id) {
        Ok(Some((identity, observation))) => {
            // Supervision may have appeared after the preliminary read. Refuse
            // this invocation instead of handing an unpinned launch to done.
            if dispatch_guard.is_none() {
                return Ok(HarvestOutcome::ReceiptUnknown);
            }
            record_supervised_observation(store, state_dir, mol_id, identity, observation)?;
            if !observation.is_terminal() {
                return Ok(HarvestOutcome::ReceiptUnknown);
            }
        }
        Err(_) => return Ok(HarvestOutcome::ReceiptUnknown),
        Ok(None) if dispatch_guard.is_some() => return Ok(HarvestOutcome::ReceiptUnknown),
        Ok(None) => {}
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

    drop(guard); // The child done command takes its own authority and fleet locks.

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

    #[cfg(unix)]
    mod supervised {
        use super::*;
        use cosmon_core::{
            id::WorkerId,
            process::MoleculeProcess,
            worker_exit::{
                LaunchRequest, LaunchToken, WorkerExitObservation, WorkerExitReceipt,
                WorkerExitStore as _,
            },
        };
        use cosmon_state::worker_exit::FileWorkerExitStore;
        type TestResult = Result<(), Box<dyn std::error::Error>>;

        fn setup(
            status: MoleculeStatus,
        ) -> Result<
            (
                tempfile::TempDir,
                FileStore,
                MoleculeId,
                FileWorkerExitStore,
            ),
            Box<dyn std::error::Error>,
        > {
            let root = tempfile::tempdir()?;
            let store = FileStore::new(root.path());
            let id = MoleculeId::new("task-20261019-bbbb")?;
            let mut molecule = sample_mol(&id, status, false);
            store.save_molecule(&id, &molecule)?;
            let receipts = FileWorkerExitStore::prepare(
                &store.molecule_dir(&id),
                LaunchRequest::bind(
                    id.clone(),
                    LaunchToken::from_bytes([1; 16]),
                    LaunchToken::from_bytes([2; 16]),
                    "worker",
                    &[],
                    std::path::Path::new("/work"),
                    &[],
                )?,
            )?;
            let mut process = MoleculeProcess::new(WorkerId::new("worker-a")?, "worker-a");
            process.worker_launch = Some(receipts.request().identity.clone());
            molecule.process = Some(process);
            store.save_molecule(&id, &molecule)?;
            Ok((root, store, id, receipts))
        }

        fn terminal(receipts: &FileWorkerExitStore) -> Result<(), String> {
            receipts.claim(123)?;
            receipts.record_birth(123, 456)?;
            receipts.publish(&WorkerExitReceipt {
                identity: receipts.request().identity.clone(),
                supervisor_pid: 123,
                child_pid: Some(456),
                observation: WorkerExitObservation::Exited { code: 0 },
            })
        }

        #[test]
        fn old_terminal_trigger_cannot_write_exit_or_condemn_replacement() -> TestResult {
            use cosmon_core::terminal::TerminalIdentity;
            use cosmon_core::transport_selection::{
                TransportKind, TransportSelection, TransportSource,
            };
            let (root, store, id, receipts) = setup(MoleculeStatus::Running)?;
            let old = TerminalIdentity::Tmux {
                socket_path: "/old.sock".into(),
                server_pid: 1,
                server_start: 1,
                session_id: 1,
                session_created: 1,
                pane_id: 1,
                pane_pid: 1,
                pane_start: 1,
            };
            let mut replacement = old.clone();
            let TerminalIdentity::Tmux { pane_id, .. } = &mut replacement;
            *pane_id = 2;
            for supervised in [false, true] {
                let mut molecule = store.load_molecule(&id)?;
                let process = molecule.process.as_mut().ok_or("missing process")?;
                process.transport = Some(TransportSelection {
                    backend: TransportKind::Tmux,
                    source: TransportSource::Default,
                });
                process.terminal = Some(replacement.clone());
                process.worker_launch = supervised.then(|| receipts.request().identity.clone());
                store.save_molecule(&id, &molecule)?;
                let before = std::fs::read(store.molecule_dir(&id).join("state.json"))?;
                let log = std::fs::read(root.path().join("events.jsonl")).unwrap_or_default();
                assert!(record_pane_died_bound(
                    &store,
                    root.path(),
                    &id,
                    Some("0"),
                    None,
                    Some(&old)
                )
                .is_err());
                assert!(
                    record_pane_died_bound(&store, root.path(), &id, Some("0"), None, None)
                        .is_err()
                );
                assert_eq!(
                    std::fs::read(store.molecule_dir(&id).join("state.json"))?,
                    before
                );
                assert_eq!(
                    std::fs::read(root.path().join("events.jsonl")).unwrap_or_default(),
                    log
                );
                assert!(!store.molecule_dir(&id).join("worker.exit").exists());
            }
            Ok(())
        }

        #[test]
        fn pane_zero_cannot_complete_or_harvest_a_missing_supervised_receipt() -> TestResult {
            let (root, store, id, receipts) = setup(MoleculeStatus::Completed)?;
            record_pane_died_expected(
                &store,
                root.path(),
                &id,
                Some("0"),
                Some(&receipts.request().identity),
            )?;
            assert!(matches!(
                harvest_one(&store, root.path(), &id, true)?,
                HarvestOutcome::ReceiptUnknown
            ));
            assert_eq!(store.load_molecule(&id)?.status, MoleculeStatus::Completed);
            assert_eq!(
                std::fs::read_to_string(store.molecule_dir(&id).join("worker.exit"))?,
                "unknown\n"
            );
            assert!(!event_log::read_all(root.path().join("events.jsonl"))?
                .iter()
                .any(|event| matches!(event.event, EventV2::WorkerExited { .. })));
            Ok(())
        }

        #[test]
        fn exit_zero_is_an_observation_and_repeated_concurrent_reads_emit_once() -> TestResult {
            let (root, store, id, receipts) = setup(MoleculeStatus::Running)?;
            terminal(&receipts)?;
            std::thread::scope(|scope| -> TestResult {
                let mut readers = Vec::new();
                for _ in 0..6 {
                    readers.push(scope.spawn(|| {
                        record_pane_died_expected(
                            &store,
                            root.path(),
                            &id,
                            Some("143"),
                            Some(&receipts.request().identity),
                        )
                    }));
                }
                for reader in readers {
                    reader.join().map_err(|_| "observation reader panicked")??;
                }
                Ok(())
            })?;
            let events = event_log::read_all(root.path().join("events.jsonl"))?;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event.event, EventV2::WorkerExitObserved { .. }))
                    .count(),
                1
            );
            assert_eq!(store.load_molecule(&id)?.status, MoleculeStatus::Running);
            assert!(matches!(
                harvest_one(&store, root.path(), &id, true)?,
                HarvestOutcome::NotCompleted
            ));
            Ok(())
        }

        #[test]
        fn missing_expected_metadata_never_falls_back_to_legacy_pane_status() -> TestResult {
            let (root, store, id, receipts) = setup(MoleculeStatus::Completed)?;
            terminal(&receipts)?;
            std::fs::remove_file(
                store
                    .molecule_dir(&id)
                    .join(cosmon_state::worker_exit::CURRENT_LAUNCH),
            )?;
            record_pane_died_expected(
                &store,
                root.path(),
                &id,
                Some("0"),
                Some(&receipts.request().identity),
            )?;
            assert!(matches!(
                harvest_one(&store, root.path(), &id, true)?,
                HarvestOutcome::ReceiptUnknown
            ));
            let mut molecule = store.load_molecule(&id)?;
            molecule.process = None;
            store.save_molecule(&id, &molecule)?;
            assert!(matches!(
                harvest_one(&store, root.path(), &id, true)?,
                HarvestOutcome::ReceiptUnknown
            ));
            Ok(())
        }

        #[test]
        fn launch_a_trigger_cannot_project_death_onto_dispatch_b_before_b_is_prepared() -> TestResult
        {
            let (root, store, id, receipts) = setup(MoleculeStatus::Running)?;
            terminal(&receipts)?;
            let old = receipts.request().identity.clone();
            // The dispatch ledger writes B before B can prepare its own launch identity.
            let mut molecule = store.load_molecule(&id)?;
            molecule.process = Some(MoleculeProcess::new(WorkerId::new("worker-b")?, "worker-b"));
            store.save_molecule(&id, &molecule)?;
            assert!(
                record_pane_died_expected(&store, root.path(), &id, Some("0"), Some(&old)).is_err()
            );
            let observed = store.load_molecule(&id)?;
            assert_eq!(
                observed
                    .process
                    .as_ref()
                    .map(|process| process.status.clone()),
                Some(cosmon_core::worker::WorkerStatus::Active)
            );
            assert!(!root.path().join("events.jsonl").exists());
            Ok(())
        }
    }

    fn sample_mol(id: &MoleculeId, status: MoleculeStatus, merged: bool) -> MoleculeData {
        let now = Utc::now();
        MoleculeData {
            harvest_reason: None,
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
            base_branch: None,
            protected_paths: Vec::new(),
            pending_step: None,
            merged_at: if merged { Some(now) } else { None },
            non_integration: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            collapse_receipt: None,
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

    #[test]
    fn issue122_dead_pane_restart_notice_is_recorded_on_molecule() {
        use cosmon_core::id::ProjectId;
        use cosmon_core::transport::TransportBackend;

        let (tmp, store, mid, state_dir) = setup(MoleculeStatus::Running, false);
        let project_id = ProjectId::generate(tmp.path());
        std::fs::write(
            state_dir.join("config.toml"),
            format!("[project]\nproject_id = \"{project_id}\"\n"),
        )
        .unwrap();
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let backend = cosmon_transport::TmuxBackend::new(super::super::tmux_socket_name(&ctx));
        let session = "issue122-restart-pane";
        backend
            .spawn_worker(
                session,
                tmp.path().to_str().unwrap(),
                "printf 'Update ran successfully! Please restart.\\n'; exec sleep 600",
            )
            .unwrap();
        backend.install_pane_died_hook(session, "true").unwrap();
        let mut mol = store.load_molecule(&mid).unwrap();
        mol.session_name = Some(session.to_owned());
        mol.process = Some(
            cosmon_core::process::MoleculeProcess::new(
                cosmon_core::id::WorkerId::new(session).unwrap(),
                session,
            )
            .with_adapter_name("codex"),
        );
        store.save_molecule(&mid, &mol).unwrap();
        let mut notice_visible = false;
        for _ in 0..40 {
            if backend
                .capture_session_output(session, 20)
                .is_ok_and(|pane| pane.contains("Please restart"))
            {
                notice_visible = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(notice_visible);

        let pane_id = std::process::Command::new("tmux")
            .args([
                "-L",
                backend.socket(),
                "list-panes",
                "-t",
                session,
                "-F",
                "#{pane_pid}",
            ])
            .output()
            .unwrap();
        let pane_pid = String::from_utf8(pane_id.stdout).unwrap();
        assert!(std::process::Command::new("kill")
            .args(["-9", pane_pid.trim()])
            .status()
            .unwrap()
            .success());
        let worker = cosmon_core::id::WorkerId::new(session).unwrap();
        for _ in 0..40 {
            if !backend.is_alive(&worker).unwrap_or(true) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!backend.is_alive(&worker).unwrap());

        record_update_from_dead_pane(&ctx, &store, &mid).unwrap();
        let reloaded = store.load_molecule(&mid).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Running);
        assert!(reloaded
            .tags
            .iter()
            .any(|tag| tag.as_str() == "worker-restart-requested"));
        backend.terminate_session(session).unwrap();
    }
}
