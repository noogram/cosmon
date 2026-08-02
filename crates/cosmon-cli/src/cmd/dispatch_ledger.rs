// SPDX-License-Identifier: AGPL-3.0-only

//! The dispatch ledger — recording a spawn is a precondition of spawning it.
//!
//! # The defect this module closes
//!
//! `cs tackle` used to spawn the worker first and record the dispatch
//! afterwards. Between those two acts sat the whole readiness pipeline:
//! the model preflight probe, the 30 s liveness wait, the briefing paste,
//! and the submit-confirmation window. On a healthy dispatch that window
//! measured **~98 seconds** (molecule `task-20260727-cd79`, 2026-07-27:
//! `remote_egress_opt_in` at 10:03:22, `worker_spawned` at 10:05:01).
//!
//! A tmux worker is committed to the operating system the instant
//! `spawn_worker` returns; it is detached and outlives its dispatcher. So
//! anything that ended the dispatcher inside that ~98 s window — an
//! operator `^C` on a dispatch that looked hung, a closed terminal, a
//! host suspend, an OOM kill, or an error return whose best-effort
//! `terminate` did not take — left a live worker with **no ledger entry at
//! all**: molecule still `pending`, `tackled_at` absent, `assigned_worker`
//! absent, no `worker_spawned` event.
//!
//! That state is invisible by construction. `cs patrol`'s orphan scan looks
//! for `Running` molecules whose session died; it cannot see a live session
//! whose molecule never left `pending`. The worker meanwhile cannot record
//! its own progress: `cs evolve` refuses with *"molecule is pending, must be
//! running to evolve"*, so a molecule can complete real, committed work with
//! an empty `completed_steps`.
//!
//! Measured on this fleet on 2026-07-27: 6 of 240 completed molecules
//! (2.5 %) carry that signature — `task-20260720-79cc`,
//! `task-20260720-bbd8`, `task-20260723-778a`, `task-20260723-9d29`,
//! `task-20260725-14f0`, `task-20260727-bbaf`. All six stop at the same
//! event (`remote_egress_opt_in`) and none ever emits `worker_spawned`.
//!
//! # The fix: order, not atomicity
//!
//! A filesystem ledger and a `fork`/`exec` cannot be made to commit
//! together — there is no transaction spanning `state.json` and the kernel
//! process table. So the ledger write is moved to the **near side** of the
//! spawn and rolled back if the spawn fails. The two failure modes are not
//! symmetric:
//!
//! - *recorded, never spawned* → the molecule reads `Running` with a dead
//!   session, which is exactly the shape [`cosmon_runtime::orphan_scan`]
//!   already detects and heals;
//! - *spawned, never recorded* → invisible to every observer cosmon has.
//!
//! We therefore prefer the first, and shrink the unwitnessed window from
//! the whole readiness pipeline down to a single `state.json` write.
//!
//! # Enforced by the type system, not by comment
//!
//! [`DispatchRecorded`] is a token with no public constructor: the only way
//! to obtain one is [`commit_dispatch`], and `spawn_and_prompt` requires one
//! by reference. A future edit that reintroduces spawn-then-record does not
//! compile. This is the typestate discipline `CLAUDE.md` asks for, applied
//! to the one transition where getting the order wrong loses work silently.

use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::spawn_seam::{LoopOwnership, ValidatedAdapterName};
use cosmon_core::tackle::TackledBy;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};

/// Proof that a dispatch was written to the ledger before the worker was
/// spawned.
///
/// Constructed only by [`commit_dispatch`], and required by
/// `crate::cmd::tackle::spawn_and_prompt`. The token carries the identities
/// it witnesses so a caller cannot pair a commit for one molecule with a
/// spawn for another; [`Self::molecule`] and [`Self::worker`] expose them
/// for that check.
#[derive(Debug, Clone)]
pub struct DispatchRecorded {
    molecule: MoleculeId,
    worker: WorkerId,
}

impl DispatchRecorded {
    /// The molecule whose ledger entry this token witnesses.
    #[must_use]
    pub fn molecule(&self) -> &MoleculeId {
        &self.molecule
    }

    /// The worker bound to the molecule by the recorded dispatch.
    #[must_use]
    pub fn worker(&self) -> &WorkerId {
        &self.worker
    }
}

/// Everything the ledger entry needs, gathered at the one call site that
/// knows all of it.
///
/// A struct rather than eight positional arguments: the two `&str` fields
/// (`session_name`, `model`) are trivially transposable and a mix-up would
/// bind a molecule to the wrong tmux session.
pub struct DispatchRecord<'a> {
    /// The worker identity about to be spawned.
    pub worker: &'a WorkerId,
    /// The tmux session name (or in-process sentinel) the worker will own.
    pub session_name: &'a str,
    /// The validated adapter serving this dispatch.
    pub adapter: &'a ValidatedAdapterName,
    /// The adapter's loop-ownership axis, stamped on `WorkerSpawned`.
    pub loop_ownership: LoopOwnership,
    /// The resolved model pin, or `None` for "the adapter's own default".
    pub model: Option<&'a str>,
    /// The actor class holding the dispatch claim (`human` / `runtime:<pid>`).
    pub tackled_by: TackledBy,
    /// The worker's working directory, registered on the fleet entry.
    pub worktree_path: &'a std::path::Path,
    /// The repository root the worktree path is made relative to.
    pub repo_root: &'a std::path::Path,
}

/// Write the dispatch to the ledger **before** the worker is spawned, and
/// return the token that authorises the spawn.
///
/// Performs, under the fleet lock, exactly what `cs tackle` used to perform
/// after the spawn: flip `Pending`/`Queued`/`Frozen` → `Running`, bind the
/// process record, stamp the anti-preemption claim, save the molecule, and
/// register the worker in `fleet.json` (which emits `WorkerSpawned`).
///
/// `Frozen` is in that set because a recorded dispatch *is* a thaw (COSMON #35
/// §4). Freezing is how patrol parks work whose worker died; the respawn that
/// picks it back up leaves a live worker attached to a molecule that reads
/// `frozen`, which `cs peek` renders as `◉ stuck` while the pane produces
/// output. Liveness that is wrong in the reassuring direction (§1) and wrong
/// in the alarming direction (§4) costs the same thing: an operator who cannot
/// use recorded state to decide anything.
///
/// The PID witness is deliberately absent here — a process id is not
/// knowable before the process exists. [`stamp_pid_witness`] adds it after
/// the spawn returns, as a second write to an already-`Running` molecule.
///
/// # Errors
///
/// Returns an error if the fleet lock cannot be taken, if the molecule or
/// fleet cannot be saved, or if the `WorkerSpawned` event cannot be
/// appended. In every error case nothing has been spawned **and nothing is
/// left recorded**: the writes this function already landed are undone
/// before the error returns.
///
/// That second half is not decoration. `register_tackle_worker`'s
/// `emit_one` is the only fallible step that runs *after* both ledger
/// writes have landed, and the state it would otherwise leave behind — a
/// `Running` molecule, a bound `MoleculeProcess`, an `Active` fleet worker,
/// and **no `WorkerSpawned` on the wire** — is bit-for-bit the forensic
/// signature `d62ba58` used to identify the six lost molecules this module
/// exists to prevent. Leaving it behind would have made that signature
/// reachable from a dispatch that correctly refused to start, which is the
/// one reading the forensics must never have to doubt.
///
/// The undo is performed here rather than in the callers because it is the
/// only place that both knows every write that landed and already holds the
/// fleet lock. A caller-side [`rollback_dispatch`] would deadlock against
/// that lock, and `cs resurrect`'s bare `?` would have to remember to make
/// the call at all.
pub fn commit_dispatch(
    store: &FileStore,
    mol: &MoleculeData,
    record: &DispatchRecord<'_>,
) -> anyhow::Result<(MoleculeData, DispatchRecorded)> {
    let mol_id = mol.id.clone();
    let _g = store.lock_fleet()?;
    let mut updated = mol.clone();
    if matches!(
        updated.status,
        MoleculeStatus::Pending | MoleculeStatus::Queued | MoleculeStatus::Frozen
    ) {
        updated.status = MoleculeStatus::Running;
    }
    let process = cosmon_core::process::MoleculeProcess::new(
        record.worker.clone(),
        record.session_name.to_owned(),
    )
    .with_adapter_name(record.adapter.as_str())
    .with_model(record.model);
    updated.bind_process(process);
    updated.mark_tackled(record.tackled_by.clone());
    store.save_molecule(&mol_id, &updated)?;

    if let Err(e) = crate::cmd::tackle::register_tackle_worker(
        store,
        record.worker,
        record.worktree_path,
        record.repo_root,
        &updated,
        record.adapter,
        record.loop_ownership,
    ) {
        // `mol` is the pre-commit snapshot by construction — this function
        // took it by reference and only ever mutated the `updated` clone.
        undo_committed_writes(store, mol, record.worker);
        return Err(e);
    }

    Ok((
        updated,
        DispatchRecorded {
            molecule: mol_id,
            worker: record.worker.clone(),
        },
    ))
}

/// Stamp the spawned worker's PID and launch fingerprint on the ledger
/// entry [`commit_dispatch`] already wrote.
///
/// Separate from the commit because the value does not exist until the
/// process does. Best-effort by design: the molecule is already `Running`
/// and supervised through its tmux session, so a failed second write costs
/// the PID axis of `orphan_scan`'s liveness check, never the dispatch.
///
/// Returns `true` when the fingerprint landed.
pub fn stamp_pid_witness(
    store: &FileStore,
    mol_id: &MoleculeId,
    pid: u32,
    pid_start_time: Option<u64>,
) -> bool {
    let Ok(_g) = store.lock_fleet() else {
        return false;
    };
    let Ok(mut mol) = store.load_molecule(mol_id) else {
        return false;
    };
    let Some(process) = mol.process.take() else {
        return false;
    };
    let mut process = process.with_pid(pid);
    if let Some(start) = pid_start_time {
        process = process.with_pid_start_time(start);
    }
    mol.bind_process(process);
    store.save_molecule(mol_id, &mol).is_ok()
}

/// Undo the ledger entry when the spawn it authorised did not happen.
///
/// Restores the molecule exactly as it stood before [`commit_dispatch`]
/// (the caller passes the pre-commit snapshot) and removes the worker from
/// `fleet.json`. Best-effort throughout: this runs on a path that is
/// already returning an error, and a rollback failure must not mask the
/// original cause. What it leaves behind on failure is the recoverable
/// shape — a `Running` molecule with no session — not the invisible one.
pub fn rollback_dispatch(store: &FileStore, prior: &MoleculeData, worker: &WorkerId) {
    if let Ok(_g) = store.lock_fleet() {
        undo_committed_writes(store, prior, worker);
    }
}

/// The undo itself, with the fleet lock left to the caller.
///
/// Split out of [`rollback_dispatch`] so [`commit_dispatch`] can call it
/// from inside its own critical section. The fleet lock is an advisory
/// `flock` taken on a freshly opened file descriptor, so it is **not**
/// reentrant: a `commit_dispatch` that called `rollback_dispatch` while
/// holding its guard would block on itself forever, which is the mute hang
/// this codebase treats as worse than the error it was handling.
///
/// Best-effort throughout, for [`rollback_dispatch`]'s reason: this only
/// ever runs on a path already returning an error, and a failed undo must
/// not mask the original cause.
fn undo_committed_writes(store: &FileStore, prior: &MoleculeData, worker: &WorkerId) {
    let mut fleet = store.load_fleet().unwrap_or_default();
    fleet.workers.remove(worker);
    let _ = store.save_fleet(&fleet);
    let _ = store.save_molecule(&prior.id, prior);
}

/// A live worker session whose molecule does not say it is running.
///
/// The mirror image of [`cosmon_runtime::orphan_scan`]'s orphan: that one
/// finds a `Running` molecule with a dead session, this one finds a live
/// session with a molecule that never left `pending`. Before the
/// commit-before-spawn reordering, nothing in cosmon could name this state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrecordedDispatch {
    /// The molecule that owns the live session but does not admit it.
    pub molecule: MoleculeId,
    /// The tmux session observed alive.
    pub session_name: String,
    /// The status the molecule actually reads.
    pub status: MoleculeStatus,
}

impl UnrecordedDispatch {
    /// One line an operator can act on, without cosmon vocabulary.
    ///
    /// Cosmon has no verb that adopts a running session into a molecule, so
    /// this deliberately does not invent one. It names the two moves that
    /// exist: the worker's commits are recoverable from its branch by hand,
    /// or the dispatch can be restarted under a recorded one.
    #[must_use]
    pub fn operator_line(&self) -> String {
        format!(
            "molecule {mol} reads `{status}` but its worker session \
             `{session}` is alive and working. Nothing recorded the dispatch, \
             so the worker cannot run `cs evolve` and its progress is not \
             being counted. Its commits are still real — look at branch \
             `feat/{mol}`. Either let it finish and merge that branch by hand, \
             or restart it under a recorded dispatch with \
             `cs tackle {mol} --force` (which kills the session and loses its \
             in-flight context, not its commits).",
            mol = self.molecule.as_str(),
            status = self.status,
            session = self.session_name,
        )
    }
}

/// Scan non-running molecules for a live worker session.
///
/// `expected_session` maps a molecule to the session name a dispatch would
/// have given it (`cs tackle` derives it deterministically from the topic
/// and id, so the mapping is reconstructible without any stored pointer —
/// which is the whole point: an unrecorded dispatch stored no pointer).
/// `is_alive` answers whether that session exists.
///
/// Both are injected so the scan is a pure function over its inputs and
/// testable without a tmux server.
pub fn scan_unrecorded_dispatches<S, A>(
    molecules: &[MoleculeData],
    expected_session: S,
    is_alive: A,
) -> Vec<UnrecordedDispatch>
where
    S: Fn(&MoleculeData) -> String,
    A: Fn(&str) -> bool,
{
    molecules
        .iter()
        .filter(|m| is_unrecorded_candidate(m.status))
        .filter_map(|m| {
            let session = expected_session(m);
            is_alive(&session).then(|| UnrecordedDispatch {
                molecule: m.id.clone(),
                session_name: session,
                status: m.status,
            })
        })
        .collect()
}

/// Which molecule statuses are suspicious when a live session exists.
///
/// `Running` is the recorded case and `Frozen` is a deliberate pause with a
/// session still bound (`cs freeze` keeps the pane). Everything else — a
/// molecule that says it was never dispatched, or that says it is already
/// finished — has no business owning a live worker.
fn is_unrecorded_candidate(status: MoleculeStatus) -> bool {
    !matches!(status, MoleculeStatus::Running | MoleculeStatus::Frozen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;

    /// A pending `task-work` molecule, the shape all six victims had.
    fn pending_molecule() -> MoleculeData {
        let now = chrono::Utc::now();
        MoleculeData {
            id: MoleculeId::new("task-20260727-aaaa").expect("id"),
            fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet"),
            formula_id: cosmon_core::id::FormulaId::new("task-work").expect("formula"),
            status: MoleculeStatus::Pending,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 0,
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
            tags: std::collections::BTreeSet::new(),
            escalations: vec![],
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
        }
    }

    /// Build a store with one pending molecule, returning both.
    fn fixture() -> (tempfile::TempDir, FileStore, MoleculeData) {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let store = FileStore::new(&state_dir);
        let mol = pending_molecule();
        store
            .save_molecule(&mol.id, &mol)
            .expect("save seed molecule");
        (dir, store, mol)
    }

    fn adapter() -> ValidatedAdapterName {
        let (adapter, _, _) = cosmon_core::spawn_seam::validate_adapter_name(
            "claude",
            &["claude".to_owned(), "aider".to_owned()],
        )
        .expect("'claude' is built in");
        adapter
    }

    fn record<'a>(
        worker: &'a WorkerId,
        adapter: &'a ValidatedAdapterName,
        path: &'a std::path::Path,
    ) -> DispatchRecord<'a> {
        DispatchRecord {
            worker,
            session_name: "rewrite-briefing-aaaa",
            adapter,
            loop_ownership: LoopOwnership::External,
            model: Some("claude-opus-5"),
            tackled_by: TackledBy::Human,
            worktree_path: path,
            repo_root: path,
        }
    }

    /// The defect, stated as a test: after the commit and BEFORE any spawn
    /// has happened, an independent reader of `state.json` already sees a
    /// dispatched molecule. This is the property whose absence lost the
    /// work of six molecules — every observer that looked between the spawn
    /// and the (never-reached) record saw `pending`.
    #[test]
    fn ledger_shows_the_dispatch_before_any_worker_exists() {
        let (dir, store, mol) = fixture();
        let worker = WorkerId::new("rewrite-briefing-aaaa").expect("worker id");
        let adapter = adapter();
        let (_updated, token) =
            commit_dispatch(&store, &mol, &record(&worker, &adapter, dir.path()))
                .expect("commit must succeed");

        // A *fresh* read, as `cs observe` or the worker's own `cs evolve`
        // would do — not the in-memory value the committer happens to hold.
        let observed = store.load_molecule(&mol.id).expect("re-read");
        assert_eq!(observed.status, MoleculeStatus::Running);
        assert!(
            observed.tackled_at.is_some(),
            "tackled_at must be stamped before the spawn, not after"
        );
        assert_eq!(observed.tackled_by, Some(TackledBy::Human));
        assert_eq!(observed.worker(), Some(&worker));
        assert_eq!(observed.tmux_session(), Some("rewrite-briefing-aaaa"));
        assert_eq!(token.molecule(), &mol.id);
        assert_eq!(token.worker(), &worker);

        // And the fleet knows the worker, so `cs patrol` can supervise it
        // from this instant onwards.
        let fleet = store.load_fleet().expect("fleet");
        assert!(fleet.workers.contains_key(&worker));
    }

    /// COSMON #35 §4 — a re-dispatch thaws.
    ///
    /// The reporter recovered a crashed worker with `cs tackle --force`, the
    /// new worker demonstrably ran (fresh persona responses landing on disk),
    /// and the molecule went on reading `frozen` — which `cs peek` renders as
    /// `◉ stuck`. Recording a dispatch and leaving the molecule parked is a
    /// contradiction in state: the seat is occupied by a live worker.
    #[test]
    fn a_recorded_dispatch_thaws_a_frozen_molecule() {
        let (dir, store, mut mol) = fixture();
        mol.status = MoleculeStatus::Frozen;
        store.save_molecule(&mol.id, &mol).expect("seed frozen");

        let worker = WorkerId::new("rewrite-briefing-aaaa").expect("worker id");
        let adapter = adapter();
        commit_dispatch(&store, &mol, &record(&worker, &adapter, dir.path()))
            .expect("commit must succeed");

        assert_eq!(
            store.load_molecule(&mol.id).expect("re-read").status,
            MoleculeStatus::Running,
            "a frozen molecule handed a live worker must read `running`, not \
             `stuck`"
        );
    }

    /// The other half of the contract: a spawn that never happened must
    /// leave no trace. Otherwise "record first" would trade an invisible
    /// worker for a permanent phantom.
    #[test]
    fn rollback_restores_the_pre_dispatch_snapshot() {
        let (dir, store, mol) = fixture();
        let worker = WorkerId::new("rewrite-briefing-aaaa").expect("worker id");
        let adapter = adapter();
        commit_dispatch(&store, &mol, &record(&worker, &adapter, dir.path()))
            .expect("commit must succeed");

        rollback_dispatch(&store, &mol, &worker);

        let observed = store.load_molecule(&mol.id).expect("re-read");
        assert_eq!(observed.status, MoleculeStatus::Pending);
        assert!(observed.tackled_at.is_none(), "claim must be released");
        assert!(
            observed.worker().is_none(),
            "worker binding must be released"
        );
        let fleet = store.load_fleet().expect("fleet");
        assert!(!fleet.workers.contains_key(&worker));
    }

    /// The missing falsifier for the `?` on `emit_one`.
    ///
    /// Round 2 replaced a `let _ =` with a propagating `?` on the
    /// `WorkerSpawned` emit, and both referee seats measured the same thing:
    /// reverting it left all six `register_tackle_worker` tests green. A
    /// behaviour change that no test can see is not a closed finding.
    ///
    /// The unwritable event log is made unwritable by *type*, not by
    /// permission: `events.jsonl` is a directory, so the append fails with
    /// `EISDIR` for every user including root. A mode-bit fixture would have
    /// been inert under euid 0 — the same defect this round found next door
    /// in `demote_git_plumbing_scope.rs`, and it is not worth importing it
    /// here to save three lines.
    ///
    /// Two properties, both required:
    /// 1. the dispatch is **refused** — no token, hence no spawn, because
    ///    `spawn_and_prompt` cannot be called without one; and
    /// 2. nothing is **left recorded** — the fleet holds no worker and the
    ///    molecule is not `Running`. Property 2 is the one that was missing
    ///    in code as well as in test: without the undo, this path produced a
    ///    phantom worker carrying the exact signature of the six lost
    ///    molecules.
    #[test]
    fn an_unwritable_event_log_refuses_the_dispatch_and_leaves_no_ledger_entry() {
        let (dir, store, mol) = fixture();
        // Occupy the event log path with a directory: `emit_one`'s append
        // cannot open it, and no uid can make it work.
        std::fs::create_dir_all(store.state_root().join("events.jsonl"))
            .expect("occupy the event log path");

        let worker = WorkerId::new("rewrite-briefing-aaaa").expect("worker id");
        let adapter = adapter();
        let err = commit_dispatch(&store, &mol, &record(&worker, &adapter, dir.path()))
            .expect_err("an unrecordable dispatch must be refused, not spawned");
        assert!(
            err.to_string().contains("WorkerSpawned"),
            "the refusal must name what could not be recorded: {err}"
        );

        // Property 2 — nothing survives the refusal.
        let observed = store.load_molecule(&mol.id).expect("re-read");
        assert_eq!(
            observed.status,
            MoleculeStatus::Pending,
            "a refused dispatch must not leave a Running molecule: that is \
             the `d62ba58` signature the propagation exists to protect"
        );
        assert!(
            observed.worker().is_none(),
            "a refused dispatch must not leave a bound worker"
        );
        assert!(
            observed.tackled_at.is_none(),
            "a refused dispatch must not leave the anti-preemption claim"
        );
        let fleet = store.load_fleet().unwrap_or_default();
        assert!(
            !fleet.workers.contains_key(&worker),
            "a refused dispatch must not leave a phantom worker in fleet.json"
        );
    }

    /// The PID witness lands on the already-committed record rather than
    /// replacing it — the adapter and session survive the second write.
    #[test]
    fn pid_witness_lands_without_disturbing_the_commit() {
        let (dir, store, mol) = fixture();
        let worker = WorkerId::new("rewrite-briefing-aaaa").expect("worker id");
        let adapter = adapter();
        commit_dispatch(&store, &mol, &record(&worker, &adapter, dir.path()))
            .expect("commit must succeed");

        assert!(stamp_pid_witness(&store, &mol.id, 4242, Some(99)));

        let observed = store.load_molecule(&mol.id).expect("re-read");
        let process = observed.process.as_ref().expect("process record");
        assert_eq!(process.pid, Some(4242));
        assert_eq!(process.pid_start_time, Some(99));
        assert_eq!(process.adapter_name.as_deref(), Some("claude"));
        assert_eq!(process.tmux_session, "rewrite-briefing-aaaa");
    }

    /// The detector reproduces the observed failure state: a `pending`
    /// molecule whose derived session name is alive. This is exactly what
    /// `task-20260727-bbaf` looked like from 10:24 to 10:43 on 2026-07-27,
    /// and what nothing in cosmon noticed.
    #[test]
    fn scan_names_a_live_session_on_a_pending_molecule() {
        let (_dir, _store, mol) = fixture();
        let found = scan_unrecorded_dispatches(
            std::slice::from_ref(&mol),
            |_| "rewrite-briefing-aaaa".to_owned(),
            |_| true,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].molecule, mol.id);
        assert_eq!(found[0].status, MoleculeStatus::Pending);
        let line = found[0].operator_line();
        assert!(line.contains("is alive and working"), "{line}");
        assert!(
            line.contains("feat/task-20260727-aaaa"),
            "the operator must be told where the worker's commits are: {line}"
        );
        assert!(
            line.contains("cs tackle task-20260727-aaaa --force"),
            "the operator must be given a move that exists: {line}"
        );
    }

    /// A recorded dispatch is not a finding — otherwise every healthy
    /// worker on the fleet would be reported and the signal would be worth
    /// nothing.
    #[test]
    fn scan_ignores_molecules_that_admit_they_are_running() {
        let (_dir, _store, mut mol) = fixture();
        mol.status = MoleculeStatus::Running;
        let found =
            scan_unrecorded_dispatches(std::slice::from_ref(&mol), |_| "s".to_owned(), |_| true);
        assert!(found.is_empty());

        // Frozen keeps its pane on purpose (`cs freeze`), so it is not a
        // finding either.
        mol.status = MoleculeStatus::Frozen;
        let found =
            scan_unrecorded_dispatches(std::slice::from_ref(&mol), |_| "s".to_owned(), |_| true);
        assert!(found.is_empty());
    }

    /// No live session, no finding — a plain undispatched molecule is the
    /// normal resting state of the backlog.
    #[test]
    fn scan_ignores_pending_molecules_with_no_session() {
        let (_dir, _store, mol) = fixture();
        let found =
            scan_unrecorded_dispatches(std::slice::from_ref(&mol), |_| "s".to_owned(), |_| false);
        assert!(found.is_empty());
    }
}
