// SPDX-License-Identifier: AGPL-3.0-only

//! Hexagonal persistence ports for Cosmon.
//!
//! Defines the `StateStore` and `EnergyTracker` traits (ports) and their
//! associated data types. File-based adapters are provided in submodules.

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::energy::{BudgetPeriod, EnergyBudget, EnergyRecord, EnergyReport};
use cosmon_core::error::CosmonError;
use cosmon_core::expiry::ExpiryPolicy;
use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, ProjectId, StepId, WorkerId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::molecule_class::MoleculeClass;
use cosmon_core::tag::Tag;
use cosmon_core::worker::{derive_worker_role, DesiredState, WorkerRole, WorkerStatus};

pub mod archive;
pub mod attestor_log;
pub mod avatar;
pub mod briefing_seal;
pub mod event_log;
pub mod events;
pub mod file_energy_tracker;
pub mod frontier;
pub mod instrumentation;
pub mod ops;
pub mod rebuild;
pub mod token_meter;
pub mod wait;

pub use briefing_seal::BriefingSeal;
pub use frontier::{Frontier, FRONTIER_SCHEMA_VERSION};
pub use rebuild::{
    project_molecules_from_events, rebuild_all_missing, rebuild_molecule_state, RebuildOutcome,
};

/// Schema version for the archive subsystem.
///
/// Bumped on breaking changes to the on-disk layout of
/// `.cosmon/state/archive/`. Writers stamp this into archive manifests;
/// readers use it to dispatch to the correct parser. Stays at `"1"` while
/// archive writing is only plumbing.
pub const SCHEMA_VERSION: &str = "1";

// ---------------------------------------------------------------------------
// Fleet — top-level state snapshot
// ---------------------------------------------------------------------------

/// A snapshot of the entire fleet state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Fleet {
    pub workers: HashMap<WorkerId, WorkerData>,
    pub repos: HashMap<String, RepoData>,
    /// Maximum number of alive (non-terminal) molecules allowed.
    ///
    /// The Attention Conservation Law (THESIS Part XVII): new nucleation
    /// is warned/rejected when the count of alive molecules reaches this limit.
    /// `None` means unlimited (default for backward compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_budget: Option<usize>,
}

impl Fleet {
    /// Create a new empty fleet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile `worker_role` for every worker using the id-prefix /
    /// [`AgentRole::Runtime`] heuristic, leaving entries that already look
    /// consistent untouched.
    ///
    /// Legacy `fleet.json` files predate the explicit `worker_role` field;
    /// `default_worker_role` returns `Cognition` for those entries, so we
    /// upgrade them here. Entries saved by a new writer already carry the
    /// correct role but running the heuristic a second time is idempotent
    /// (it only promotes Cognition → Runtime when the derivation demands
    /// it; it never demotes Runtime → Cognition, protecting operators who
    /// deliberately labelled a worker).
    pub fn reconcile_worker_roles(&mut self) {
        for worker in self.workers.values_mut() {
            if worker.worker_role == WorkerRole::Cognition {
                let derived = derive_worker_role(worker.role, worker.id.as_str());
                if derived != WorkerRole::Cognition {
                    worker.worker_role = derived;
                }
            }
        }
    }
}

/// Persisted data for a single worker.
///
/// Optional preemption fields (`preempted_by`, `frozen_molecule`, `frozen_at`)
/// support Slurm-style freeze/thaw: when a high-clearance worker needs
/// resources held by a low-clearance worker, the incumbent is frozen and
/// later thawed. These fields are `None` for normal workers and only populated
/// during a preemption cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkerData {
    pub id: WorkerId,
    pub agent_id: AgentId,
    pub role: AgentRole,
    pub clearance: Clearance,
    pub status: WorkerStatus,
    /// Runtime-vs-cognition discriminator (ADR-039 phase 1).
    ///
    /// Serialised as `worker_role` so legacy deserialisers that don't know
    /// about the field keep ignoring the `role: AgentRole` semantics. When
    /// absent in JSON (legacy file), [`derive_worker_role`] reconstructs
    /// the bit from [`AgentRole`] + worker-id prefix, and writers stamp the
    /// field explicitly thereafter.
    #[serde(default = "default_worker_role")]
    pub worker_role: WorkerRole,
    /// What the operator intends for this worker (desired/observed split).
    ///
    /// New field — defaults to [`DesiredState::Running`] for backward
    /// compatibility with fleet.json files that predate the split.
    #[serde(default = "default_desired")]
    pub desired: DesiredState,
    /// Working directory for this worker, relative to the project root.
    ///
    /// Like git, cosmon stores paths relative to the project root for
    /// portability. Legacy fleet.json files may contain absolute paths —
    /// consumers should use `cosmon_filestore::resolve_repo_path` to handle
    /// both cases.
    pub repo: Option<String>,
    pub current_molecule: Option<MoleculeId>,
    pub updated_at: DateTime<Utc>,
    /// The worker that preempted this one (set when frozen by a higher-clearance worker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preempted_by: Option<WorkerId>,
    /// The molecule this worker was processing when frozen (for resume on thaw).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_molecule: Option<MoleculeId>,
    /// When this worker was frozen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_at: Option<DateTime<Utc>>,
    /// Number of times this worker has been respawned by patrol.
    /// Circuit-breaker: patrol stops respawning when this exceeds `MAX_RESTARTS`.
    #[serde(default)]
    pub restart_count: u32,
}

/// Default desired state for backward-compatible deserialization.
fn default_desired() -> DesiredState {
    DesiredState::Running
}

/// Default `worker_role` for legacy fleet.json files that predate the field.
///
/// The eventual value is overridden by [`Fleet::reconcile_worker_roles`]
/// right after load, which applies the id-prefix / `AgentRole::Runtime`
/// heuristic; we return `Cognition` here so serde has a concrete default.
fn default_worker_role() -> WorkerRole {
    WorkerRole::Cognition
}

impl WorkerData {
    /// Create a new `WorkerData` with required fields. Optional fields default to `None`.
    ///
    /// `worker_role` is derived from `(role, id)` via [`derive_worker_role`]
    /// — callers only need to set the discriminator explicitly when they
    /// want to override the default (e.g. registering a runtime under a
    /// non-`runtime-` prefix).
    #[must_use]
    pub fn new(
        id: WorkerId,
        agent_id: AgentId,
        role: AgentRole,
        clearance: Clearance,
        status: WorkerStatus,
    ) -> Self {
        let worker_role = derive_worker_role(role, id.as_str());
        Self {
            id,
            agent_id,
            role,
            clearance,
            status,
            worker_role,
            desired: DesiredState::Running,
            repo: None,
            current_molecule: None,
            updated_at: Utc::now(),
            preempted_by: None,
            frozen_molecule: None,
            frozen_at: None,
            restart_count: 0,
        }
    }

    /// Override the `worker_role` discriminator (builder pattern).
    #[must_use]
    pub fn with_worker_role(mut self, role: WorkerRole) -> Self {
        self.worker_role = role;
        self
    }

    /// Set the repo field (builder pattern).
    #[must_use]
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// Set the current molecule (builder pattern).
    #[must_use]
    pub fn with_molecule(mut self, mol: MoleculeId) -> Self {
        self.current_molecule = Some(mol);
        self
    }

    /// Set the frozen molecule (builder pattern).
    #[must_use]
    pub fn with_frozen_molecule(mut self, mol: MoleculeId) -> Self {
        self.frozen_molecule = Some(mol);
        self
    }

    /// Set the `preempted_by` field (builder pattern).
    #[must_use]
    pub fn with_preempted_by(mut self, by: WorkerId) -> Self {
        self.preempted_by = Some(by);
        self
    }

    /// Set `frozen_at` (builder pattern).
    #[must_use]
    pub fn with_frozen_at(mut self, at: DateTime<Utc>) -> Self {
        self.frozen_at = Some(at);
        self
    }
}

/// Persisted data for a repository managed by the fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoData {
    pub name: String,
    /// Filesystem path, relative to the project root for portability.
    pub path: String,
}

// ---------------------------------------------------------------------------
// MoleculeData — flat, serializable molecule representation
// ---------------------------------------------------------------------------

/// Flat, serializable representation of a molecule for persistence.
///
/// Unlike `Molecule<S>` (which uses typestate), this struct captures all
/// molecule fields in a single type suitable for storage and retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoleculeData {
    pub id: MoleculeId,
    /// The fleet this molecule belongs to.
    ///
    /// Required since ADR-013. Backward compat: defaults to "default" for
    /// state files predating fleet-scoped molecules.
    #[serde(default = "default_fleet_id")]
    pub fleet_id: FleetId,
    pub formula_id: FormulaId,
    pub status: MoleculeStatus,
    pub variables: HashMap<String, String>,
    pub assigned_worker: Option<WorkerId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_steps: usize,
    pub current_step: usize,
    pub completed_steps: Vec<StepId>,
    pub collapse_reason: Option<String>,
    /// Structured cause attribution for the collapse (ADR-062 minimum
    /// hook). Set by `cs collapse --cause`. `None` for legacy molecules
    /// and for collapses that pre-date the field — matches the legacy
    /// "free-form `reason` only" behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_cause: Option<cosmon_core::molecule::CollapseCause>,
    /// Operator-facing collapse classification picked at `cs collapse --kind`.
    ///
    /// Distinct from `collapse_cause`: this is the
    /// failure-shape (`worker_crashed`, `gate_failed`, `blocker_stuck`,
    /// `manual_abort`, `resource_exhausted`, `Other(String)`) the operator
    /// or runtime tagged the collapse with so `cs errors` can aggregate
    /// over it without re-parsing free-form prose. `None` for legacy
    /// molecules — `cs errors` falls them into `CollapseReason::Other`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_reason_kind: Option<cosmon_core::event_v2::CollapseReason>,
    pub collapsed_step: Option<usize>,
    pub links: Vec<String>,
    /// The cognitive nature of this molecule (idea, task, decision, issue, signal).
    ///
    /// `None` for legacy molecules — treated as `Task` by convention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MoleculeKind>,
    /// Operational class of this molecule (ADR-085).
    ///
    /// Orthogonal to `kind`: a `Deliberation` may be a
    /// tactical exploration ([`MoleculeClass::Standard`]) or a
    /// stress-test of a pre-committed prior ([`MoleculeClass::StressTest`]).
    /// Stress-test molecules opt out of autopilot drain and require the
    /// two-layer seal at dispatch.
    ///
    /// Defaults to [`MoleculeClass::Standard`] for legacy molecules and
    /// nucleations that do not pass `--class`. Skipped from serialisation
    /// when standard so legacy state files remain byte-identical.
    #[serde(default, skip_serializing_if = "is_standard_class")]
    pub class: MoleculeClass,
    /// Typed links to other molecules (decay, merge, transform relationships).
    ///
    /// Coexists with `links: Vec<String>` during migration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub typed_links: Vec<MoleculeLink>,
    /// The project this molecule belongs to.
    ///
    /// Stamped at nucleation from `.cosmon/config.toml`'s `[project]` section.
    /// `None` for legacy molecules that predate project-scoped ensembles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// The agent role requested for execution of this molecule.
    ///
    /// When set, `cs tackle` uses this role instead of the default
    /// `Implementation` when registering the worker. `None` for legacy
    /// molecules or when no specific role is requested — defaults to
    /// `Implementation` at tackle time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_role: Option<AgentRole>,
    /// The tmux session name chosen at tackle time.
    ///
    /// When `Some`, this is the functional name (slug + short ID) that
    /// identifies the worker's tmux session. `cs done` and `cs purge`
    /// look it up here instead of deriving it from the molecule ID — so
    /// renames stay in lockstep with teardown. `None` for legacy
    /// molecules that predate functional session names; callers fall
    /// back to the molecule ID in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Typed labels attached to this molecule.
    ///
    /// Stored as a `BTreeSet` so tags are deduplicated and serialize in
    /// deterministic order. Legacy state files without this field
    /// deserialize to the empty set.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<Tag>,
    /// Escalation audit trail for mechanical-first cognitive escalation.
    ///
    /// Appended by `cs done` when a merge conflict triggers auto-propel.
    /// Each entry records the retry number, outcome, and timestamp.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<EscalationEntry>,
    /// When `true`, the runtime transitions this molecule to `Frozen` after
    /// it completes the last step instead of leaving it in `Completed`.
    ///
    /// Stamped at nucleation from the formula's `freeze_on_last_step` field.
    /// Defaults to `false` for backward compatibility.
    #[serde(default, skip_serializing_if = "is_false")]
    pub freeze_on_last_step: bool,
    /// Absolute UTC deadline past which this molecule is "expired" (ADR-029).
    ///
    /// `None` means no TTL — indefinite retention, today's default.
    /// Evaluated by `cs expire` / `cs patrol --expire` against the wall
    /// clock; an expired molecule triggers `expiry_policy`.
    /// Legacy state files without this field deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// What happens when `expires_at` is in the past (ADR-029).
    ///
    /// `None` means inherit the per-kind default from `.cosmon/config.toml`;
    /// if no default is configured, evaluation behaves as
    /// [`ExpiryPolicy::Warn`]. Legacy state files without this field
    /// deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_policy: Option<ExpiryPolicy>,
    /// The git branch this molecule was tackled on.
    ///
    /// Stamped by `cs tackle` when the worktree is created. Enables orphan
    /// detection: if a molecule is stuck in `Running` but its branch no
    /// longer exists, `temp-review` can flag it as orphaned. `None` for
    /// legacy molecules that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originating_branch: Option<String>,
    /// Durable intent record for an in-flight `cs evolve` transition.
    ///
    /// Written to state.json BEFORE artifact writes (log.md, briefing.md,
    /// and — later — per-step git commits) and cleared AFTER those
    /// receipts land. If a crash interrupts the transition, replay can
    /// inspect this field to distinguish three cases:
    ///
    /// 1. `None` — no transition in flight, normal operation.
    /// 2. `Some(p)` with `p.target_step == current_step` — intent written
    ///    and state advanced but artifact/commit receipts not yet
    ///    completed; replay should finish them.
    /// 3. `Some(p)` with `p.target_step != current_step` — intent stale,
    ///    likely from a prior crashed run; replay clears it.
    ///
    /// This is godel's intent+receipt pattern (ADR-036). Legacy state
    /// files without this field deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_step: Option<PendingStep>,
    /// Wall-clock time at which this molecule's feature branch was
    /// successfully merged back onto the base branch by `cs done`.
    ///
    /// `None` means the branch has not (yet) been merged — either because
    /// the molecule is still in flight, because merge failed, or because
    /// the molecule predates this field (legacy state files deserialize to
    /// `None`).
    ///
    /// This field is the **structural** half of merge-before-dispatch:
    /// together with [`MoleculeStatus::Completed`], it forms the atomic
    /// "ready-to-release" fact that [`crate::frontier::Frontier`] reads
    /// when deciding whether a dependent molecule may dispatch. Before
    /// this field existed, the scheduler had to trust temporal ordering
    /// in the runtime loop — see ADR-041 for the rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<DateTime<Utc>>,
    /// Soft-contract seal captured when `prompt.md` was first written at
    /// nucleation time. `None` for legacy molecules that predate the
    /// feature — `cs verify` treats the absence as "inconclusive", not
    /// "tampered".
    ///
    /// See [`BriefingSeal`] for the full rationale. In short: the seal
    /// is a trace that lets retrospective tools detect if `prompt.md`
    /// changed after nucleation, without imposing any lock on the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_seal: Option<BriefingSeal>,
    /// Append-only log of briefing seals, one per successful step advance.
    ///
    /// `cs evolve` appends a [`BriefingSeal`] after regenerating
    /// `briefing.md` so a later `cs verify` can detect post-advance edits
    /// to the briefing that would silently reshape the agent's contract.
    /// Defaults to the empty vector for backward compatibility — legacy
    /// state files load fine and are treated as "no seals recorded".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub briefing_seals: Vec<BriefingSeal>,
    /// Append-only log of bootstrap-context seals, one per successful
    /// step advance.
    ///
    /// `cs evolve` runs the agent-harness bootstrap walk
    /// (`AGENTS.md` + `CLAUDE.md` from `work_dir` up to the enclosing
    /// `.git/`) and stamps a [`BriefingSeal`] over the concatenated,
    /// fenced bootstrap-context bytes. A later `cs verify` can detect a
    /// post-advance edit to any of those files — the cross-worktree
    /// poisoning surface named in the audit.
    ///
    /// Defaults to the empty vector for backward compatibility — legacy
    /// state files (and molecules dispatched outside a git-rooted tree
    /// where the bootstrap walk legitimately produces no content)
    /// deserialize fine and read as "no bootstrap seals recorded".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_seals: Vec<BriefingSeal>,
    /// `true` once a terminal archive entry has been written for this
    /// molecule (ADR-030 M3).
    ///
    /// Terminal-transition commands (`cs done`, `cs collapse`, `cs freeze`,
    /// `cs stuck`) call the archive writer under `.cosmon/state/archive/`
    /// and, on success, set this flag. Re-running the same terminal verb
    /// reads the flag first and skips the archive write — the idempotence
    /// gate required by ADR-030 Consequences (running `cs done` twice
    /// must equal once).
    ///
    /// Defaults to `false` for backward compatibility: legacy state files
    /// predating M3 deserialize as un-archived and are re-archived on the
    /// next terminal transition.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
    /// Wall-clock timestamp of the most recent observable forward motion
    /// on this molecule — set by `cs evolve` on each step completion and
    /// (optionally) bumped by `cs heartbeat --molecule` while a worker is
    /// in-flight inside a step.
    ///
    /// Distinct from [`Self::updated_at`], which tracks any field mutation
    /// (including bookkeeping like seal appends and `pending_step` clears).
    /// `last_progress_at` is the inference-stall signal: if the worker is
    /// `Running` but this timestamp has not advanced for a long time,
    /// `cs peek` can derive a `Stalled` health state without introspecting
    /// tmux. Legacy state files without this field deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<DateTime<Utc>>,
    /// Wall-clock timestamp of the most recent durable work product: a file
    /// write, commit, or completed step. Unlike [`Self::last_progress_at`],
    /// this is never advanced by a liveness heartbeat. It lets the health
    /// Witness distinguish a live worker that is merely thinking from one
    /// that has recently produced observable output.
    ///
    /// Legacy state files deserialize to `None`; readers use `tackled_at` as
    /// the conservative start of the no-output window in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_at: Option<DateTime<Utc>>,
    /// Number of times `cs patrol --nudge` has poked this molecule.
    /// Incremented monotonically each time the
    /// patrol surfaces a stalled worker via `tmux send-keys` so a later
    /// post-mortem can answer "how many nudges before a recovery?".
    /// Skipped from serialisation when zero — legacy state files load
    /// transparently and report no nudges.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub nudge_count: u32,
    /// Wall-clock of the most recent `cs patrol --nudge` poke.
    /// Companion to [`Self::nudge_count`] —
    /// the patrol uses this timestamp to enforce idempotence
    /// (no two nudges within 60 seconds; cheap nudges, but not
    /// duplicate ones). `None` for legacy molecules and rows that
    /// have never been nudged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_nudged_at: Option<DateTime<Utc>>,
    /// Propulsion nudges (`cs patrol --propel`) delivered for the *current*
    /// stall. Distinct from [`Self::nudge_count`], which is the lifetime
    /// counter of the separate `--nudge` sweep: this one is a working register
    /// that patrol **resets to zero the moment the molecule makes progress**,
    /// because it drives the exponential backoff and a lifetime total would
    /// space out the nudges of an unrelated later stall.
    ///
    /// Read by [`cosmon_core::propel::decide_propel`] as `attempts`; once it
    /// reaches [`cosmon_core::propel::PROPEL_MAX_ATTEMPTS`] patrol stops
    /// repeating itself and escalates. Skipped from serialisation when zero.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub propel_count: u32,
    /// Wall-clock of the most recent propulsion nudge, the anchor for the
    /// backoff window. `None` when this stall has not been propelled.
    ///
    /// Patrol writes this **without touching [`Self::updated_at`]**: a nudge is
    /// bookkeeping about the worker, not progress by it, and advancing
    /// `updated_at` here would make every propelled molecule look fresh and
    /// silently disqualify it from the next sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_propelled_at: Option<DateTime<Utc>>,
    /// **Inline live-process record (the `Worker`-into-`Molecule` fold-in).**
    ///
    /// The single authoritative slot for "is a worker bound to this
    /// molecule, and where". Replaces the trio of back-pointers
    /// ([`Self::assigned_worker`] + [`Self::session_name`] +
    /// `WorkerData::current_molecule`) that previously had to be kept
    /// in sync by hand. Disagreement between those three was the
    /// **phantom-worker class**; after
    /// this fold-in the trio's two side-channels are derived
    /// projections of `MoleculeProcess` and the disagreement cannot
    /// occur at the data-structure level.
    ///
    /// Lifecycle:
    ///
    /// * `cs tackle` writes `Some(MoleculeProcess::new(...))` and
    ///   *also* updates [`Self::assigned_worker`] and
    ///   [`Self::session_name`] for backwards compatibility during
    ///   the migration window.
    /// * `cs done`, `cs collapse`, `cs stuck` clear the field back to
    ///   `None` on terminal transitions.
    /// * Legacy state files predating the fold-in deserialize to
    ///   `None`; readers that need to inspect the legacy trio fall
    ///   back to it transparently.
    ///
    /// Invariant tested by the proptest in
    /// `tests::proptests`: a `MoleculeData`
    /// in [`MoleculeStatus::Pending`] never carries a `process`, and
    /// any `process` that is `Some` has a non-empty `tmux_session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<cosmon_core::process::MoleculeProcess>,

    /// Per-molecule step counter circuit breaker (THESIS Part XI).
    ///
    /// Stamped at nucleate time from `.cosmon/config.toml` `[energy]
    /// default_step_budget` (or the operator's `--energy-budget <N>`).
    /// Decremented once per successful `cs evolve` step. When `remaining`
    /// hits zero, the next `cs evolve` transitions the molecule to
    /// [`MoleculeStatus::Frozen`] with reason `"energy-exhausted"` instead
    /// of advancing — the structural protection against silent runaway loops.
    ///
    /// `None` means "no budget configured" — legacy molecules and projects
    /// where `[energy] default_step_budget = 0` runs the breaker disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_budget: Option<cosmon_core::energy::StepBudget>,

    /// Wall-clock timestamp of the most recent `cs stuck` call on this
    /// molecule.
    ///
    /// `cs stuck` and `cs freeze` both transition the molecule to
    /// [`MoleculeStatus::Frozen`], but they are distinct operator gestures
    /// — `cs stuck` records a mandatory blocker reason and emits a
    /// `MoleculeStuck` event, while `cs freeze` is the symmetric pair with
    /// `cs thaw`. Without a marker the two paths collapse into the same
    /// `Frozen` state, and a downstream `cs collapse` reports
    /// `previous_status: "frozen"` even when the molecule was *just*
    /// stuck — losing the cognitive context that mattered to the operator.
    ///
    /// Lifecycle:
    ///
    /// * `cs stuck` writes `Some(now)`.
    /// * `cs thaw` clears it back to `None` (the molecule is no longer stuck).
    /// * `cs collapse` reads it: when `Some` and `status == Frozen`, the
    ///   wire-level `previous_status` is rendered as `"stuck"` (and the
    ///   emitted `MoleculeStatusChanged` event mirrors that).
    /// * Subsequent transitions out of `Frozen` clear it.
    ///
    /// Defaults to `None` for backward compatibility; legacy state files
    /// that predate this field deserialise as "never stuck", which means
    /// pre-existing collapse-from-stuck molecules render `previous_status:
    /// "frozen"` (best-effort honesty about the data we have).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_at: Option<DateTime<Utc>>,

    /// Actor class that currently holds the dispatch claim on this molecule
    /// — the anti-preemption lease.
    ///
    /// `Some(TackledBy::Human)` is a **sticky** lease: the resident runtime
    /// (`cs run`) must NEVER dispatch a molecule a human manually tackled,
    /// even if it briefly returns to `Pending` on a revision. This is the
    /// missing datum that closes the convoy-cascade-class race where the
    /// runtime, polling every few seconds, raffles a molecule a human just
    /// reached for. `assigned_worker` records *which* worker;
    /// `TackledBy` records *who dispatched*.
    ///
    /// `Some(Runtime { pid })` records a runtime-owned dispatch and does
    /// **not** block re-dispatch — only human claims are sticky ("manual
    /// always wins", enforced by a binary owner field, no clock, no tunable
    /// cooldown window). `None` for legacy molecules and never-tackled
    /// pendings — both deserialise transparently via `serde(default)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tackled_by: Option<cosmon_core::tackle::TackledBy>,

    /// Wall-clock timestamp of the most recent tackle that set
    /// [`Self::tackled_by`].
    ///
    /// Promotes the implicit "tackled" log line to a typed field so an
    /// audit can answer "when was the claim recorded?" without parsing
    /// `log.md`. `None` for legacy molecules and never-tackled pendings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tackled_at: Option<DateTime<Utc>>,
}

/// Serde skip predicate — keeps zero-valued `u32` counters out of the
/// serialised state. Reference signature is mandated by serde.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// Serde skip predicate — keeps the default
/// [`MoleculeClass::Standard`] out of the serialised state so legacy
/// state files remain byte-identical after the ADR-085 `class` field
/// is added. Reference signature is mandated by serde.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_standard_class(class: &MoleculeClass) -> bool {
    matches!(class, MoleculeClass::Standard)
}

/// Durable intent record for an in-flight step transition.
///
/// See [`MoleculeData::pending_step`] for the replay semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStep {
    /// Zero-based index of the step the advance is transitioning *to*.
    pub target_step: usize,
    /// Wall-clock timestamp the intent was written.
    pub started_at: DateTime<Utc>,
    /// Commit SHA receipt, recorded once a per-step git commit lands.
    ///
    /// Today this is always `None` — per-step commits are sibling work.
    /// Kept in the schema so future replay can
    /// detect "commit landed but state not bumped" by matching the SHA
    /// against `git log --grep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
}

/// One escalation attempt recorded during `cs done` auto-propel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationEntry {
    /// Zero-based retry number.
    pub retry: u32,
    /// Human-readable outcome (e.g. "conflict→propel", "resolved", "exhausted").
    pub outcome: String,
    /// When this escalation occurred.
    pub timestamp: DateTime<Utc>,
}

impl MoleculeData {
    /// Returns the molecule IDs this molecule blocks — i.e. the targets
    /// of its `Blocks` typed links. Empty if this molecule blocks nothing.
    ///
    /// This is the downstream-dependency walk: these molecules cannot
    /// progress until `self` completes. Used by `cs deps --transitive`
    /// and by the future resident runtime's `DagPolicy`.
    #[must_use]
    pub fn blocks(&self) -> Vec<&MoleculeId> {
        self.typed_links
            .iter()
            .filter_map(cosmon_core::interaction::MoleculeLink::blocks_target)
            .collect()
    }

    /// Returns the molecule IDs that block this molecule — i.e. the
    /// sources of its `BlockedBy` typed links. Empty if this molecule
    /// is not blocked.
    ///
    /// This is the upstream-dependency walk: `self` cannot progress
    /// until these molecules complete. Symmetric counterpart of
    /// `blocks`.
    #[must_use]
    pub fn blocked_by(&self) -> Vec<&MoleculeId> {
        self.typed_links
            .iter()
            .filter_map(cosmon_core::interaction::MoleculeLink::blocked_by_source)
            .collect()
    }

    /// Returns the molecule IDs of this molecule's decay products —
    /// i.e. the targets of its `DecayProduct` typed links. Empty if
    /// this molecule has not decayed.
    ///
    /// Decay products are children created when a molecule decomposes
    /// (e.g. an idea decaying into tasks). The DAG runtime must
    /// traverse these links to discover children nucleated during a
    /// previous run's glide phase.
    #[must_use]
    pub fn decay_products(&self) -> Vec<&MoleculeId> {
        self.typed_links
            .iter()
            .filter_map(cosmon_core::interaction::MoleculeLink::decay_product_id)
            .collect()
    }

    /// Returns the molecule IDs this molecule refines (cites) — i.e. the
    /// targets of its `Refines` typed links. Empty if no citation edges.
    ///
    /// Used primarily by `Constellation` molecules that name a pattern
    /// across N existing molecules.
    #[must_use]
    pub fn refines(&self) -> Vec<&MoleculeId> {
        self.typed_links
            .iter()
            .filter_map(cosmon_core::interaction::MoleculeLink::refines_target)
            .collect()
    }

    /// Returns the molecule IDs that refine (cite) this molecule — i.e.
    /// the sources of its `RefinedBy` typed links. Symmetric counterpart
    /// of `refines`.
    #[must_use]
    pub fn refined_by(&self) -> Vec<&MoleculeId> {
        self.typed_links
            .iter()
            .filter_map(cosmon_core::interaction::MoleculeLink::refined_by_source)
            .collect()
    }

    // ---- live-process helpers (delib-20260426-1bcd #1 fold-in) -----------

    /// Returns `true` when the molecule currently owns a live-process
    /// record (the inline [`Self::process`] field is `Some`).
    ///
    /// Distinct from [`MoleculeStatus::Running`]: a molecule may be
    /// `Running` without a `process` (e.g. a gate step that runs in the
    /// `cs tackle` parent process), and a molecule may carry a
    /// `process` while marking [`MoleculeStatus::Frozen`] (the worker
    /// session exists but is paused).
    #[must_use]
    pub fn has_live_process(&self) -> bool {
        self.process.is_some()
    }

    /// Returns the bound worker identity, preferring the inline
    /// [`Self::process`] record when present and falling back to the
    /// legacy [`Self::assigned_worker`] field for state files that
    /// predate the fold-in.
    #[must_use]
    pub fn worker(&self) -> Option<&WorkerId> {
        self.process
            .as_ref()
            .map(|p| &p.worker_id)
            .or(self.assigned_worker.as_ref())
    }

    /// Returns the tmux session bound to this molecule, preferring the
    /// inline [`Self::process`] record over the legacy
    /// [`Self::session_name`] field.
    #[must_use]
    pub fn tmux_session(&self) -> Option<&str> {
        self.process
            .as_ref()
            .map(|p| p.tmux_session.as_str())
            .or(self.session_name.as_deref())
    }

    /// Bind a fresh `MoleculeProcess` to this molecule and mirror
    /// the relevant fields on the legacy trio for backwards
    /// compatibility.
    ///
    /// This is the only writer path callers should use to start a
    /// process; it keeps the inline record and the legacy back-pointers
    /// in lockstep so old readers see no regression during the
    /// migration window.
    pub fn bind_process(&mut self, process: cosmon_core::process::MoleculeProcess) {
        self.assigned_worker = Some(process.worker_id.clone());
        self.session_name = Some(process.tmux_session.clone());
        self.updated_at = Utc::now();
        self.process = Some(process);
    }

    /// Record a dispatch claim: stamp the actor class and the tackle
    /// instant ([`Self::tackled_by`] + [`Self::tackled_at`]).
    ///
    /// This is the single writer of the anti-preemption lease. `cs tackle`
    /// calls it when it flips the molecule to `Running`; the actor is
    /// `human` for direct operator invocations and `runtime:<pid>` when the
    /// resident runtime dispatched it (`cs tackle --by runtime:<pid>`).
    pub fn mark_tackled(&mut self, by: cosmon_core::tackle::TackledBy) {
        let now = Utc::now();
        self.tackled_by = Some(by);
        self.tackled_at = Some(now);
        self.updated_at = now;
    }

    /// Returns `true` when a human holds the (sticky) dispatch claim.
    ///
    /// The walker (`cs run`) consults this to enforce "manual always
    /// wins": a human-claimed molecule is never dispatched by the runtime,
    /// even if it briefly returns to `Pending` on a revision.
    #[must_use]
    pub fn is_human_claimed(&self) -> bool {
        self.tackled_by
            .as_ref()
            .is_some_and(cosmon_core::tackle::TackledBy::is_human)
    }

    /// Clear the live-process record on terminal teardown.
    ///
    /// Mirrors [`Self::bind_process`]: clears the inline slot and also
    /// nulls the legacy `session_name` so a phantom session string
    /// cannot outlive the worker. The `assigned_worker` legacy field
    /// is preserved as a historical trace of who held the molecule —
    /// terminal molecules still answer "who completed this?" via that
    /// pointer.
    pub fn release_process(&mut self) {
        self.process = None;
        self.session_name = None;
        self.updated_at = Utc::now();
    }

    /// Return the molecule's human-readable display label, walking the
    /// variable-only fallback chain `topic` → `title` → `description`.
    ///
    /// Returns `None` if all three are missing or empty. Empty strings are
    /// treated as absent so a formula that declares `topic = ""` does not
    /// short-circuit the chain.
    ///
    /// This is the source-of-truth helper that every UI surface (peek TUI,
    /// inbox, GitHub Issues title) calls before falling back to its own
    /// per-surface chain (declaration description, formula description,
    /// molecule id). The fallback chain itself was first encoded in
    /// `cosmon-surface::compute_issue_title`; lifting the variable portion
    /// here removes the silent `(aucun topic)` regression on formulas
    /// (e.g. `task-work`) that declare `title`/`description` but no
    /// `topic`.
    #[must_use]
    pub fn display_topic(&self) -> Option<&str> {
        ["topic", "title", "description"].iter().find_map(|key| {
            self.variables
                .get(*key)
                .map(String::as_str)
                .filter(|s| !s.is_empty())
        })
    }
}

/// Serde `skip_serializing_if` helper for `bool` — skips when `false`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(v: &bool) -> bool {
    !v
}

fn default_fleet_id() -> FleetId {
    // SAFETY: "default" is non-empty, so this cannot fail.
    #[allow(clippy::expect_used)]
    FleetId::new("default").expect("non-empty string")
}

// ---------------------------------------------------------------------------
// MoleculeFilter — query predicate for list_molecules
// ---------------------------------------------------------------------------

/// Filter criteria for listing molecules.
///
/// All fields are optional — `None` means "don't filter on this field".
/// Multiple set fields are combined with AND semantics.
#[derive(Debug, Clone, Default)]
pub struct MoleculeFilter {
    pub fleet: Option<FleetId>,
    pub kind: Option<MoleculeKind>,
    pub status: Option<MoleculeStatus>,
    pub worker: Option<WorkerId>,
    pub formula: Option<FormulaId>,
    pub search_text: Option<String>,
    /// Filter by project ID. `None` means "don't filter on project".
    pub project: Option<ProjectId>,
    /// Keep only molecules whose tag set contains at least one tag that
    /// matches any of these glob patterns (see [`Tag::matches_glob`]).
    ///
    /// Empty vector means "don't filter on tags".
    pub tag_globs: Vec<String>,
}

// ---------------------------------------------------------------------------
// EnergyTracker — hexagonal port for energy persistence
// ---------------------------------------------------------------------------

/// Hexagonal port for recording and querying energy (token) consumption.
///
/// Implementations persist `EnergyRecord` entries, load budget configuration,
/// and aggregate records into reports.
pub trait EnergyTracker {
    /// Append a single energy record to the log.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the write fails.
    fn record(&self, record: &EnergyRecord) -> Result<(), CosmonError>;

    /// Load the current energy budget.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the budget config is missing or corrupt.
    fn budget(&self) -> Result<EnergyBudget, CosmonError>;

    /// Aggregate energy records into a report for the given period.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the log cannot be read.
    fn report(&self, period: &BudgetPeriod) -> Result<EnergyReport, CosmonError>;
}

// ---------------------------------------------------------------------------
// StateStore — the hexagonal port
// ---------------------------------------------------------------------------

/// RAII guard over an exclusive **fleet-state** lock.
///
/// Returned by [`StateStore::lock_fleet`]. The lock is held for as long as the
/// guard is alive and released when it drops — the object-safe successor to the
/// old `FileStore::with_fleet_lock(|s| …)` closure form (ADR-131 Decision 2).
///
/// The trait carries no methods: it exists purely so a `Box<dyn FleetGuard>`
/// can model "something is holding the fleet lock", whatever the backend's
/// mechanism — a `flock` for the JSON `FileStore`, a `BEGIN IMMEDIATE`
/// transaction for a future SQL adapter. Atomicity of the read-modify-write is
/// the *concern* the port expresses; the lock primitive is the adapter's
/// *mechanism*.
pub trait FleetGuard {}

/// RAII guard over the **cosmon main trunk** lock (ADR-110 invariant
/// I1 WRITER-UNIQUE).
///
/// Returned by [`StateStore::lock_trunk`]. Same RAII contract as
/// [`FleetGuard`]: the trunk write token is held for the guard's lifetime and
/// released on drop. The trunk lock is the **outer** lock in the trunk ⊃ fleet
/// order; see the `FileStore` adapter docs for the deadlock-freedom argument.
pub trait TrunkGuard {}

/// The no-op fleet guard — a unit that holds nothing and releases nothing on
/// drop. The default [`StateStore::lock_fleet`] returns this, so an in-memory
/// adapter (test double) needs no cross-process locking.
impl FleetGuard for () {}

/// The no-op trunk guard — see [`FleetGuard`] `for ()`.
impl TrunkGuard for () {}

/// Hexagonal port for persisting and retrieving Cosmon state.
///
/// Implementations (adapters) provide the actual storage backend
/// (filesystem, database, etc.). This trait is object-safe so it can
/// be used as `dyn StateStore`.
pub trait StateStore {
    /// Load the full fleet snapshot.
    ///
    /// # Errors
    /// Returns [`CosmonError::StateStore`] if the backing store is unreachable
    /// or the data is corrupt.
    fn load_fleet(&self) -> Result<Fleet, CosmonError>;

    /// Persist the full fleet snapshot.
    ///
    /// # Errors
    /// Returns [`CosmonError::StateStore`] if the write fails.
    fn save_fleet(&self, fleet: &Fleet) -> Result<(), CosmonError>;

    /// Load a single molecule by ID.
    ///
    /// # Errors
    /// Returns [`CosmonError::MoleculeNotFound`] if no molecule with the
    /// given ID exists, or [`CosmonError::StateStore`] on I/O failure.
    fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError>;

    /// Persist a single molecule.
    ///
    /// # Errors
    /// Returns [`CosmonError::StateStore`] if the write fails.
    fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError>;

    /// List molecules matching the given filter.
    ///
    /// # Errors
    /// Returns [`CosmonError::StateStore`] on I/O failure.
    fn list_molecules(&self, filter: &MoleculeFilter) -> Result<Vec<MoleculeData>, CosmonError>;

    /// Resolve the directory holding a molecule's durable artifacts
    /// (`log.md`, `briefing.md`, `responses/`, …).
    ///
    /// Even a future database backend persists molecule artifacts as files
    /// on disk, so "where do this molecule's files live" is a state-store
    /// concern, not a filesystem-adapter implementation detail. Promoting it
    /// to the port lets handlers obtain the path through `&dyn StateStore`
    /// instead of welding themselves to a concrete adapter.
    ///
    /// The default returns an empty path: a purely in-memory adapter (e.g. a
    /// test double) holds no on-disk artifacts and has no directory to point
    /// at. Adapters that persist artifacts on disk (the `FileStore` JSON
    /// backend, and any future DB backend that still writes `log.md`/
    /// `responses/` to a worktree) override this.
    fn molecule_dir(&self, _id: &MoleculeId) -> std::path::PathBuf {
        std::path::PathBuf::new()
    }

    /// Resolve the project root — the directory holding `.cosmon/` for this
    /// store, i.e. the worktree the worker operates in.
    ///
    /// "Where is the project rooted" is a state-location question, not a
    /// filesystem-adapter detail: a future database backend running inside a
    /// worktree still answers it (the worktree path is where `cs done` merges,
    /// where worker repos are resolved relative to). Promoting it to the port
    /// — exactly as [`StateStore::molecule_dir`] was — lets handlers such as
    /// `cs thaw` and `cs patrol` resolve worker working directories through
    /// `&dyn StateStore` instead of welding to a concrete adapter.
    ///
    /// The default returns `None`: a purely in-memory adapter (test double)
    /// has no on-disk root to point at. Adapters rooted at a real directory
    /// override this.
    fn project_root(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Acquire an exclusive guard over fleet-state read-modify-write.
    ///
    /// Hold the returned guard across a `load_fleet` → mutate → `save_fleet`
    /// (or the molecule equivalent) cycle to prevent concurrent `cs` commands
    /// from clobbering each other's writes. The lock releases when the guard
    /// drops (RAII) — the object-safe successor to the welded
    /// `FileStore::with_fleet_lock` closure (ADR-131 Decision 2). Call shape:
    /// `let _g = store.lock_fleet()?;` then read-modify-write through the port.
    ///
    /// The default returns a no-op [`FleetGuard`]: a purely in-memory adapter
    /// has no cross-process contention to serialise. The `FileStore` JSON
    /// backend overrides this to return its `flock` guard; a future SQL adapter
    /// would return a transaction guard.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the underlying lock cannot be acquired.
    fn lock_fleet(&self) -> Result<Box<dyn FleetGuard + '_>, CosmonError> {
        Ok(Box::new(()))
    }

    /// Acquire an exclusive guard over the **cosmon main trunk** (ADR-110
    /// invariant I1 WRITER-UNIQUE) — the outer lock taken by `cs done` and
    /// `cs stitch` around any mutation of the shared `main` checkout (merge,
    /// post-merge hook, frontier write).
    ///
    /// `cmd_hint` is a human-readable label (e.g. `"cs done task-…"`) the
    /// backend may record in its holder hint so a contending process can report
    /// *who* holds the lock. The lock releases when the guard drops.
    ///
    /// The default returns a no-op [`TrunkGuard`]. The `FileStore` backend
    /// overrides this to delegate to its `flock`-based trunk guard.
    ///
    /// # Errors
    /// Returns [`CosmonError`] if the trunk lock cannot be acquired.
    fn lock_trunk(&self, _cmd_hint: &str) -> Result<Box<dyn TrunkGuard + '_>, CosmonError> {
        Ok(Box::new(()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Prove that `StateStore` is object-safe by constructing a trait object reference.
    #[test]
    fn state_store_is_object_safe() {
        fn accepts_dyn(_store: &dyn StateStore) {}
        let _ = accepts_dyn;
    }

    /// Prove the locking guards are object-safe — the `lock_fleet` /
    /// `lock_trunk` port methods (ADR-131 Decision 2) return
    /// `Box<dyn FleetGuard>` / `Box<dyn TrunkGuard>`, so the guard traits must
    /// stay `dyn`-compatible for `StateStore` itself to remain object-safe.
    #[test]
    fn lock_guards_are_object_safe() {
        fn accepts_fleet(_g: &dyn FleetGuard) {}
        fn accepts_trunk(_g: &dyn TrunkGuard) {}
        let _ = accepts_fleet;
        let _ = accepts_trunk;
    }

    /// A minimal in-memory [`StateStore`] that overrides only the persistence
    /// methods, exercising the *default* no-op `lock_fleet` / `lock_trunk`
    /// bodies. Proves the default guards acquire and drop without I/O so a test
    /// double need not implement locking.
    #[test]
    fn default_lock_methods_return_noop_guards() {
        struct Mem;
        impl StateStore for Mem {
            fn load_fleet(&self) -> Result<Fleet, CosmonError> {
                Ok(Fleet::default())
            }
            fn save_fleet(&self, _fleet: &Fleet) -> Result<(), CosmonError> {
                Ok(())
            }
            fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
                Err(CosmonError::MoleculeNotFound(id.clone()))
            }
            fn save_molecule(
                &self,
                _id: &MoleculeId,
                _data: &MoleculeData,
            ) -> Result<(), CosmonError> {
                Ok(())
            }
            fn list_molecules(
                &self,
                _filter: &MoleculeFilter,
            ) -> Result<Vec<MoleculeData>, CosmonError> {
                Ok(Vec::new())
            }
        }
        let store: &dyn StateStore = &Mem;
        let fleet_guard = store.lock_fleet().expect("no-op fleet guard");
        let trunk_guard = store.lock_trunk("test").expect("no-op trunk guard");
        // Guards drop here without panicking — the no-op release path.
        drop(fleet_guard);
        drop(trunk_guard);
    }

    /// Prove that `EnergyTracker` is object-safe.
    #[test]
    fn energy_tracker_is_object_safe() {
        fn accepts_dyn(_tracker: &dyn EnergyTracker) {}
        let _ = accepts_dyn;
    }

    fn mol_with_links(links: Vec<MoleculeLink>) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new("task-20260409-hlpr").unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
            typed_links: links,
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
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
    fn test_blocks_helper_returns_targets() {
        let a = MoleculeId::new("task-20260409-aaaa").unwrap();
        let b = MoleculeId::new("task-20260409-bbbb").unwrap();
        let mol = mol_with_links(vec![
            MoleculeLink::Blocks { target: a.clone() },
            MoleculeLink::Blocks { target: b.clone() },
            MoleculeLink::Entangled {
                target: "noise".to_owned(),
            },
        ]);
        let blocked = mol.blocks();
        assert_eq!(blocked.len(), 2);
        assert!(blocked.contains(&&a));
        assert!(blocked.contains(&&b));
    }

    #[test]
    fn test_blocked_by_helper_returns_sources() {
        let a = MoleculeId::new("task-20260409-cccc").unwrap();
        let mol = mol_with_links(vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::DecayedFrom {
                id: MoleculeId::new("task-20260409-dddd").unwrap(),
            },
        ]);
        let upstream = mol.blocked_by();
        assert_eq!(upstream.len(), 1);
        assert_eq!(upstream[0], &a);
    }

    #[test]
    fn test_blocks_helpers_empty_for_molecule_without_links() {
        let mol = mol_with_links(Vec::new());
        assert!(mol.blocks().is_empty());
        assert!(mol.blocked_by().is_empty());
    }

    #[test]
    fn test_blocks_and_blocked_by_are_independent() {
        // A single molecule can be both upstream and downstream in the DAG.
        let parent = MoleculeId::new("task-20260409-pppp").unwrap();
        let child = MoleculeId::new("task-20260409-kkkk").unwrap();
        let mol = mol_with_links(vec![
            MoleculeLink::BlockedBy {
                source: parent.clone(),
            },
            MoleculeLink::Blocks {
                target: child.clone(),
            },
        ]);
        assert_eq!(mol.blocks(), vec![&child]);
        assert_eq!(mol.blocked_by(), vec![&parent]);
    }

    #[test]
    fn test_display_topic_prefers_topic_variable() {
        let mut mol = mol_with_links(Vec::new());
        mol.variables.insert("topic".into(), "the topic".into());
        mol.variables.insert("title".into(), "the title".into());
        mol.variables
            .insert("description".into(), "the description".into());
        assert_eq!(mol.display_topic(), Some("the topic"));
    }

    #[test]
    fn test_display_topic_falls_back_to_title_then_description() {
        let mut mol = mol_with_links(Vec::new());
        mol.variables.insert("title".into(), "only title".into());
        mol.variables
            .insert("description".into(), "the description".into());
        assert_eq!(mol.display_topic(), Some("only title"));

        mol.variables.clear();
        mol.variables
            .insert("description".into(), "only description".into());
        assert_eq!(mol.display_topic(), Some("only description"));
    }

    #[test]
    fn test_display_topic_empty_strings_do_not_short_circuit() {
        // A formula author who sets `topic = ""` (empty default) must not
        // mask a populated `title` further down the chain. The bug
        // task-20260423-19ca chases is exactly this kind of silent
        // `(aucun topic)` regression on `task-work` molecules.
        let mut mol = mol_with_links(Vec::new());
        mol.variables.insert("topic".into(), String::new());
        mol.variables.insert("title".into(), "the title".into());
        assert_eq!(mol.display_topic(), Some("the title"));
    }

    #[test]
    fn test_display_topic_returns_none_when_all_absent_or_empty() {
        let mut mol = mol_with_links(Vec::new());
        assert_eq!(mol.display_topic(), None);

        mol.variables.insert("topic".into(), String::new());
        mol.variables.insert("title".into(), String::new());
        mol.variables.insert("description".into(), String::new());
        assert_eq!(mol.display_topic(), None);
    }

    #[test]
    fn test_assigned_role_serde_roundtrip() {
        let mut mol = mol_with_links(Vec::new());
        mol.assigned_role = Some(AgentRole::Research);
        let json = serde_json::to_string(&mol).unwrap();
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.assigned_role, Some(AgentRole::Research));
    }

    #[test]
    fn test_seals_default_to_empty_on_legacy_json() {
        // A state.json from before briefing seals landed has no
        // `prompt_seal` and no `briefing_seals` fields. The
        // `#[serde(default)]` attributes must guarantee such files
        // deserialize successfully with empty defaults so backward
        // compat is preserved.
        let legacy_json = r#"{
            "id": "task-20260417-legacy",
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": "pending",
            "variables": {},
            "assigned_worker": null,
            "created_at": "2026-04-17T10:00:00Z",
            "updated_at": "2026-04-17T10:00:00Z",
            "total_steps": 2,
            "current_step": 0,
            "completed_steps": [],
            "collapse_reason": null,
            "collapsed_step": null,
            "links": []
        }"#;
        let mol: MoleculeData = serde_json::from_str(legacy_json).unwrap();
        assert!(mol.prompt_seal.is_none());
        assert!(mol.briefing_seals.is_empty());
    }

    #[test]
    fn test_seals_roundtrip_through_serde() {
        use crate::BriefingSeal;
        let mut mol = mol_with_links(Vec::new());
        mol.prompt_seal = Some(BriefingSeal::of_bytes(0, b"prompt contents"));
        mol.briefing_seals
            .push(BriefingSeal::of_bytes(0, b"step 0"));
        mol.briefing_seals
            .push(BriefingSeal::of_bytes(1, b"step 1"));
        let json = serde_json::to_string(&mol).unwrap();
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.prompt_seal, mol.prompt_seal);
        assert_eq!(back.briefing_seals, mol.briefing_seals);
    }

    #[test]
    fn test_seals_omitted_when_empty() {
        // Empty seals must not bloat state.json — keeps diffs readable
        // and legacy reader-safe.
        let mol = mol_with_links(Vec::new());
        let json = serde_json::to_string(&mol).unwrap();
        assert!(!json.contains("prompt_seal"), "should be omitted: {json}");
        assert!(
            !json.contains("briefing_seals"),
            "should be omitted: {json}"
        );
    }

    #[test]
    fn test_pending_step_serde_roundtrip() {
        let mut mol = mol_with_links(Vec::new());
        mol.pending_step = Some(PendingStep {
            target_step: 2,
            started_at: Utc::now(),
            commit_sha: Some("abc123".to_owned()),
        });
        let json = serde_json::to_string(&mol).unwrap();
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        let p = back.pending_step.expect("pending_step present");
        assert_eq!(p.target_step, 2);
        assert_eq!(p.commit_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_last_progress_at_serde_roundtrip() {
        let mut mol = mol_with_links(Vec::new());
        let stamp = Utc::now();
        mol.last_progress_at = Some(stamp);
        let json = serde_json::to_string(&mol).unwrap();
        assert!(
            json.contains("last_progress_at"),
            "field must serialize when set: {json}"
        );
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_progress_at, Some(stamp));
    }

    #[test]
    fn test_last_output_at_serde_roundtrip() {
        let stamp = Utc::now();
        let mut mol = mol_with_links(Vec::new());
        mol.last_output_at = Some(stamp);
        let json = serde_json::to_string(&mol).unwrap();
        assert!(json.contains("last_output_at"));
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_output_at, Some(stamp));
    }

    #[test]
    fn test_last_progress_at_omitted_when_none() {
        let mol = mol_with_links(Vec::new());
        assert!(mol.last_progress_at.is_none());
        let json = serde_json::to_string(&mol).unwrap();
        assert!(
            !json.contains("last_progress_at"),
            "should be omitted when None: {json}"
        );
    }

    /// `nudge_count` round-trips through serde, is omitted when zero
    /// (default), and defaults to zero on legacy state files.
    #[test]
    fn test_nudge_count_serde_roundtrip_and_legacy() {
        // Default is zero — must be omitted from JSON.
        let mol = mol_with_links(Vec::new());
        assert_eq!(mol.nudge_count, 0);
        let json = serde_json::to_string(&mol).unwrap();
        assert!(
            !json.contains("nudge_count"),
            "should be omitted when zero: {json}"
        );

        // Non-zero round-trips.
        let mut mol = mol_with_links(Vec::new());
        mol.nudge_count = 3;
        let json = serde_json::to_string(&mol).unwrap();
        assert!(
            json.contains("\"nudge_count\":3"),
            "field must serialize when set: {json}"
        );
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.nudge_count, 3);

        // Legacy state files (no field) deserialize as zero.
        let legacy_json = r#"{
            "id": "task-20260421-old",
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": "pending",
            "variables": {},
            "assigned_worker": null,
            "created_at": "2026-04-21T10:00:00Z",
            "updated_at": "2026-04-21T10:00:00Z",
            "total_steps": 2,
            "current_step": 0,
            "completed_steps": [],
            "collapse_reason": null,
            "collapsed_step": null,
            "links": []
        }"#;
        let mol: MoleculeData = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(mol.nudge_count, 0);
    }

    #[test]
    fn test_last_progress_at_defaults_to_none_on_legacy_json() {
        // State files predating this field must deserialize cleanly with
        // `last_progress_at` defaulting to `None` (no inference stall signal
        // available — peek treats absence as "unknown", not "stalled").
        let legacy_json = r#"{
            "id": "task-20260421-old",
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": "pending",
            "variables": {},
            "assigned_worker": null,
            "created_at": "2026-04-21T10:00:00Z",
            "updated_at": "2026-04-21T10:00:00Z",
            "total_steps": 2,
            "current_step": 0,
            "completed_steps": [],
            "collapse_reason": null,
            "collapsed_step": null,
            "links": []
        }"#;
        let mol: MoleculeData = serde_json::from_str(legacy_json).unwrap();
        assert!(mol.last_progress_at.is_none());
    }

    #[test]
    fn test_pending_step_defaults_to_none_on_legacy_json() {
        let mol = mol_with_links(Vec::new());
        let mut json: serde_json::Value = serde_json::to_value(&mol).unwrap();
        // Simulate a legacy JSON file that predates the field.
        assert!(
            json.as_object_mut()
                .unwrap()
                .remove("pending_step")
                .is_none()
                || json.get("pending_step").is_none()
        );
        let back: MoleculeData = serde_json::from_value(json).unwrap();
        assert!(back.pending_step.is_none());
    }

    #[test]
    fn test_pending_step_omitted_when_none() {
        // Intent records are "expensive" metadata — only serialize when
        // an advance is actually in flight. This keeps state.json clean
        // in the steady state and makes `git diff` on surfaces readable.
        let mol = mol_with_links(Vec::new());
        assert!(mol.pending_step.is_none());
        let json = serde_json::to_string(&mol).unwrap();
        assert!(
            !json.contains("pending_step"),
            "pending_step should be omitted when None: {json}"
        );
    }

    #[test]
    fn test_assigned_role_defaults_to_none_on_legacy_json() {
        let mut mol = mol_with_links(Vec::new());
        mol.assigned_role = Some(AgentRole::Advisory);
        let mut json: serde_json::Value = serde_json::to_value(&mol).unwrap();
        // Simulate a legacy JSON file that predates the field.
        json.as_object_mut().unwrap().remove("assigned_role");
        let back: MoleculeData = serde_json::from_value(json).unwrap();
        assert!(back.assigned_role.is_none());
    }

    // ── WorkerRole phase-1 migration (delib-20260414-2ab2 / ADR-040) ──

    /// Fabricate a `WorkerData` for `WorkerRole` migration tests.
    fn worker_for_role_test(
        id: &str,
        agent_role: AgentRole,
        worker_role: WorkerRole,
    ) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(id).unwrap(),
            AgentId::new("test-agent").unwrap(),
            agent_role,
            Clearance::Write,
            WorkerStatus::Active,
        );
        // `WorkerData::new` derives worker_role from (agent_role, id); the
        // tests that exercise the reconcile_worker_roles path need a starting
        // value that does NOT already agree with the derivation, so we
        // override it here.
        w.worker_role = worker_role;
        w
    }

    #[test]
    fn test_worker_role_defaults_to_cognition_on_legacy_json() {
        // A fleet.json written before phase 1 has no `worker_role` field on
        // worker entries. The `#[serde(default)]` attribute must guarantee
        // such entries deserialize without error and land in `Cognition` so
        // the runtime-vs-cognition discriminator becomes a no-op for legacy
        // state files that predate ADR-040.
        let legacy_json = r#"{
            "id": "ep-quartz",
            "agent_id": "polecat",
            "role": "implementation",
            "clearance": "write",
            "status": "active",
            "desired": "running",
            "repo": null,
            "current_molecule": null,
            "updated_at": "2026-04-14T10:00:00Z"
        }"#;
        let worker: WorkerData = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(worker.worker_role, WorkerRole::Cognition);
    }

    #[test]
    fn test_worker_role_roundtrip_runtime() {
        // Round-trip a worker_role = Runtime through JSON; prove it survives
        // serialization intact (no default collapse on deserialize).
        let w = worker_for_role_test("runtime-foo-1234", AgentRole::Runtime, WorkerRole::Runtime);
        let json = serde_json::to_string(&w).unwrap();
        let back: WorkerData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_reconcile_worker_roles_promotes_legacy_runtime() {
        // Legacy fleet.json: a worker whose id screams "runtime-" but whose
        // persisted worker_role is the default `Cognition` (because the JSON
        // file predates the field). Reconcile must promote it to `Runtime`.
        let mut fleet = Fleet::new();
        let w = worker_for_role_test(
            "runtime-xyz-1234",
            AgentRole::Implementation,
            WorkerRole::Cognition,
        );
        fleet.workers.insert(w.id.clone(), w);
        fleet.reconcile_worker_roles();
        let out = fleet
            .workers
            .get(&WorkerId::new("runtime-xyz-1234").unwrap())
            .unwrap();
        assert_eq!(out.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_reconcile_worker_roles_promotes_agent_role_runtime() {
        // Second derivation rule: a worker whose AgentRole is Runtime but
        // whose worker_role defaulted to Cognition (legacy file) must be
        // promoted to Runtime on reconcile.
        let mut fleet = Fleet::new();
        let w = worker_for_role_test("quartz-abcd", AgentRole::Runtime, WorkerRole::Cognition);
        fleet.workers.insert(w.id.clone(), w);
        fleet.reconcile_worker_roles();
        let out = fleet
            .workers
            .get(&WorkerId::new("quartz-abcd").unwrap())
            .unwrap();
        assert_eq!(out.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_reconcile_worker_roles_never_demotes_runtime() {
        // Runtime never degrades to Cognition on reconcile — protects
        // operators who deliberately labelled an unusual runtime session.
        let mut fleet = Fleet::new();
        let w = worker_for_role_test(
            "non-runtime-shaped-id",
            AgentRole::Implementation,
            WorkerRole::Runtime,
        );
        fleet.workers.insert(w.id.clone(), w);
        fleet.reconcile_worker_roles();
        let out = fleet
            .workers
            .get(&WorkerId::new("non-runtime-shaped-id").unwrap())
            .unwrap();
        assert_eq!(out.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_reconcile_worker_roles_leaves_cognition_untouched() {
        // Cognition stays Cognition when neither derivation heuristic fires.
        let mut fleet = Fleet::new();
        let w = worker_for_role_test(
            "quartz-efgh",
            AgentRole::Implementation,
            WorkerRole::Cognition,
        );
        fleet.workers.insert(w.id.clone(), w);
        fleet.reconcile_worker_roles();
        let out = fleet
            .workers
            .get(&WorkerId::new("quartz-efgh").unwrap())
            .unwrap();
        assert_eq!(out.worker_role, WorkerRole::Cognition);
    }

    #[test]
    fn test_reconcile_worker_roles_is_idempotent() {
        // Running the reconcile twice must yield the same fleet — phase 1 is
        // purely a projection from legacy state to the explicit field.
        let mut fleet = Fleet::new();
        let w = worker_for_role_test(
            "runtime-abc-9999",
            AgentRole::Implementation,
            WorkerRole::Cognition,
        );
        fleet.workers.insert(w.id.clone(), w);
        fleet.reconcile_worker_roles();
        let after_once: WorkerData = fleet
            .workers
            .get(&WorkerId::new("runtime-abc-9999").unwrap())
            .unwrap()
            .clone();
        fleet.reconcile_worker_roles();
        let after_twice = fleet
            .workers
            .get(&WorkerId::new("runtime-abc-9999").unwrap())
            .unwrap();
        assert_eq!(after_once.worker_role, after_twice.worker_role);
        assert_eq!(after_once.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_worker_data_new_derives_runtime_role_from_prefix() {
        // Constructor-time derivation: runtime- prefix pins worker_role
        // without caller needing to call with_worker_role.
        let w = WorkerData::new(
            WorkerId::new("runtime-foo-abcd").unwrap(),
            AgentId::new("runtime").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        assert_eq!(w.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_worker_data_new_derives_runtime_role_from_agent_role() {
        let w = WorkerData::new(
            WorkerId::new("jasper-xyz").unwrap(),
            AgentId::new("runtime").unwrap(),
            AgentRole::Runtime,
            Clearance::Write,
            WorkerStatus::Active,
        );
        assert_eq!(w.worker_role, WorkerRole::Runtime);
    }

    #[test]
    fn test_worker_data_new_defaults_to_cognition() {
        let w = WorkerData::new(
            WorkerId::new("quartz-xyz").unwrap(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        assert_eq!(w.worker_role, WorkerRole::Cognition);
    }

    // ── MoleculeData lossless serde roundtrip (task-20260523-5bd6) ──
    //
    // Acceptance (b) for the voix data-loss postmortem: prove that the
    // `MoleculeData` ↔ JSON roundtrip is lossless for every shape the
    // codebase can produce *today*, plus a hand-crafted "voix-era"
    // fixture that captures the on-disk shape predating the recent
    // schema additions. If a future field is added without `#[serde(default)]`
    // these tests catch the silent-drop regression at compile/test time.

    /// Voix bootstrapped 2026-04-19. Its state.json files predate
    /// `class`, `collapse_cause`, `collapse_reason_kind`, `bootstrap_seals`,
    /// `process`, `energy_budget`, `stuck_at`, and `last_nudged_at`. They
    /// carry user-visible payload (variables, kind, `project_id`,
    /// `session_name`) that the event-stream projection cannot reconstruct.
    ///
    /// This fixture is the literal shape `cs reconcile` was destroying.
    /// Deserialising it must succeed; reserialising and re-deserialising
    /// must preserve every operator-visible field intact.
    const VOIX_ERA_STATE_JSON: &str = r#"{
        "id": "task-20260419-0442",
        "fleet_id": "default",
        "formula_id": "task-work",
        "status": "running",
        "variables": {
            "topic": "scaffold voix transport layer",
            "detail": "wire whisper / propulsion pipes; carry over channel taxonomy from cosmon"
        },
        "assigned_worker": "polecat-9999",
        "created_at": "2026-04-19T13:42:00Z",
        "updated_at": "2026-04-19T14:11:30Z",
        "total_steps": 2,
        "current_step": 1,
        "completed_steps": ["implement"],
        "collapse_reason": null,
        "collapsed_step": null,
        "links": [],
        "kind": "task",
        "typed_links": [],
        "project_id": "voix-7c3f",
        "session_name": "voix-task-0442",
        "tags": ["temp:hot"],
        "freeze_on_last_step": false
    }"#;

    #[test]
    fn test_voix_era_state_deserializes_with_all_fields() {
        use cosmon_core::kind::MoleculeKind;

        let mol: MoleculeData = serde_json::from_str(VOIX_ERA_STATE_JSON)
            .expect("voix-era state.json must deserialise on current binary");
        assert_eq!(mol.id.as_str(), "task-20260419-0442");
        assert_eq!(mol.formula_id.as_str(), "task-work");
        assert_eq!(mol.kind, Some(MoleculeKind::Task));
        assert_eq!(
            mol.variables.get("topic").map(String::as_str),
            Some("scaffold voix transport layer")
        );
        assert_eq!(
            mol.variables.get("detail").map(String::as_str),
            Some("wire whisper / propulsion pipes; carry over channel taxonomy from cosmon")
        );
        assert_eq!(
            mol.project_id.as_ref().map(ProjectId::as_str),
            Some("voix-7c3f")
        );
        assert_eq!(mol.session_name.as_deref(), Some("voix-task-0442"));
        assert_eq!(mol.completed_steps.len(), 1);
        assert_eq!(mol.completed_steps[0].as_str(), "implement");
        // Fields added after voix-era default cleanly:
        assert!(mol.collapse_cause.is_none());
        assert!(mol.collapse_reason_kind.is_none());
        assert!(mol.bootstrap_seals.is_empty());
        assert!(mol.process.is_none());
        assert!(mol.energy_budget.is_none());
        assert!(mol.stuck_at.is_none());
        assert_eq!(mol.nudge_count, 0);
    }

    #[test]
    fn test_voix_era_roundtrip_preserves_every_operator_visible_field() {
        let original: MoleculeData = serde_json::from_str(VOIX_ERA_STATE_JSON).unwrap();
        let serialized = serde_json::to_string(&original).unwrap();
        let back: MoleculeData = serde_json::from_str(&serialized).unwrap();
        // Operator-visible payload (variables, kind, project_id,
        // session_name, tags, completed_steps named ids) — every one of
        // these was being destroyed by the lossy rebuild.
        assert_eq!(back.id, original.id);
        assert_eq!(back.formula_id, original.formula_id);
        assert_eq!(back.status, original.status);
        assert_eq!(back.variables, original.variables);
        assert_eq!(back.kind, original.kind);
        assert_eq!(back.project_id, original.project_id);
        assert_eq!(back.session_name, original.session_name);
        assert_eq!(back.tags, original.tags);
        assert_eq!(back.completed_steps, original.completed_steps);
        assert_eq!(back.assigned_worker, original.assigned_worker);
    }

    proptest::proptest! {
        /// `MoleculeData` survives a JSON serialise → deserialise roundtrip
        /// for arbitrary inputs over the space of operator-controllable
        /// fields. If a future schema bump drops or renames a field without
        /// preserving backward compatibility, this proptest fails — the
        /// regression cannot land silently. This safety net was motivated
        /// by a real incident where a renamed field silently dropped data
        /// on deserialise.
        #[test]
        fn molecule_data_roundtrip_is_lossless(input in arb_molecule_data()) {
            let json = serde_json::to_string(&input).expect("serialise");
            let back: MoleculeData =
                serde_json::from_str(&json).expect("deserialise");
            // Compare via the canonical JSON form: avoids requiring `PartialEq`
            // on every nested type while still catching any field drop.
            let canon_a = serde_json::to_value(&input).unwrap();
            let canon_b = serde_json::to_value(&back).unwrap();
            proptest::prop_assert_eq!(canon_a, canon_b);
        }
    }

    /// Strategy that builds a `MoleculeData` exercising the operator-visible
    /// surface that the voix data-loss bug was destroying. Covers the fields a
    /// future schema bump is most likely to silently drop.
    ///
    /// The strategy deliberately varies the *drift-prone* fields — the ones
    /// whose serde shape is most likely to regress unnoticed: `typed_links`
    /// (each variant's `rel` tag + target payload), `tags`, `collapse_cause`,
    /// `briefing_seals`, and `energy_budget`. An earlier version pinned all of
    /// these to their defaults (`Vec::new()` / `None` / `BTreeSet::new()`), so
    /// a serde regression on any of them still passed the "lossless" proptest
    /// green — the fixture never produced a non-default value to lose. See
    /// C10 test review, `review-report.md` F1.
    fn arb_molecule_data() -> impl proptest::strategy::Strategy<Value = MoleculeData> {
        use cosmon_core::kind::MoleculeKind;
        use proptest::collection::{hash_map, vec};
        use proptest::option;
        use proptest::strategy::{Just, Strategy};

        // Use a fixed valid date — MoleculeId requires YYYYMMDD with valid
        // calendar fields, which a naive `[0-9]{8}` regex would frequently
        // miss. The prefix and suffix carry all the variability we need
        // for serde-roundtrip coverage.
        let id_strategy =
            "[a-z]{3,6}-20260523-[a-f0-9]{4}".prop_map(|s| MoleculeId::new(s).unwrap());
        let formula_strategy = "[a-z][a-z0-9-]{2,15}".prop_map(|s| FormulaId::new(s).unwrap());
        let project_strategy =
            option::of("[a-z]{3,8}-[a-f0-9]{4}".prop_map(|s| ProjectId::new(s).unwrap()));
        let session_strategy = option::of("[a-z0-9-]{4,32}");
        let kind_strategy = option::of(proptest::sample::select(vec![
            MoleculeKind::Idea,
            MoleculeKind::Task,
            MoleculeKind::Decision,
            MoleculeKind::Issue,
            MoleculeKind::Signal,
            MoleculeKind::Deliberation,
        ]));
        let variables_strategy = hash_map("[a-z][a-z_]{0,12}", ".{0,80}", 0..6);
        let completed_strategy = vec(
            "[a-z][a-z0-9-]{0,15}".prop_map(|s| StepId::new(s).unwrap()),
            0..6,
        );

        // --- Drift-prone fields (see doc comment above) ---

        // `typed_links` — vary both the `rel` discriminant and the payload,
        // since serde tags the enum by `rel` and a rename on either the tag
        // or a payload field is exactly the kind of silent drop this guards.
        let link_id = "[a-z]{3,6}-20260523-[a-f0-9]{4}".prop_map(|s| MoleculeId::new(s).unwrap());
        let typed_links_strategy = vec(
            proptest::prop_oneof![
                link_id
                    .clone()
                    .prop_map(|target| MoleculeLink::Blocks { target }),
                link_id
                    .clone()
                    .prop_map(|source| MoleculeLink::BlockedBy { source }),
                link_id
                    .clone()
                    .prop_map(|target| MoleculeLink::Refines { target }),
                link_id.prop_map(|id| MoleculeLink::DecayedFrom { id }),
                "[a-z0-9:/._-]{0,40}".prop_map(|target| MoleculeLink::Entangled { target }),
            ],
            0..5,
        );

        // `tags` — a `BTreeSet<Tag>` built from valid kebab-case keys with an
        // optional `:value`. Tags are the operator-visible curation surface.
        let tags_strategy = vec(
            (
                "[a-z][a-z0-9-]{0,10}",
                option::of("[a-z0-9][a-z0-9._-]{0,10}"),
            )
                .prop_map(|(key, value)| match value {
                    Some(v) => Tag::new(format!("{key}:{v}")).unwrap(),
                    None => Tag::new(key).unwrap(),
                }),
            0..5,
        )
        .prop_map(|tags| tags.into_iter().collect::<BTreeSet<Tag>>());

        // `collapse_cause` — every variant, including the payload-carrying
        // `RateLimit` (ADR-062) whose optional `account` / `kind_quota`
        // fields use `skip_serializing_if` and are prime drop candidates.
        let collapse_cause_strategy = option::of(proptest::prop_oneof![
            (option::of("[a-z]{3,8}"), option::of("[a-z_]{3,16}")).prop_map(
                |(account, kind_quota)| cosmon_core::molecule::CollapseCause::RateLimit {
                    account,
                    kind_quota,
                }
            ),
            Just(cosmon_core::molecule::CollapseCause::InferenceStall),
            Just(cosmon_core::molecule::CollapseCause::Manual),
            Just(cosmon_core::molecule::CollapseCause::ProcessDeath),
            Just(cosmon_core::molecule::CollapseCause::Unknown),
        ]);

        // `briefing_seals` — the retrospective-verification trail (ADR-056).
        // `canonical_version` uses `#[serde(default)]`, so a serialize/skip
        // regression there would slip past a hardcoded-empty fixture.
        // `sealed_at` uses `Utc::now()` (as the surrounding fields do) so the
        // RFC3339 roundtrip stays trivially lossless while step/hash/bytes/
        // version vary.
        let briefing_seals_strategy = vec(
            (0u32..8, "[a-f0-9]{64}", 0u64..100_000, 0u8..=1).prop_map(
                |(step, hash, briefing_bytes, canonical_version)| BriefingSeal {
                    step,
                    hash,
                    sealed_at: Utc::now(),
                    briefing_bytes,
                    canonical_version,
                },
            ),
            0..4,
        );

        // `energy_budget` — the circuit-breaker counter (ADR-062).
        let energy_budget_strategy = option::of(
            (0u32..1000, 0u32..1000)
                .prop_map(|(cap, remaining)| cosmon_core::energy::StepBudget { cap, remaining }),
        );

        (
            id_strategy,
            formula_strategy,
            variables_strategy,
            project_strategy,
            session_strategy,
            kind_strategy,
            completed_strategy,
            (
                typed_links_strategy,
                tags_strategy,
                collapse_cause_strategy,
                briefing_seals_strategy,
                energy_budget_strategy,
            ),
        )
            .prop_map(
                |(
                    id,
                    formula_id,
                    variables,
                    project_id,
                    session_name,
                    kind,
                    completed_steps,
                    (typed_links, tags, collapse_cause, briefing_seals, energy_budget),
                )| {
                    MoleculeData {
                        id,
                        fleet_id: FleetId::new("default").unwrap(),
                        formula_id,
                        status: MoleculeStatus::Running,
                        variables,
                        assigned_worker: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        total_steps: 2,
                        current_step: 1,
                        completed_steps,
                        collapse_reason: None,
                        collapse_cause,
                        collapse_reason_kind: None,
                        collapsed_step: None,
                        links: Vec::new(),
                        kind,
                        class: cosmon_core::molecule_class::MoleculeClass::default(),
                        typed_links,
                        project_id,
                        assigned_role: None,
                        session_name,
                        tags,
                        escalations: Vec::new(),
                        freeze_on_last_step: false,
                        expires_at: None,
                        expiry_policy: None,
                        originating_branch: None,
                        pending_step: None,
                        merged_at: None,
                        prompt_seal: None,
                        briefing_seals,
                        bootstrap_seals: Vec::new(),
                        archived: false,
                        last_progress_at: None,
                        last_output_at: None,
                        nudge_count: 0,
                        last_nudged_at: None,
                        propel_count: 0,
                        last_propelled_at: None,
                        process: None,
                        energy_budget,
                        stuck_at: None,
                        tackled_by: None,
                        tackled_at: None,
                    }
                },
            )
    }
}
