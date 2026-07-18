// SPDX-License-Identifier: AGPL-3.0-only

//! Cosmon Resident Runtime — skeleton (ADR-016 Phase 3, sub-task 1/3).
//!
//! # Role in the architecture
//!
//! The **Resident Runtime** is the optional long-lived layer that sits above
//! the stateless [`cosmon_state::StateStore`] transactional core. It owns an
//! event loop that:
//!
//! 1. Loads the current fleet [`FleetSnapshot`] from the shared store.
//! 2. Asks a pluggable [`Policy`] for the next [`RuntimeAction`]s.
//! 3. Applies those actions as transactional mutations against the same
//!    store the CLI uses.
//! 4. Waits for observable state changes (currently: a polling interval).
//! 5. Repeats until the policy returns an empty action set, or a shutdown
//!    signal fires.
//!
//! # Coherence invariants (ADR-016 §1)
//!
//! The runtime is a **client** of the transactional core, not a replacement.
//! It does not introduce a second source of truth, nor does it bypass the
//! `StateStore` trait. Every mutation it performs must correspond to a
//! pre-existing CLI-visible transition (`nucleate`, `evolve`, `complete`,
//! `collapse`). A human operator can call `cs observe` or `cs freeze` while
//! the runtime is running — they share the same JSON state files.
//!
//! # What is in this sub-task
//!
//! This crate ships only the skeleton:
//!
//! - The [`Policy`] trait (what scheduling policies implement).
//! - The [`RuntimeAction`] enum (commands the policy asks the loop to execute).
//! - The [`FleetSnapshot`] view (read-only projection of store state).
//! - The [`Runtime`] struct holding a `StateStore` + `Policy` + loop config.
//! - A trivial [`NoOpPolicy`] that returns an empty action vector so the
//!   loop exits immediately — enough to test graceful shutdown.
//!
//! Concrete policies (`DagPolicy`, `DynamicDagPolicy`) and the `cs run` CLI
//! adapter are sub-tasks 2 and 3 of this phase.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::HashSet;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use cosmon_core::error::CosmonError;
use cosmon_core::id::{FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};

mod dag_policy;
pub mod guard;
pub mod resident;
pub mod witness;

pub use dag_policy::{
    compile_plan, dag_depth, load_parallel_limits, load_step_models, DagPolicy, ModelResolver,
};
pub use guard::{
    check_backlog, check_prior_seal, compute_sediment, current_threshold, emit_seal_bypassed,
    is_sediment, BacklogGuardError, SealGuardError, SealReport, SedimentReport,
    DEFAULT_STALE_THRESHOLD, SEDIMENT_AGE_HOURS, THRESHOLD_ENV_VAR,
};
pub use resident::{
    Decision, EnsembleMolecule, EnsembleSnapshot, ExitReason, ReadyFrontierScheduler,
    ResidentError, ResidentScheduler, RunSummary, RuntimeLoop, RuntimeLoopConfig,
};
pub use witness::{
    canonical_attestation_record, compute_attestation_b3, refuse_if_same_session,
    resolve_witness_id, resolve_witness_id_from, SameSessionRefusal, ATTESTATION_RECORD_SCHEMA,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failure modes of the resident runtime event loop.
///
/// Every variant corresponds to a concrete place in the loop where progress
/// stops being possible; callers of [`Runtime::run`] can match on the variant
/// to decide whether to retry, escalate, or bail out.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The underlying [`StateStore`] returned an error.
    #[error("state store error: {0}")]
    State(#[from] CosmonError),

    /// The policy asked the runtime to apply an action that is not yet
    /// wired up in this skeleton (e.g. `Nucleate` before sub-task 3).
    #[error("action not yet supported by the runtime skeleton: {0}")]
    Unsupported(&'static str),

    /// Worker dispatch failed (e.g. `cs tackle` could not be spawned or
    /// exited with an error).
    #[error("dispatch failed for molecule {id}: {reason}")]
    Dispatch {
        /// The molecule that failed to dispatch.
        id: MoleculeId,
        /// Human-readable reason for the failure.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// FleetSnapshot — read-only projection handed to the policy
// ---------------------------------------------------------------------------

/// An immutable view of the fleet state passed to [`Policy::next_actions`].
///
/// The runtime loads this once per tick from the [`StateStore`] and hands it
/// to the policy. Policies **must not** mutate the snapshot; they decide
/// based on it and return [`RuntimeAction`]s that the runtime then applies
/// through the store. This separation keeps policies trivially testable
/// (they are pure functions of their input) and keeps the "who owns the
/// right to mutate" question unambiguous.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FleetSnapshot {
    /// All molecules currently known to the store, regardless of status.
    pub molecules: Vec<MoleculeData>,
}

impl FleetSnapshot {
    /// Build a snapshot by listing all molecules from a [`StateStore`].
    ///
    /// # Errors
    /// Returns the underlying [`CosmonError`] if the store cannot be read.
    pub fn load(store: &dyn StateStore) -> Result<Self, CosmonError> {
        let molecules = store.list_molecules(&MoleculeFilter::default())?;
        Ok(Self { molecules })
    }

    /// Returns `true` if no molecule is present in the snapshot.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.molecules.is_empty()
    }
}

// ---------------------------------------------------------------------------
// RuntimeAction — the policy-to-runtime command vocabulary
// ---------------------------------------------------------------------------

/// A request from a [`Policy`] asking the runtime to perform one transactional
/// mutation against the shared [`StateStore`].
///
/// Variants mirror the `cs` CLI verbs one-to-one. The runtime is responsible
/// for translating each variant into the equivalent store call (or, in later
/// sub-tasks, into a child `cs` subprocess invocation). In this skeleton only
/// [`RuntimeAction::NoOp`] is fully supported — the other variants return
/// [`RuntimeError::Unsupported`] so the shape of the enum is frozen and
/// future sub-tasks can fill them in incrementally.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RuntimeAction {
    /// Nucleate a new molecule with the given formula id and variables.
    Nucleate {
        /// Formula to instantiate.
        formula: FormulaId,
        /// Variables forwarded to the formula on nucleation.
        variables: Vec<(String, String)>,
    },
    /// Evolve an existing molecule by one step.
    Evolve {
        /// Target molecule.
        id: MoleculeId,
        /// Free-form evidence string recorded on the transition.
        evidence: String,
    },
    /// Complete a molecule, recording the reason on the terminal transition.
    Complete {
        /// Target molecule.
        id: MoleculeId,
        /// Reason for completion.
        reason: String,
    },
    /// Collapse a molecule into a terminal failure state.
    Collapse {
        /// Target molecule.
        id: MoleculeId,
        /// Reason for collapse.
        reason: String,
    },
    /// A sentinel the policy can emit to say "nothing to do this tick".
    ///
    /// Useful when the policy wants to keep the loop alive for another
    /// interval without actually mutating state.
    NoOp,
}

// ---------------------------------------------------------------------------
// Executor — the dispatch interface (C7: real worker dispatch)
// ---------------------------------------------------------------------------

/// The pluggable dispatch function that translates a scheduling decision into
/// real worker execution.
///
/// When the runtime's [`Policy`] emits a [`RuntimeAction::Evolve`], the
/// runtime transitions the molecule to [`MoleculeStatus::Running`] and then
/// calls [`Executor::dispatch`] to hand off actual work. This separation
/// keeps the runtime agnostic to *how* workers are spawned — the default
/// [`SubprocessExecutor`] calls `cs tackle`, but tests can inject a
/// [`NoOpExecutor`] that skips the subprocess.
///
/// # Object safety
///
/// The trait is object-safe so the runtime can hold a `Box<dyn Executor>`.
pub trait Executor {
    /// Dispatch a molecule to a worker for execution.
    ///
    /// Called after the molecule has been transitioned to `Running` in the
    /// store. Implementations should spawn the worker asynchronously (e.g.
    /// `cs tackle` creates a tmux pane + worktree) — this method must not
    /// block until the worker finishes.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] if the dispatch fails (e.g. subprocess
    /// cannot be spawned).
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError>;

    /// Called when a molecule transitions to Completed during a tick.
    ///
    /// The runtime calls this BEFORE computing the next ready frontier,
    /// so implementations can merge the worker's branch into main. This
    /// ensures downstream molecules (dispatched next) see the predecessor's
    /// output in their worktree.
    ///
    /// Default: no-op (for test executors that don't manage worktrees).
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] if the teardown fails. Implementations
    /// should treat failures as non-fatal (log and continue).
    fn on_complete(&self, _id: &MoleculeId) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// Drive a `Running` molecule whose current step does not require a
    /// dedicated worker process (native / shell-gate). Called every tick
    /// by the runtime so mixed formulas (claude → native → claude) don't
    /// stall on the native tail after the claude worker exits.
    ///
    /// Implementations should be fast and idempotent: the runtime may
    /// invoke this for the same molecule on repeated ticks if it stays
    /// `Running`. Returning `Ok(false)` means "no in-process work was
    /// done on this tick" (e.g. the current step is a claude step and
    /// is owned by its tmux worker). `Ok(true)` means the executor
    /// executed at least one native/gate step.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] on dispatch failures. Implementations
    /// should treat "formula missing" / "step not native" as `Ok(false)`.
    fn drain_native_tail(&self, _mol: &MoleculeData) -> Result<bool, RuntimeError> {
        Ok(false)
    }
}

/// An executor that calls `cs tackle <id>` as a subprocess.
///
/// This is the production executor: it creates a worktree, tmux pane, and
/// fleet worker entry for the molecule, then returns immediately. The worker
/// runs independently; the runtime observes its progress through the shared
/// [`StateStore`] on subsequent ticks.
///
/// The [`quiet`](Self::quiet) flag silences child stdout/stderr so callers
/// like `cs run` that render their own event log aren't flooded by the
/// chatter of every dispatched subprocess.
#[derive(Debug, Clone)]
pub struct SubprocessExecutor {
    /// Working directory passed to the subprocess. `cs tackle` resolves its
    /// `.cosmon/` directory by walking up from `cwd`.
    cwd: std::path::PathBuf,
    /// When true, redirect child stdout and stderr to `/dev/null` so the
    /// parent's own rendering stays clean.
    quiet: bool,
}

impl SubprocessExecutor {
    /// Create a new subprocess executor rooted at the given directory.
    ///
    /// The directory should be the project root containing `.cosmon/`.
    #[must_use]
    pub fn new(cwd: impl Into<std::path::PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            quiet: false,
        }
    }

    /// Silence child stdout/stderr. Returns `self` so callers can chain:
    /// `SubprocessExecutor::new(dir).quiet(true)`.
    #[must_use]
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }
}

impl Executor for SubprocessExecutor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        // `cs tackle` is always a leaf dispatch since the unification
        // landed in delib-20260426-1bcd #2 / task-20260426-c33f. The
        // runtime owns the DAG walk; its executor only dispatches one
        // ready node at a time. `COSMON_RUNTIME_ACTIVE` is preserved as
        // a defensive marker so downstream tooling (and any pre-grace
        // `cs tackle` binary that still auto-detects) can recognise the
        // nested-runtime context.
        // Pass `--by runtime:<pid>` so `cs tackle` stamps the dispatch
        // claim as runtime-owned (task-20260531-a12f). Without it `cs tackle`
        // would default to `human` and overwrite the runtime's claim — which
        // would make the runtime's own dispatch look like a sticky human
        // lease. `<pid>` is the `cs run` process id (this executor runs
        // inside it), matching the claim `apply_evolve` stamped on the flip.
        let mut cmd = Command::new("cs");
        cmd.arg("tackle")
            .arg(id.as_str())
            .arg("--by")
            .arg(format!("runtime:{}", std::process::id()))
            .env("COSMON_RUNTIME_ACTIVE", "1")
            .current_dir(&self.cwd);
        if self.quiet {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        let status = cmd.status().map_err(|e| RuntimeError::Dispatch {
            id: id.clone(),
            reason: format!("failed to spawn cs tackle: {e}"),
        })?;

        if !status.success() {
            return Err(RuntimeError::Dispatch {
                id: id.clone(),
                reason: format!(
                    "cs tackle exited with {}",
                    status
                        .code()
                        .map_or_else(|| "signal".to_owned(), |c| c.to_string())
                ),
            });
        }

        Ok(())
    }

    fn on_complete(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        // Merge the worker's branch into main via `cs done`.
        // This propagates the worker's output so downstream molecules
        // (dispatched next) see it in their worktree. Without this,
        // each worker lives in an isolated branch and never sees
        // the predecessor's work.
        let mut cmd = Command::new("cs");
        cmd.args(["done", id.as_str()]).current_dir(&self.cwd);
        if self.quiet {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        let status = cmd.status().map_err(|e| RuntimeError::Dispatch {
            id: id.clone(),
            reason: format!("cs done failed to launch: {e}"),
        })?;

        if !status.success() {
            // Non-fatal: the molecule IS completed in cosmon state.
            // The merge might fail (conflict, no branch, etc.) but
            // the DAG should continue. Log and move on.
            eprintln!(
                "⚠ cs done {} exited with {} (non-fatal)",
                id,
                status.code().unwrap_or(-1)
            );
        }

        Ok(())
    }

    fn drain_native_tail(&self, mol: &MoleculeData) -> Result<bool, RuntimeError> {
        // Only drive molecules that are actively progressing.
        if mol.status != MoleculeStatus::Running {
            return Ok(false);
        }

        // Load the formula from the project's formulas dir. We walk up
        // from the executor's cwd so this works whether the process was
        // launched at the repo root or inside `.cosmon/`.
        let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(&self.cwd);
        let formula_path = formulas_dir.join(format!("{}.formula.toml", mol.formula_id.as_str()));
        let Ok(toml_text) = std::fs::read_to_string(&formula_path) else {
            return Ok(false);
        };
        let Ok(formula) = cosmon_core::formula::Formula::parse(&toml_text) else {
            return Ok(false);
        };
        let Some(step) = formula.steps.get(mol.current_step) else {
            return Ok(false);
        };

        // Unified dispatch — a Running molecule's current step is either:
        //   - native / shell-gate → cascade in-process via `cs tackle`
        //   - claude with no assigned worker → spawn worker via same call
        //   - claude with a live worker → worker owns progress, skip
        // `cs tackle` handles all three routing decisions itself; we
        // only filter out the "worker already owns it" case so we don't
        // spawn a second session every tick. (Since the verb-unification
        // of delib-20260426-1bcd, `cs tackle` is always leaf — the
        // historical `--leaf` flag is no longer required.)
        let bypasses = step.is_automated();
        if !bypasses && mol.assigned_worker.is_some() {
            return Ok(false);
        }

        let mut cmd = Command::new("cs");
        cmd.arg("tackle")
            .arg(mol.id.as_str())
            .arg("--by")
            .arg(format!("runtime:{}", std::process::id()))
            .env("COSMON_RUNTIME_ACTIVE", "1")
            .current_dir(&self.cwd);
        if self.quiet {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        let status = cmd.status().map_err(|e| RuntimeError::Dispatch {
            id: mol.id.clone(),
            reason: format!("drain_native_tail spawn failed: {e}"),
        })?;
        if !status.success() {
            return Err(RuntimeError::Dispatch {
                id: mol.id.clone(),
                reason: format!(
                    "cs tackle exited with {}",
                    status
                        .code()
                        .map_or_else(|| "signal".to_owned(), |c| c.to_string())
                ),
            });
        }
        Ok(true)
    }
}

/// An executor that does nothing — used in tests and for the [`NoOpPolicy`]
/// integration path where no real worker dispatch is needed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpExecutor;

impl Executor for NoOpExecutor {
    fn dispatch(&self, _id: &MoleculeId) -> Result<(), RuntimeError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LivenessCheck — detect orphaned workers at runtime startup
// ---------------------------------------------------------------------------

/// Probe for whether a worker's tmux session is still alive.
///
/// The runtime calls [`LivenessCheck::is_session_alive`] once at startup for
/// every molecule in `Running` status with a stamped `session_name`. If the
/// session is dead, the runtime emits an `ORPHAN WARNING` to stderr so the
/// operator can investigate (typically: a worker hit a token limit, crashed,
/// or was killed manually). The runtime deliberately does **not** mutate
/// state — orphan resolution is operator-driven via `cs resume` or a manual
/// reset. Fail loud, not silently.
pub trait LivenessCheck {
    /// Return `true` if a tmux session with the given functional name is
    /// currently running. Implementations should treat "unable to tell"
    /// as `true` (optimistic) so transient failures never synthesize a
    /// false-positive orphan warning.
    fn is_session_alive(&self, session_name: &str) -> bool;
}

/// Default [`LivenessCheck`] used when none is configured — always reports
/// sessions as alive, so no orphan warnings are emitted.
///
/// Useful in tests and when the runtime is embedded in a context where tmux
/// is not the transport (e.g. a unit test that does not want to shell out).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLivenessCheck;

impl LivenessCheck for NoLivenessCheck {
    fn is_session_alive(&self, _session_name: &str) -> bool {
        true
    }
}

/// Production [`LivenessCheck`] that shells out to
/// `tmux -L <socket> has-session -t <session_name>`.
///
/// `socket` is the tmux socket name the transport was configured with (see
/// `cosmon-transport`'s `TmuxBackend`). When `None`, `tmux`'s default socket
/// is used — correct only in environments where cosmon did not override it.
#[derive(Debug, Clone)]
pub struct TmuxLivenessCheck {
    socket: Option<String>,
}

impl TmuxLivenessCheck {
    /// Build a liveness probe pinned to a specific tmux socket name.
    #[must_use]
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            socket: Some(socket.into()),
        }
    }

    /// Build a liveness probe that uses tmux's default socket.
    #[must_use]
    pub fn default_socket() -> Self {
        Self { socket: None }
    }
}

impl LivenessCheck for TmuxLivenessCheck {
    fn is_session_alive(&self, session_name: &str) -> bool {
        let mut cmd = Command::new("tmux");
        if let Some(sock) = &self.socket {
            cmd.arg("-L").arg(sock);
        }
        cmd.args(["has-session", "-t", session_name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.status() {
            Ok(status) => status.success(),
            // If we cannot even run tmux, assume alive (optimistic): better
            // to miss an orphan than cry wolf.
            Err(_) => true,
        }
    }
}

/// One detected orphan: a molecule that is `Running` in the store but whose
/// tmux session is no longer alive.
#[derive(Debug, Clone)]
pub struct Orphan {
    /// The molecule stuck in `Running`.
    pub id: MoleculeId,
    /// The session name recorded at tackle time that no longer has a live
    /// tmux session.
    pub session_name: String,
}

/// Scan a [`FleetSnapshot`] for running molecules whose tmux session is dead.
///
/// Molecules without a `session_name` are skipped — they predate functional
/// session naming and cannot be probed. The scan is a pure function over the
/// snapshot and the liveness probe; it never mutates state.
#[must_use]
pub fn orphan_scan(snapshot: &FleetSnapshot, liveness: &dyn LivenessCheck) -> Vec<Orphan> {
    snapshot
        .molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Running)
        .filter_map(|m| {
            let session = m.session_name.as_ref()?;
            if liveness.is_session_alive(session) {
                None
            } else {
                Some(Orphan {
                    id: m.id.clone(),
                    session_name: session.clone(),
                })
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Policy — the scheduling interface
// ---------------------------------------------------------------------------

/// The pluggable deliberation function of the resident runtime (ADR-016 §1).
///
/// A `Policy` maps an observed [`FleetSnapshot`] to a list of
/// [`RuntimeAction`]s to apply. The runtime calls [`Policy::next_actions`]
/// once per tick and terminates the loop when the returned vector is empty
/// (or, equivalently, contains only [`RuntimeAction::NoOp`] values and the
/// policy reports idle — see the skeleton shutdown rule on [`Runtime::run`]).
///
/// # Determinism and purity
///
/// Policies should be pure with respect to the snapshot they receive.
/// Internal state is allowed (hence `&mut self`), but side effects —
/// spawning threads, writing files, calling `cs` subprocesses — belong in
/// the runtime layer, not the policy layer.
///
/// # Object safety
///
/// The trait is intentionally object-safe so the runtime can hold a
/// `Box<dyn Policy>` without binding its type parameter to a concrete
/// implementation. The [`Runtime::new`] constructor demonstrates this.
pub trait Policy {
    /// Compute the next batch of actions given the current fleet snapshot.
    ///
    /// An empty return value means "no work left" and the runtime will
    /// gracefully shut down at the end of the tick.
    fn next_actions(&mut self, snapshot: &FleetSnapshot) -> Vec<RuntimeAction>;

    /// Return `true` if the policy wants the runtime to reload its edge
    /// set from the store before dispatching further actions. Defaults to
    /// `false` so policies without a disk-backed plan are unaffected.
    fn needs_recompile(&self) -> bool {
        false
    }

    /// Reload the policy's internal plan from the given store. Called by
    /// the runtime when [`Policy::needs_recompile`] returns `true`. Default
    /// implementation is a no-op.
    ///
    /// # Errors
    ///
    /// Implementations propagate [`CosmonError`] from the underlying store.
    fn recompile(&mut self, _store: &dyn StateStore) -> Result<(), CosmonError> {
        Ok(())
    }

    /// Periodic scope refresh (ADR-038, Limit 1).
    ///
    /// Called by the runtime every
    /// [`RuntimeConfig::sweep_orphan_descendants_every`] ticks when that
    /// option is set. Implementations should re-walk the store starting
    /// from the molecules they already track and absorb any newly-reachable
    /// descendants into their plan. The default implementation is a no-op
    /// so policies that don't need adaptive scope (including [`NoOpPolicy`])
    /// remain unaffected.
    ///
    /// # Errors
    ///
    /// Implementations propagate [`CosmonError`] from the underlying store.
    fn refresh_scope(&mut self, _store: &dyn StateStore) -> Result<(), CosmonError> {
        Ok(())
    }

    /// Return `true` if the molecule is inside this policy's scope — i.e. it
    /// belongs to the DAG the policy is responsible for walking.
    ///
    /// The runtime uses this to **scope its per-molecule side effects to the
    /// DAG the operator actually asked it to run**. A `FleetSnapshot` always
    /// carries the *whole* store, but `cs run <root>` is a connected-component
    /// walk — it must not `cs done`-merge, drain, or reset molecules that lie
    /// outside the root's closure. Without this filter the completion pass
    /// fires `cs done` on **every** completed molecule in the store (≈150 in a
    /// mature galaxy), a multi-minute subprocess storm that blocks dispatch
    /// long enough for a human to beat the runtime to the frontier and that
    /// merges branches the operator never named (the unscoped-dispatch
    /// incident). Scoping the side effects to the tracked closure
    /// restores the `cs run <root>` contract: touch the root's DAG, nothing
    /// else.
    ///
    /// The default returns `true` so policies without an explicit node
    /// universe (notably [`NoOpPolicy`] and any future whole-store policy)
    /// keep their historical "the snapshot is my scope" behaviour. DAG-shaped
    /// policies override it to gate on their tracked node set.
    fn tracks_molecule(&self, _id: &MoleculeId) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// NoOpPolicy — trivial test policy used by the sub-task 1 integration test
// ---------------------------------------------------------------------------

/// A policy that always returns an empty action list.
///
/// This exists so the runtime skeleton can be exercised end-to-end without
/// depending on `DagPolicy` (which is sub-task 2). An empty action list is
/// treated as "policy is done" by [`Runtime::run`], so a runtime built with
/// `NoOpPolicy` immediately shuts down on its first tick — which is exactly
/// what the graceful-shutdown test needs to verify.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpPolicy;

impl Policy for NoOpPolicy {
    fn next_actions(&mut self, _snapshot: &FleetSnapshot) -> Vec<RuntimeAction> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// ShutdownSignal — cooperative stop flag
// ---------------------------------------------------------------------------

/// A cooperative shutdown handle shared between the caller and the loop.
///
/// Cloning the handle is cheap (`Arc<AtomicBool>`); the loop checks
/// [`ShutdownSignal::is_tripped`] at the top of every tick and exits cleanly
/// if set. A CLI binary that wires `Ctrl-C` (`SIGINT`) to [`ShutdownSignal::trip`]
/// gives the resident runtime the "stop gracefully on signal" behavior the
/// ADR requires without pulling a signal-handling library into this crate.
#[derive(Debug, Clone, Default)]
pub struct ShutdownSignal {
    tripped: Arc<AtomicBool>,
}

impl ShutdownSignal {
    /// Create a fresh, untripped shutdown signal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the signal as tripped. The next tick of the loop will observe
    /// this and exit cleanly.
    pub fn trip(&self) {
        self.tripped.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if [`ShutdownSignal::trip`] has been called.
    #[must_use]
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// RuntimeConfig — tunables for the event loop
// ---------------------------------------------------------------------------

/// Tunables for the resident runtime event loop.
///
/// Kept intentionally small so the skeleton is understandable in one glance.
/// Future sub-tasks will grow this struct with file-watcher toggles, per-policy
/// budgets, and observability hooks.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// How long to sleep between ticks when the policy emits no actions but
    /// still wants the loop to keep running.
    ///
    /// A very small value keeps the runtime responsive in tests; a real
    /// deployment will tune this higher to reduce store churn.
    pub poll_interval: Duration,

    /// Hard upper bound on loop wall time. The loop exits with
    /// [`RunReport::reason`] set to [`ShutdownReason::Deadline`] once this
    /// elapses, regardless of policy state. Protects tests from hangs and
    /// protects operators from runaway loops.
    pub max_runtime: Option<Duration>,

    /// Re-walk the state store every N ticks to absorb descendants that
    /// were nucleated dynamically by workers (e.g. mission-controller
    /// decompose, deep-think step 4) and are not yet reachable from the
    /// policy's edge closure.
    ///
    /// When `Some(n)`, the runtime calls [`Policy::refresh_scope`] every
    /// `n` ticks. When `None` (default), scope is frozen at compile-plan
    /// time — the pre-2026-04-14 behavior. See
    /// [ADR-038](../../../docs/adr/038-runtime-adaptive-scope.md).
    pub sweep_orphan_descendants_every: Option<u32>,

    /// Re-run [`orphan_scan`] inside the runtime loop every N ticks, and
    /// reset any Running molecule whose worker session is dead back to
    /// Pending so the frontier can re-dispatch it.
    ///
    /// This closes the *phantom-workers part 2* gap
    /// (`docs/diagnostic/2026-04-25-phantom-workers-part2-invariance-review.md`):
    /// a worker tmux session that dies *after* startup is not noticed
    /// by the one-shot startup orphan scan, and is invisible to
    /// `frontier::compute_from_molecules` (filtered out by the
    /// assigned-worker test). Without an in-loop recheck, the runtime
    /// wedges in `actions empty + has_running` forever.
    ///
    /// `Some(n)` enables the recheck on every Nth tick. `None` disables
    /// it entirely (test-only behaviour). Default: `Some(10)` — at the
    /// production 1s poll, that is one recheck every ~10s.
    pub liveness_recheck_every: Option<u64>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(25),
            max_runtime: Some(Duration::from_secs(5)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: Some(10),
        }
    }
}

// ---------------------------------------------------------------------------
// RunReport — observable summary of a finished loop
// ---------------------------------------------------------------------------

/// Why the resident runtime loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// The policy returned an empty action vector — it considers itself done.
    PolicyDrained,
    /// The caller tripped the shared [`ShutdownSignal`] (Ctrl-C in a CLI).
    SignalTripped,
    /// [`RuntimeConfig::max_runtime`] elapsed before the policy drained.
    Deadline,
    /// [`RunBounds::max_actions`] (B3, the decreasing budget) was
    /// exhausted before the plan drained. This is the well-founded
    /// measure that makes an unbounded moussage total: every applied
    /// action decrements the budget, the budget never refills mid-run,
    /// and reaching the floor is a NAMED exit, never a stall (I4).
    BudgetExhausted,
    /// [`RunBounds::max_molecules`] (B2, the cardinality bound) was
    /// exceeded by the fleet snapshot — the DAG moussed wider than the
    /// binding allows. Distinct from [`Self::BudgetExhausted`] for
    /// diagnosability: a DAG that dies by width says something
    /// different from one that dies by budget.
    MoleculeQuotaExceeded,
}

/// Observable result of a single [`Runtime::run`] invocation.
///
/// Returned to the caller so tests and future CLI wrappers can assert on
/// exactly why the loop stopped, how many ticks it ran, and how many actions
/// it applied. Kept intentionally `#[non_exhaustive]` so we can grow it in
/// later sub-tasks without a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunReport {
    /// Why the loop stopped.
    pub reason: ShutdownReason,
    /// Number of ticks executed (snapshots taken) before stopping.
    pub ticks: u64,
    /// Number of [`RuntimeAction`]s the runtime applied (not counting `NoOp`).
    pub actions_applied: u64,
}

// ---------------------------------------------------------------------------
// Runtime — the event loop itself
// ---------------------------------------------------------------------------

/// Server-side drain bounds for one [`Runtime::run`] invocation
/// (the B1 moussage-resident bounds).
///
/// A worker is Turing-complete in what it decides to nucleate, so
/// "does this DAG drain completely?" is undecidable in general. The
/// only route to a *total* loop (one that always terminates) is to
/// impose bounds that make the state space finite:
///
/// - **B3 `max_actions`** — the decreasing budget. Each applied action
///   costs one unit, the floor is zero, and the floor is a NAMED exit
///   ([`ShutdownReason::BudgetExhausted`]). B3 alone suffices for
///   totality (well-founded measure).
/// - **B2 `max_molecules`** — the cardinality bound on the fleet
///   snapshot. Not required for termination; required so a drain that
///   dies by width is *diagnosable* as such
///   ([`ShutdownReason::MoleculeQuotaExceeded`]).
///
/// B1 (DAG depth) is a compile-time property of the plan, enforced by
/// the caller against [`dag_depth`] before the loop starts — a plan
/// too deep is refused, never started.
///
/// The values come from the *binding* (operator-written, server-side):
/// the client requests a drain, the server decides under which bounds.
/// `None` = unbounded — the operator-local `cs run` default, unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunBounds {
    /// B3 — maximum actions the loop may apply before exiting with
    /// [`ShutdownReason::BudgetExhausted`]. `None` = unbounded.
    pub max_actions: Option<u64>,
    /// B2 — maximum molecules tolerated in the fleet snapshot before
    /// exiting with [`ShutdownReason::MoleculeQuotaExceeded`].
    /// `None` = unbounded.
    pub max_molecules: Option<usize>,
}

/// The resident runtime: a [`StateStore`] + a [`Policy`] + an event loop.
///
/// The runtime owns trait objects rather than generic parameters so callers
/// can select store and policy implementations at run time (e.g. the future
/// `cs run <dag>` CLI will choose between `DagPolicy` and `DynamicDagPolicy`
/// based on a flag). Holding them as `Box<dyn …>` keeps the type signature
/// of the CLI layer flat.
pub struct Runtime {
    store: Box<dyn StateStore>,
    policy: Box<dyn Policy>,
    executor: Box<dyn Executor>,
    liveness: Box<dyn LivenessCheck>,
    config: RuntimeConfig,
    bounds: RunBounds,
    shutdown: ShutdownSignal,
    /// Per-tick probe invoked for every `Running` molecule the policy tracks
    /// (round-3 / F-01). The production caller (`cs run`) passes the
    /// realized-model capture so `ModelObserved` is emitted at the first
    /// model-bearing turn *during* the run — durable even if the worker later
    /// crashes before `cs complete`. `None` (the default) is a no-op; the
    /// runtime core stays I/O-free with respect to what the probe does.
    tick_probe: Option<TickProbe>,
}

/// A per-tick probe over one `Running` molecule — see [`Runtime::with_tick_probe`].
pub type TickProbe = Box<dyn FnMut(&MoleculeId)>;

impl Runtime {
    /// Build a new runtime from a store, a policy, an executor, and a config.
    ///
    /// The [`Executor`] controls how `Evolve` actions are dispatched to real
    /// workers. Use [`SubprocessExecutor`] for production (`cs tackle`) or
    /// [`NoOpExecutor`] for tests.
    ///
    /// The returned runtime is idle until [`Runtime::run`] is called.
    #[must_use]
    pub fn new(
        store: Box<dyn StateStore>,
        policy: Box<dyn Policy>,
        executor: Box<dyn Executor>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            store,
            policy,
            executor,
            liveness: Box::new(NoLivenessCheck),
            config,
            bounds: RunBounds::default(),
            shutdown: ShutdownSignal::new(),
            tick_probe: None,
        }
    }

    /// Install a per-tick probe invoked for every `Running` molecule inside
    /// the policy's scope (round-3 / F-01 — the realized-model runtime
    /// consumer). Without this setter the runtime never probes.
    #[must_use]
    pub fn with_tick_probe(mut self, probe: TickProbe) -> Self {
        self.tick_probe = Some(probe);
        self
    }

    /// Install server-side drain bounds (the B1 moussage-resident
    /// bounds). Without this setter the runtime is
    /// unbounded — the operator-local `cs run` default. Callers
    /// draining on behalf of a tenant pass the binding-derived
    /// [`RunBounds`] so the loop is total by construction (B3) and
    /// width-diagnosable (B2).
    #[must_use]
    pub fn with_run_bounds(mut self, bounds: RunBounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Install a [`LivenessCheck`] used by the startup orphan scan.
    ///
    /// Without this setter, the runtime defaults to [`NoLivenessCheck`],
    /// which never flags orphans. Production callers (`cs run`) should pass
    /// a [`TmuxLivenessCheck`] pinned to the configured tmux socket so
    /// workers that died (token limit, crash, manual kill) are surfaced
    /// as warnings on startup instead of wedging the DAG forever.
    #[must_use]
    pub fn with_liveness_check(mut self, liveness: Box<dyn LivenessCheck>) -> Self {
        self.liveness = liveness;
        self
    }

    /// Return a clone of the runtime's [`ShutdownSignal`] so callers can
    /// trip it from another thread (typically: a Ctrl-C handler).
    #[must_use]
    pub fn shutdown_handle(&self) -> ShutdownSignal {
        self.shutdown.clone()
    }

    /// Run the event loop until the policy drains, the shutdown signal is
    /// tripped, or the deadline elapses.
    ///
    /// The loop is deliberately simple:
    ///
    /// 1. Check shutdown signal and deadline.
    /// 2. Load a [`FleetSnapshot`] from the store.
    /// 3. Ask the policy for [`RuntimeAction`]s.
    /// 4. If the action list is empty → return with
    ///    [`ShutdownReason::PolicyDrained`].
    /// 5. Otherwise apply each action and sleep for
    ///    [`RuntimeConfig::poll_interval`].
    ///
    /// # Errors
    ///
    /// Propagates [`RuntimeError::State`] if any store call fails, and
    /// [`RuntimeError::Unsupported`] if the policy emits an action variant
    /// that this skeleton does not yet implement.
    #[allow(clippy::too_many_lines)]
    pub fn run(&mut self) -> Result<RunReport, RuntimeError> {
        let started = Instant::now();
        let mut ticks: u64 = 0;
        let mut actions_applied: u64 = 0;
        // Track molecules that have already been handed to `on_complete`
        // so a fast executor that completes synchronously during
        // [`Executor::dispatch`] (see the `CompletingExecutor` used in
        // the diamond integration test) still gets its post-merge
        // stamping, but only once. Without this set the runtime would
        // either miss same-tick completions entirely or re-merge every
        // tick.
        let mut completion_handled: HashSet<MoleculeId> = HashSet::new();

        // Startup orphan scan: surface Running molecules whose tmux session
        // is dead. We fail loud (stderr warning, no state mutation) so the
        // operator can decide whether to `cs resume --agent <name>` or reset
        // the molecule manually. Silently resetting to pending would discard
        // step progress and hide the underlying failure.
        let startup_snapshot = FleetSnapshot::load(self.store.as_ref())?;
        for orphan in orphan_scan(&startup_snapshot, self.liveness.as_ref()) {
            // Only warn about orphans inside the policy's DAG scope — a
            // `cs run <root>` should not spew warnings about Running
            // molecules in unrelated subgraphs (task-20260610-5297).
            if !self.policy.tracks_molecule(&orphan.id) {
                continue;
            }
            eprintln!(
                "ORPHAN WARNING: molecule {} is running but session {} is dead. \
                 Run cs resume --agent {} or reset manually.",
                orphan.id, orphan.session_name, orphan.session_name
            );
        }

        loop {
            if self.shutdown.is_tripped() {
                return Ok(RunReport {
                    reason: ShutdownReason::SignalTripped,
                    ticks,
                    actions_applied,
                });
            }
            if let Some(deadline) = self.config.max_runtime {
                if started.elapsed() >= deadline {
                    return Ok(RunReport {
                        reason: ShutdownReason::Deadline,
                        ticks,
                        actions_applied,
                    });
                }
            }

            let snapshot = FleetSnapshot::load(self.store.as_ref())?;
            ticks += 1;

            // B2 — cardinality bound (task-20260610-e5f6). A DAG that
            // mousses wider than the binding allows exits with a NAMED
            // reason instead of dispatching further. Checked on the
            // fresh snapshot so mid-run nucleations (DecayProduct
            // children absorbed by the scope sweep) are counted too.
            if let Some(max) = self.bounds.max_molecules {
                if snapshot.molecules.len() > max {
                    return Ok(RunReport {
                        reason: ShutdownReason::MoleculeQuotaExceeded,
                        ticks,
                        actions_applied,
                    });
                }
            }

            // Adaptive scope sweep (ADR-038, Limit 1): periodically
            // re-walk the store from the policy's known molecules so
            // descendants nucleated dynamically by workers (without a
            // typed-link path back to the runtime's root) are absorbed
            // into the plan. Zero-diff when the option is unset.
            if let Some(every) = self.config.sweep_orphan_descendants_every {
                if every > 0 && ticks.is_multiple_of(u64::from(every)) {
                    self.policy.refresh_scope(self.store.as_ref())?;
                }
            }

            // Realized-model runtime consumer (round-3 / F-01): probe every
            // in-scope Running molecule each tick so `ModelObserved` lands on
            // the journal at the FIRST model-bearing turn, during the run —
            // not at `cs complete`. A worker that crashes mid-run has already
            // been observed. The probe itself dedups (first + on-change), so
            // per-tick invocation is idempotent.
            if let Some(probe) = self.tick_probe.as_mut() {
                for mol in &snapshot.molecules {
                    if mol.status == MoleculeStatus::Running && self.policy.tracks_molecule(&mol.id)
                    {
                        probe(&mol.id);
                    }
                }
            }

            // Phantom-workers part 2 (task-20260425-911f): periodic
            // in-loop liveness recheck. The startup orphan scan (above
            // the loop) only catches sessions that were already dead
            // when `cs run` started. A worker that dies mid-run — for
            // any reason: claude crash, host kill, runtime tmux session
            // dropped — is invisible to every other safeguard until
            // `cs purge` is run manually:
            //   - frontier::compute_from_molecules filters out
            //     Running-with-assigned-worker, so the policy sees no
            //     candidate to dispatch.
            //   - apply_evolve's rollback only fires on synchronous
            //     dispatch errors.
            //   - cs patrol --propel / --nudge skip molecules with no
            //     last_progress_at, which is the case for workers that
            //     died seconds after spawn.
            // The recheck runs orphan_scan against the same liveness
            // probe production already injects (TmuxLivenessCheck), and
            // for every Running molecule whose tmux session is dead
            // resets the molecule to Pending (clearing assigned_worker
            // and session_name). The next tick's frontier reducer then
            // re-dispatches it via the normal path.
            // See `docs/diagnostic/2026-04-25-phantom-workers-part2-invariance-review.md`.
            //
            // The reset is best-effort: if it succeeds, we let the
            // current tick proceed with the (slightly stale) snapshot —
            // the next tick will reload and re-dispatch. Forcing an
            // intra-tick reload would only save one poll interval and
            // would complicate the loop's already-non-trivial control
            // flow. Note also that we do NOT reset the molecule for
            // every detected orphan — only those whose status we just
            // re-loaded as `Running` (the snapshot may be stale and the
            // worker may have been intentionally cleaned up by another
            // path between snapshot load and the recheck).
            let mut reset_any = false;
            if let Some(every) = self.config.liveness_recheck_every {
                if every > 0 && ticks.is_multiple_of(every) {
                    for orphan in orphan_scan(&snapshot, self.liveness.as_ref()) {
                        // Only reset orphans inside the policy's DAG scope —
                        // a `cs run <root>` must not reset to Pending a
                        // Running molecule that belongs to an unrelated
                        // subgraph (task-20260610-5297).
                        if !self.policy.tracks_molecule(&orphan.id) {
                            continue;
                        }
                        if let Ok(mut latest) = self.store.load_molecule(&orphan.id) {
                            if latest.status == MoleculeStatus::Running {
                                eprintln!(
                                    "ORPHAN RESET: molecule {} session {} died — \
                                     resetting to Pending so frontier can re-dispatch.",
                                    orphan.id, orphan.session_name
                                );
                                latest.status = MoleculeStatus::Pending;
                                latest.assigned_worker = None;
                                latest.session_name = None;
                                latest.updated_at = Utc::now();
                                if self.store.save_molecule(&orphan.id, &latest).is_ok() {
                                    reset_any = true;
                                }
                            }
                        }
                    }
                }
            }

            // Detect newly-completed molecules. Call on_complete (which
            // merges branches) BEFORE policy computes the next ready
            // frontier — so downstream workers see predecessors' output
            // in their worktree. Using `completion_handled` (an ever-
            // growing set), not "was-Running-last-tick", keeps the
            // invariant correct across fast executors that complete
            // synchronously and slow executors whose workers land the
            // completion several ticks after dispatch.
            let mut stamped_any = false;
            for mol in &snapshot.molecules {
                if mol.status != MoleculeStatus::Completed {
                    continue;
                }
                // Scope the merge to the policy's DAG closure. The snapshot
                // carries the whole store, but `cs run <root>` is a
                // connected-component walk: `cs done`-merging every completed
                // molecule in a mature galaxy (≈150 in showroom) is a
                // multi-minute subprocess storm that blocks dispatch and
                // merges branches the operator never named — the ONCUE-100
                // stall (task-20260610-5297). `tracks_molecule` defaults to
                // `true`, so whole-store policies are unaffected.
                if !self.policy.tracks_molecule(&mol.id) {
                    continue;
                }
                // A completion is either freshly-observed (was Running on
                // the previous tick) or same-tick (a fast executor
                // flipped the status inside its own `dispatch` call).
                // Either way, only call `on_complete` + stamp once.
                if completion_handled.contains(&mol.id) {
                    continue;
                }
                if self.executor.on_complete(&mol.id).is_ok() {
                    // Stamp `merged_at` once the executor reports the
                    // branch has landed. This is the single point where
                    // the merge-before-dispatch temporal invariant is
                    // promoted to a structural fact on disk (ADR-041),
                    // so the next `frontier::compute_from_molecules`
                    // pass can release dependents. Idempotent: we only
                    // set the field if it was unset.
                    if let Ok(mut latest) = self.store.load_molecule(&mol.id) {
                        if latest.merged_at.is_none() {
                            latest.merged_at = Some(Utc::now());
                            if self.store.save_molecule(&mol.id, &latest).is_ok() {
                                stamped_any = true;
                            }
                        }
                    }
                }
                completion_handled.insert(mol.id.clone());
                // If the formula requested freeze-on-last-step, transition
                // the molecule from Completed → Frozen so it stays visible
                // to the pilot rather than draining out of the DAG.
                if mol.freeze_on_last_step {
                    self.apply_freeze(&mol.id)?;
                }
            }

            // If the completion pass stamped `merged_at` on any molecule,
            // OR the liveness recheck reset an orphan to Pending, reload
            // the snapshot so the policy sees the up-to-date state when
            // it recomputes the frontier. Without this reload, the next
            // tick's frontier reducer would still see stale fields from
            // `snapshot.molecules` and either refuse to release
            // dependents (stale `merged_at = None`) or fail to surface
            // the freshly-reset Pending root (stale Running status).
            let snapshot = if stamped_any || reset_any {
                FleetSnapshot::load(self.store.as_ref())?
            } else {
                snapshot
            };

            // Drive Running molecules whose current step is native or
            // shell-gate. These don't have a dedicated worker process —
            // if we don't drain them in-process, a mixed formula
            // (claude → native → claude) stalls on the native tail
            // forever once the claude worker exits. The executor decides
            // whether any given molecule is eligible; the runtime just
            // enumerates candidates.
            for mol in &snapshot.molecules {
                if mol.status != MoleculeStatus::Running {
                    continue;
                }
                // Scope the native-tail drive to the policy's DAG closure:
                // a `cs run <root>` must not spawn `cs tackle` for a Running
                // native step that belongs to an unrelated subgraph
                // (task-20260610-5297).
                if !self.policy.tracks_molecule(&mol.id) {
                    continue;
                }
                match self.executor.drain_native_tail(mol) {
                    Ok(true) => {
                        actions_applied += 1;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("⚠ drain_native_tail {} failed: {e} (non-fatal)", mol.id);
                    }
                }
            }

            let mut actions = self.policy.next_actions(&snapshot);
            // If a splice in next_actions left the policy with a stale
            // edge set (parent→child only, missing inter-child BlockedBy
            // edges that only exist on disk), reload from the store and
            // recompute the action batch before dispatching. Without this,
            // all N spliced children look ready at once and we launch N
            // workers in parallel instead of honoring the real DAG.
            if self.policy.needs_recompile() {
                self.policy.recompile(self.store.as_ref())?;
                actions = self.policy.next_actions(&snapshot);
            }
            if actions.is_empty() {
                // Check if any molecules are still Running — if so, we must
                // wait for them to complete before declaring the plan drained.
                // Without this, the runtime exits prematurely when workers are
                // active but no NEW actions are available yet (e.g. A is running,
                // B+C are blocked-by A, nothing to dispatch right now).
                let has_running = snapshot
                    .molecules
                    .iter()
                    .any(|m| m.status == cosmon_core::molecule::MoleculeStatus::Running);
                if has_running {
                    std::thread::sleep(self.config.poll_interval);
                    continue;
                }

                // Last-chance rescue before declaring the plan drained
                // (task-20260610-808b). The policy's scope can lag the store
                // at exactly the moment the last worker completes, in two
                // ways that both end in a premature `plan drained`:
                //
                //   1. **Dynamic-DAG children.** A mission-controller (or
                //      deep-think step 4, freeze/thaw cycle, …) nucleates a
                //      child whose only typed link is a `BlockedBy` back to a
                //      tracked molecule. `compile_plan`'s forward BFS never
                //      reaches it, and when `--sweep-every` is 0 (the default)
                //      the periodic `refresh_scope` never runs. The child is
                //      runnable on disk but invisible to `self.plan.ready()`.
                //   2. **Harvest/compile race.** The last worker's completion
                //      was merged on *this* tick (`stamped_any` reload above),
                //      but the freshly-unblocked frontier was computed against
                //      a snapshot the policy had already reduced to empty.
                //
                // A single `refresh_scope` + `next_actions` closes both:
                // `refresh_scope` rescans every pending molecule in the store
                // for links into the policy's known set, recompiles the edge
                // closure, and the follow-up `next_actions` absorbs any
                // newly-known completion and surfaces its dependents. Only if
                // THIS also yields nothing is the plan truly drained. The
                // cost is bounded — it runs only at the drain decision point
                // (actions empty, nothing running), not on the hot path — and
                // the default `Policy::refresh_scope` is a no-op, so policies
                // without dynamic scope (NoOpPolicy, fixed DAGs) drain exactly
                // as before. See `docs/diagnostic/` runtime-drain notes and
                // the qfa/mission-20260610-6d98 reproduction.
                self.policy.refresh_scope(self.store.as_ref())?;
                let rescue_snapshot = FleetSnapshot::load(self.store.as_ref())?;
                actions = self.policy.next_actions(&rescue_snapshot);
                if self.policy.needs_recompile() {
                    self.policy.recompile(self.store.as_ref())?;
                    actions = self.policy.next_actions(&rescue_snapshot);
                }
                if actions.is_empty() {
                    return Ok(RunReport {
                        reason: ShutdownReason::PolicyDrained,
                        ticks,
                        actions_applied,
                    });
                }
                // Rescue surfaced runnable work — fall through to dispatch it
                // this tick rather than exiting. The next iteration will see
                // the dispatched molecule(s) as Running and keep the loop
                // alive via the `has_running` guard above.
            }

            let mut dispatched: Vec<MoleculeId> = Vec::new();
            for action in actions {
                // B3 — the decreasing budget (task-20260610-e5f6).
                // Checked BEFORE forming each side-effect so the bound
                // is exact: action `max_actions + 1` is never applied.
                // Every applied action decrements the remaining budget
                // (a well-founded, strictly decreasing measure with
                // floor 0), which is what makes an otherwise
                // undecidable moussage *total*: the loop reaches either
                // PolicyDrained or this NAMED exit in finite time.
                if let Some(max) = self.bounds.max_actions {
                    if actions_applied >= max {
                        return Ok(RunReport {
                            reason: ShutdownReason::BudgetExhausted,
                            ticks,
                            actions_applied,
                        });
                    }
                }
                match action {
                    RuntimeAction::NoOp => {}
                    RuntimeAction::Nucleate { .. } => {
                        return Err(RuntimeError::Unsupported("Nucleate"));
                    }
                    RuntimeAction::Evolve { id, evidence } => {
                        match self.apply_evolve(&id, &evidence) {
                            Ok(()) => dispatched.push(id.clone()),
                            // A transient dispatch failure (e.g. `cs tackle`
                            // lost a git/worktree race immediately after a
                            // sibling molecule merged) must NOT tear down the
                            // whole DAG runtime. `apply_evolve` already rolled
                            // the molecule back to `Pending`; log it and skip
                            // to the next action so this tick's other
                            // dispatches still land and the *next* tick retries
                            // this one — exactly the recovery the rollback in
                            // `apply_evolve` was written for. Before this guard
                            // a single blip propagated through `?`, exited
                            // `run()`, and orphaned every downstream child as
                            // "pending, never auto-tackled" (the gridgame
                            // 2026-05-02 cs-run-policy=dag incident). Store-level
                            // (`State`) and `Unsupported` errors stay fatal.
                            Err(e @ RuntimeError::Dispatch { .. }) => {
                                eprintln!(
                                    "⚠ {e} — rolled back to Pending, retrying next tick (non-fatal)"
                                );
                                continue;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    RuntimeAction::Complete { id, reason } => {
                        self.apply_complete(&id, &reason)?;
                    }
                    RuntimeAction::Collapse { id, reason } => {
                        self.apply_collapse(&id, &reason)?;
                    }
                }
                actions_applied += 1;
            }

            // Check dispatched molecules that may have completed within the
            // same tick (fast/auto-completing executors). If they have
            // freeze_on_last_step, transition Completed → Frozen now.
            for id in &dispatched {
                if let Ok(mol) = self.store.load_molecule(id) {
                    if mol.status == MoleculeStatus::Completed && mol.freeze_on_last_step {
                        self.apply_freeze(id)?;
                    }
                }
            }

            std::thread::sleep(self.config.poll_interval);
        }
    }

    // -- action applicators -------------------------------------------------

    /// Apply an `Evolve` action: transition a molecule to `Running` and
    /// dispatch it to a worker via the [`Executor`].
    ///
    /// The runtime transitions the molecule to `Running` in the store, then
    /// calls [`Executor::dispatch`] to hand off actual work (e.g. `cs tackle`).
    /// The molecule stays in `Running` until the worker completes it via
    /// `cs complete` — the runtime does not synthetic-complete molecules.
    ///
    /// Idempotent: molecules already in `Running`, `Completed`, or `Collapsed`
    /// are silently skipped.
    fn apply_evolve(&self, id: &MoleculeId, _evidence: &str) -> Result<(), RuntimeError> {
        // L1 contract: disk is truth. This `load_molecule` is the
        // pre-dispatch re-read that closes the convoy-cascade-class race —
        // the `FleetSnapshot` the policy reasoned over is up to one
        // poll-interval stale, but this read is fresh. Anything that
        // flipped out of `Pending` since the last poll (e.g. a human's
        // `cs tackle` landed first) is skipped below.
        let mut mol = self.store.load_molecule(id)?;
        if mol.status == MoleculeStatus::Running
            || mol.status == MoleculeStatus::Completed
            || mol.status == MoleculeStatus::Collapsed
        {
            return Ok(()); // idempotent
        }
        // Anti-preemption lease (task-20260531-a12f / delib-20260531-c761):
        // "manual always wins". A molecule a human manually tackled carries
        // a sticky `tackled_by == human` claim. The runtime must NEVER
        // raffle it — even if it briefly returned to `Pending` on a
        // revision. This is a binary owner field, not a tunable N-second
        // cooldown: no clock, no window to calibrate.
        if mol.is_human_claimed() {
            return Ok(()); // human claim — runtime does not preempt
        }
        mol.status = MoleculeStatus::Running;
        // Stamp the runtime's own dispatch claim. A `runtime:<pid>` claim is
        // NOT sticky (unlike `human`), so a later tick may freely re-dispatch
        // this molecule if it falls back to `Pending`. The subsequent
        // `cs tackle --by runtime:<pid>` (Executor::dispatch) records the
        // same actor class — the two writers agree. `mark_tackled` also
        // bumps `updated_at`.
        mol.mark_tackled(cosmon_core::tackle::TackledBy::runtime(std::process::id()));
        self.store.save_molecule(&mol.id.clone(), &mol)?;
        if let Err(e) = self.executor.dispatch(id) {
            // Rollback: the molecule was flipped to Running optimistically
            // before dispatch. If dispatch fails, the molecule is stranded
            // in Running with no worker — the DAG stalls. Reset to Pending
            // so the next tick re-evaluates and retries. The save error is
            // swallowed because the dispatch failure is the primary signal.
            mol.status = MoleculeStatus::Pending;
            mol.updated_at = Utc::now();
            let _ = self.store.save_molecule(&mol.id.clone(), &mol);
            return Err(e);
        }
        Ok(())
    }

    /// Apply a `Complete` action: transition to `Completed` (idempotent).
    fn apply_complete(&self, id: &MoleculeId, _reason: &str) -> Result<(), RuntimeError> {
        let mut mol = self.store.load_molecule(id)?;
        if mol.status == MoleculeStatus::Completed {
            return Ok(());
        }
        mol.status = MoleculeStatus::Completed;
        mol.current_step = mol.total_steps;
        mol.updated_at = Utc::now();
        self.store.save_molecule(&mol.id.clone(), &mol)?;
        Ok(())
    }

    /// Apply a freeze transition: `Completed` → `Frozen`.
    ///
    /// Called when a molecule's formula has `freeze_on_last_step = true`.
    /// The molecule stays visible to the pilot (frozen, not drained) so
    /// it can be inspected before teardown.
    fn apply_freeze(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        let mut mol = self.store.load_molecule(id)?;
        if mol.status != MoleculeStatus::Completed {
            return Ok(()); // idempotent — only freeze from Completed
        }
        mol.status = MoleculeStatus::Frozen;
        mol.updated_at = Utc::now();
        self.store.save_molecule(&mol.id.clone(), &mol)?;
        Ok(())
    }

    /// Apply a `Collapse` action: transition to `Collapsed`.
    fn apply_collapse(&self, id: &MoleculeId, reason: &str) -> Result<(), RuntimeError> {
        let mut mol = self.store.load_molecule(id)?;
        if mol.status == MoleculeStatus::Collapsed {
            return Ok(());
        }
        mol.status = MoleculeStatus::Collapsed;
        mol.collapse_reason = Some(reason.to_owned());
        mol.updated_at = Utc::now();
        self.store.save_molecule(&mol.id.clone(), &mol)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Prove `Policy` is object-safe — the runtime requires a trait object.
    #[test]
    fn policy_is_object_safe() {
        fn accepts_dyn(_policy: &dyn Policy) {}
        let mut policy = NoOpPolicy;
        accepts_dyn(&policy);
        // Call through the trait object to exercise the vtable.
        let snap = FleetSnapshot {
            molecules: Vec::new(),
        };
        let boxed: Box<dyn Policy> = Box::new(NoOpPolicy);
        assert!(boxed_next_actions(boxed, &snap).is_empty());
        assert!(policy.next_actions(&snap).is_empty());
    }

    fn boxed_next_actions(mut p: Box<dyn Policy>, s: &FleetSnapshot) -> Vec<RuntimeAction> {
        p.next_actions(s)
    }

    #[test]
    fn shutdown_signal_round_trip() {
        let signal = ShutdownSignal::new();
        assert!(!signal.is_tripped());
        let clone = signal.clone();
        clone.trip();
        assert!(signal.is_tripped());
    }

    #[test]
    fn fleet_snapshot_is_empty_on_empty_molecule_list() {
        let snap = FleetSnapshot {
            molecules: Vec::new(),
        };
        assert!(snap.is_empty());
    }

    struct DeadSessionCheck;
    impl LivenessCheck for DeadSessionCheck {
        fn is_session_alive(&self, _session_name: &str) -> bool {
            false
        }
    }

    struct AliveSessionCheck;
    impl LivenessCheck for AliveSessionCheck {
        fn is_session_alive(&self, _session_name: &str) -> bool {
            true
        }
    }

    fn running_mol_with_session(id: &str, session: Option<&str>) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).expect("id"),
            fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet"),
            formula_id: FormulaId::new("task-work").expect("formula"),
            status: MoleculeStatus::Running,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 1,
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
            session_name: session.map(str::to_owned),
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

    #[test]
    fn orphan_scan_detects_dead_session() {
        let snap = FleetSnapshot {
            molecules: vec![running_mol_with_session(
                "task-20260412-0001",
                Some("dead-sess"),
            )],
        };
        let orphans = orphan_scan(&snap, &DeadSessionCheck);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].session_name, "dead-sess");
    }

    #[test]
    fn orphan_scan_skips_alive_sessions() {
        let snap = FleetSnapshot {
            molecules: vec![running_mol_with_session(
                "task-20260412-0001",
                Some("live-sess"),
            )],
        };
        assert!(orphan_scan(&snap, &AliveSessionCheck).is_empty());
    }

    #[test]
    fn orphan_scan_skips_molecules_without_session_name() {
        let snap = FleetSnapshot {
            molecules: vec![running_mol_with_session("task-20260412-0001", None)],
        };
        assert!(orphan_scan(&snap, &DeadSessionCheck).is_empty());
    }

    #[test]
    fn orphan_scan_ignores_non_running_molecules() {
        let mut mol = running_mol_with_session("task-20260412-0001", Some("sess"));
        mol.status = MoleculeStatus::Pending;
        let snap = FleetSnapshot {
            molecules: vec![mol],
        };
        assert!(orphan_scan(&snap, &DeadSessionCheck).is_empty());
    }

    #[test]
    fn no_liveness_check_reports_alive() {
        assert!(NoLivenessCheck.is_session_alive("whatever"));
    }

    /// Executor that records `drain_native_tail` calls so tests can assert
    /// the runtime re-enters a molecule stalled on a native/gate step.
    #[derive(Default)]
    struct RecordingExecutor {
        dispatched: std::sync::Mutex<Vec<MoleculeId>>,
        drained: std::sync::Mutex<Vec<MoleculeId>>,
    }

    impl Executor for RecordingExecutor {
        fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
            self.dispatched.lock().unwrap().push(id.clone());
            Ok(())
        }
        fn drain_native_tail(&self, mol: &MoleculeData) -> Result<bool, RuntimeError> {
            // Only report "drained" for molecules whose current step index
            // is odd — simulating a mixed formula where even steps are
            // claude (worker-owned) and odd steps are native.
            if mol.status == MoleculeStatus::Running && mol.current_step % 2 == 1 {
                self.drained.lock().unwrap().push(mol.id.clone());
                return Ok(true);
            }
            Ok(false)
        }
    }

    /// Mixed-mode DAG: a Running molecule whose current step is a native
    /// tail (odd index in the recorder's convention) must be driven by
    /// `drain_native_tail` every tick until it completes. Before the
    /// task-3733 fix, the runtime only dispatched `Pending` molecules and
    /// this scenario would stall forever.
    #[test]
    #[allow(clippy::items_after_statements)]
    fn test_run_drains_running_molecule_with_native_tail() {
        use cosmon_filestore::FileStore;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

        // Molecule is already Running, on step 1 (native in our convention).
        let mol = MoleculeData {
            id: MoleculeId::new("task-20260414-aaaa").unwrap(),
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 1,
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
        };
        store.save_molecule(&mol.id.clone(), &mol).unwrap();

        let executor = std::sync::Arc::new(RecordingExecutor::default());
        struct Proxy(std::sync::Arc<RecordingExecutor>);
        impl Executor for Proxy {
            fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
                self.0.dispatch(id)
            }
            fn drain_native_tail(&self, m: &MoleculeData) -> Result<bool, RuntimeError> {
                self.0.drain_native_tail(m)
            }
        }

        let config = RuntimeConfig {
            poll_interval: Duration::from_millis(5),
            max_runtime: Some(Duration::from_millis(50)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        };
        let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
        let mut runtime = Runtime::new(
            store_box,
            Box::new(NoOpPolicy),
            Box::new(Proxy(executor.clone())),
            config,
        );
        let _ = runtime.run().unwrap();

        let drained = executor.drained.lock().unwrap();
        assert!(
            !drained.is_empty(),
            "runtime must drain native tail of Running molecules"
        );
        assert_eq!(drained[0].as_str(), "task-20260414-aaaa");
    }

    #[test]
    fn noop_policy_returns_empty_vector() {
        let mut policy = NoOpPolicy;
        let snap = FleetSnapshot {
            molecules: Vec::new(),
        };
        assert!(policy.next_actions(&snap).is_empty());
    }

    /// Regression for the gridgame 2026-05-02 `cs run --policy=dag` incident:
    /// a single transient `cs tackle` failure must NOT tear down the whole
    /// DAG runtime, and the rolled-back molecule must be retried — not
    /// orphaned as "pending, never auto-tackled".
    ///
    /// Before the fix two defects compounded: `apply_evolve` propagated the
    /// dispatch error through `?`, exiting `run()` (fatal), and even had it
    /// survived, `DagPolicy` leaked the molecule in the plan's one-way
    /// `running` set so `ready()` never re-surfaced it. The executor here
    /// fails its first dispatch then succeeds; the runtime must reach the
    /// deadline (Ok), retry, and leave the molecule `Running`.
    #[test]
    #[allow(clippy::items_after_statements)]
    fn test_transient_dispatch_failure_is_non_fatal_and_retries() {
        use cosmon_filestore::FileStore;
        use std::sync::atomic::AtomicUsize;

        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

        let id = MoleculeId::new("task-20260502-flak").unwrap();
        let mut mol = pending_mol(id.as_str());
        mol.status = MoleculeStatus::Pending;
        store.save_molecule(&mol.id.clone(), &mol).unwrap();

        // Fails the first dispatch (simulating a git/worktree race right
        // after a sibling merge), then records subsequent successes.
        #[derive(Default)]
        struct FlakyExecutor {
            attempts: AtomicUsize,
            succeeded: std::sync::Mutex<Vec<MoleculeId>>,
        }
        impl Executor for FlakyExecutor {
            fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
                let n = self.attempts.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    return Err(RuntimeError::Dispatch {
                        id: id.clone(),
                        reason: "simulated transient git/worktree race".into(),
                    });
                }
                self.succeeded.lock().unwrap().push(id.clone());
                Ok(())
            }
        }

        let executor = std::sync::Arc::new(FlakyExecutor::default());
        struct Proxy(std::sync::Arc<FlakyExecutor>);
        impl Executor for Proxy {
            fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
                self.0.dispatch(id)
            }
        }

        let (plan, edges) = compile_plan(&store, std::slice::from_ref(&id)).unwrap();
        let config = RuntimeConfig {
            poll_interval: Duration::from_millis(5),
            max_runtime: Some(Duration::from_millis(120)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        };
        let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
        let mut runtime = Runtime::new(
            store_box,
            Box::new(DagPolicy::new(plan, edges)),
            Box::new(Proxy(executor.clone())),
            config,
        );

        let report = runtime
            .run()
            .expect("a transient dispatch failure must not kill the runtime");
        assert_eq!(report.reason, ShutdownReason::Deadline);

        assert!(
            executor.attempts.load(Ordering::SeqCst) >= 2,
            "runtime must retry after a transient dispatch failure"
        );
        let ok = executor.succeeded.lock().unwrap();
        assert_eq!(ok.len(), 1, "molecule dispatched exactly once after retry");
        assert_eq!(ok[0].as_str(), "task-20260502-flak");

        let reloaded = store.load_molecule(&id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Running,
            "molecule must end Running after the successful retry, not stranded Pending"
        );
    }

    /// Minimal `Pending` molecule for runtime tests.
    fn pending_mol(id: &str) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 1,
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
}
