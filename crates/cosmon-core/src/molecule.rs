// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule lifecycle types and typestate machine.
//!
//! The molecule state machine uses the typestate pattern: each lifecycle state
//! is a distinct Rust type, and only valid transitions are available as methods.
//! Invalid transitions (e.g., `Molecule<Frozen>::evolve()`) are compile errors.
//!
//! **Where to start.** For a molecule's *runtime* state — what the pilot
//! asked for versus what the worker actually did — read
//! [`crate::run_state::RunState`], the canonical type. The
//! [`MoleculeStatus`] enum in this module is a legacy single-field
//! projection kept only for serialization; new code should not reach for it.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::id::{FormulaId, MoleculeId, StepId, WorkerId};
//! use cosmon_core::molecule::{EvolveOutcome, Molecule, MoleculeStatus};
//!
//! let mol_id = MoleculeId::new("test-20260401-abcd").unwrap();
//! let formula = FormulaId::new("mol-polecat-work").unwrap();
//! let worker = WorkerId::new("onyx").unwrap();
//!
//! // Nucleate lands in Pending — no worker yet, no evolve possible.
//! let mol = Molecule::new(mol_id, formula, 2);
//! assert_eq!(mol.status(), MoleculeStatus::Pending);
//!
//! // Tackle lifts Pending → Active; the type-system forbids skipping it.
//! let mol = mol.tackle(worker);
//! assert_eq!(mol.status(), MoleculeStatus::Running);
//!
//! // Evolve through step 0:
//! let step0 = StepId::new("load-context").unwrap();
//! let mol = match mol.evolve(step0) {
//!     EvolveOutcome::Active(m) => m,
//!     EvolveOutcome::Completed(_) => panic!("still has steps"),
//! };
//!
//! // Evolve through step 1 (last) → completes:
//! let step1 = StepId::new("implement").unwrap();
//! match mol.evolve(step1) {
//!     EvolveOutcome::Completed(done) => {
//!         assert_eq!(done.status(), MoleculeStatus::Completed);
//!     }
//!     EvolveOutcome::Active(_) => panic!("should be done"),
//! }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::id::{FormulaId, MoleculeId, StepId, WorkerId};

// ---------------------------------------------------------------------------
// MoleculeStatus — serializable enum for persistence / wire format
// ---------------------------------------------------------------------------

/// Serializable molecule lifecycle status — **legacy projection, read
/// [`crate::run_state::RunState`] first.**
///
/// **⚠ Demoted to a legacy projection (ADR-052).** If you are new to this
/// code, the type to understand is [`crate::run_state::RunState`], not this
/// enum. `RunState` splits *what the pilot wanted* (intent) from *what the
/// worker actually did* (witness) into two separate fields, so a molecule
/// that stalled mid-run is never confused with one the pilot deliberately
/// paused. `MoleculeStatus` collapses those two axes into a single value,
/// which means it cannot tell those drift cases apart — that is exactly why
/// it was retired. New code must consume `RunState` and use
/// [`crate::run_state::RunState::ghost`] to classify drift.
///
/// This enum survives only as a serialization shim for persisted/wire
/// formats. Use [`crate::run_state::molecule_status_from_run_state`] to
/// derive it from a `RunState` during the migration; use
/// `Intent::from(status)` to lift a persisted value back into the canonical
/// type. The `#[serde(alias = "active")]` on `Running` must remain in place
/// until the next major bump.
///
/// `#[non_exhaustive]` is the ADR-062 §5.2 mitigation: new variants
/// (e.g. [`Self::Starved`]) can be added without breaking external
/// consumers' exhaustive matches.
///
/// # Status lifecycle
///
/// ```text
/// pending → queued → running → completed
///                  ↘ frozen ↗
///                  ↘ starved (refresh restores → running, ADR-062)
///                  ↘ collapsed (terminal)
/// ```
///
/// - **Pending**: created but not assigned to any worker
/// - **Queued**: assigned to a worker but not yet executing
/// - **Running**: actively being worked on by a worker
/// - **Frozen**: execution suspended (can be thawed)
/// - **Starved**: external authority (e.g. quota provider) refused
///   service — wait, rotate, or collapse, but never re-prompt (ADR-062)
/// - **Completed**: all steps finished (terminal)
/// - **Collapsed**: unrecoverable error (terminal)
#[doc(hidden)]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoleculeStatus {
    /// Molecule created but not assigned to a worker — inert until assigned.
    Pending,
    /// Assigned to a worker but not yet executing.
    Queued,
    /// Actively being worked on by a worker.
    #[serde(alias = "active")]
    Running,
    /// Molecule execution is suspended.
    Frozen,
    /// External authority refused service — quota exhausted, rate-limited,
    /// or otherwise out of compute budget (ADR-062). Peer of `Stalled`:
    /// different cause, different repair. `Starved` invites a wait or a
    /// rotation; **never a re-prompt** — re-prompting a starved molecule
    /// is wasted exergy and may compound the throttle.
    Starved,
    /// All steps finished successfully (terminal).
    Completed,
    /// Molecule encountered an unrecoverable error (terminal).
    Collapsed,
}

impl MoleculeStatus {
    /// Every variant, for exhaustive iteration — the peer of
    /// [`Phase::ALL`].
    ///
    /// This enum is `#[non_exhaustive]`, so downstream crates cannot match
    /// it exhaustively and their "for every status" tests degrade into
    /// hand-maintained arrays: an array does not fail to compile when a
    /// variant is added, so the coverage silently lapses on precisely the
    /// day the new variant needed testing. Iterating this const instead
    /// means such a test picks the new variant up for free.
    ///
    /// [`ordinal`](Self::ordinal) is what keeps this honest — adding a
    /// variant breaks *its* match, in the crate that owns the enum, two
    /// lines from the array that must grow.
    pub const ALL: [Self; 7] = [
        Self::Pending,
        Self::Queued,
        Self::Running,
        Self::Frozen,
        Self::Starved,
        Self::Completed,
        Self::Collapsed,
    ];

    /// This status's index in [`ALL`](Self::ALL).
    ///
    /// Exists to fail the build when a variant is added without extending
    /// `ALL` — the wildcard-free match below is the guard, and the
    /// round-trip is asserted in this module's tests. `#[non_exhaustive]`
    /// only binds *other* crates, so this match is checked for
    /// exhaustiveness here, which is the whole point.
    #[must_use]
    pub fn ordinal(self) -> usize {
        match self {
            Self::Pending => 0,
            Self::Queued => 1,
            Self::Running => 2,
            Self::Frozen => 3,
            Self::Starved => 4,
            Self::Completed => 5,
            Self::Collapsed => 6,
        }
    }

    /// Returns `true` if the molecule can be evolved (advanced to next step).
    #[must_use]
    pub fn is_evolvable(self) -> bool {
        matches!(self, Self::Running)
    }

    /// Returns `true` if the molecule is in a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Collapsed)
    }

    /// Returns `true` if the molecule is alive (not terminal).
    #[must_use]
    pub fn is_alive(self) -> bool {
        !self.is_terminal()
    }

    /// Returns `true` if transitioning from `self` to `to` is valid.
    ///
    /// The valid transitions mirror the typestate machine. The
    /// `Pending → Running` arm was dropped when the typestate lift
    /// (`Molecule<Pending>::tackle`) made the in-band path a
    /// compile-time guarantee; verification-side allowance of that
    /// transition lives in `cosmon_verify::invariants` so the event
    /// log continues to pass.
    ///
    /// - Pending → Collapsed (reject before execution)
    /// - Running → Completed (all steps done)
    /// - Running → Collapsed (unrecoverable error)
    /// - Running → Frozen (suspend)
    /// - Running → Starved (external quota refused, ADR-062)
    /// - Frozen → Running (thaw)
    /// - Starved → Running (refresh restored funding, ADR-062)
    /// - Starved → Collapsed (operator-collapsed after starvation, ADR-062)
    #[must_use]
    pub fn can_transition_to(self, to: Self) -> bool {
        matches!(
            (self, to),
            (Self::Frozen | Self::Starved, Self::Running)
                | (
                    Self::Pending | Self::Running | Self::Starved,
                    Self::Collapsed
                )
                | (
                    Self::Running,
                    Self::Completed | Self::Frozen | Self::Starved
                )
        )
    }

    /// Emoji glyph for this status — used in CLI output and docs.
    #[must_use]
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Pending => "⏳",
            Self::Queued => "📋",
            Self::Running => "▶️",
            Self::Frozen => "❄️",
            Self::Starved => "🥀",
            Self::Completed => "✅",
            Self::Collapsed => "💥",
        }
    }

    /// The operator-facing [`Phase`] this status belongs to.
    ///
    /// This is the only classification of `MoleculeStatus` into
    /// operator-facing categories. Before it existed, five separate
    /// hand-written tables in `cs peek` partitioned the same domain, each
    /// ending in a `_ =>` wildcard, each free to disagree — and all five
    /// did, most visibly on `Starved`, which one table filed as archive,
    /// another as dead, another as parked, and another as failed.
    ///
    /// The match below has **no wildcard arm**, and that is the whole
    /// point. `#[non_exhaustive]` is a promise to downstream crates, never
    /// a shield upstream: adding a variant to `MoleculeStatus` must fail to
    /// compile *here*, at exactly one site, so the author who adds a status
    /// names its phase in the same commit.
    #[must_use]
    pub fn phase(self) -> Phase {
        match self {
            Self::Running => Phase::Live,
            Self::Pending | Self::Queued => Phase::Waiting,
            // ADR-062: an external authority refused service. The molecule
            // is alive and the repair is a wait or a rotation — never a
            // re-prompt. It is the one status whose entire purpose is to
            // summon the operator, so it must never band with the archive.
            Self::Starved => Phase::Blocked,
            Self::Frozen => Phase::Parked,
            Self::Collapsed => Phase::Failed,
            Self::Completed => Phase::Done,
        }
    }

    /// Is this molecule *stuck* — frozen with a `stuck_at` stamp on it?
    ///
    /// `stuck` := `frozen` ∧ `stuck_at.is_some()`. The word is not new and
    /// it is not a rename: `cs stuck` is an operator verb that already
    /// ships, and `cosmon-runtime`'s scheduler already acts on exactly this
    /// distinction (`resident.rs`, the release rule) — a *delivered* freeze
    /// releases its dependents, a *stuck* freeze does not. Two things the
    /// scheduler has always separated; only the observer rendered them as
    /// one word.
    ///
    /// The word previously named `Starved`, which it never described:
    /// starvation is an external authority refusing service, and it clears
    /// itself when the quota refreshes. Nothing about it is stuck.
    #[must_use]
    pub fn is_stuck(self, stuck_at: Option<DateTime<Utc>>) -> bool {
        matches!(self, Self::Frozen) && stuck_at.is_some()
    }
}

/// The operator-facing category of a molecule — the codomain that five
/// hand-written classifications in `cs peek` were each inventing privately.
///
/// A `Phase` answers *"what is this molecule to me right now?"*, where
/// [`MoleculeStatus`] answers *"what has the runtime recorded about it?"*.
/// The two are distinct: `Queued` and `Pending` are different facts with
/// the same answer ([`Phase::Waiting`]), and the operator has no gesture
/// that tells them apart.
///
/// Obtained only via [`MoleculeStatus::phase`], which is total. There is no
/// second way to compute a phase, by construction — that absence is the
/// feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// A worker is on it right now (`Running`).
    Live,
    /// Nucleated, not yet started — waiting for `cs tackle`
    /// (`Pending`, `Queued`).
    Waiting,
    /// Alive, but an external authority is refusing service (`Starved`,
    /// ADR-062). Rotate or wait; never re-prompt.
    Blocked,
    /// Suspended by an operator gesture and reversible with `cs thaw`
    /// (`Frozen`).
    Parked,
    /// Ended in an unrecoverable error (`Collapsed`).
    Failed,
    /// Ended successfully (`Completed`).
    Done,
}

impl Phase {
    /// Every phase, in the order the operator reads them: the live work
    /// first, the archive last.
    pub const ALL: [Self; 6] = [
        Self::Live,
        Self::Waiting,
        Self::Blocked,
        Self::Parked,
        Self::Failed,
        Self::Done,
    ];

    /// Returns `true` when the molecule's story is over — nothing the
    /// operator does will move it again.
    ///
    /// Agrees with [`MoleculeStatus::is_terminal`] for every status, and a
    /// test in this module holds the two in lockstep. `Parked` is *not*
    /// terminal: a frozen molecule is one `cs thaw` from running.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Done)
    }

    /// Lowercase label, matching the vocabulary the operator reads in the
    /// table and types at the prompt.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Parked => "parked",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl MoleculeStatus {
    /// The canonical `snake_case` label — the same word [`Display`](Self),
    /// serde, and [`FromStr`] all use.
    ///
    /// Exhaustive and wildcard-free: a new variant must be named here or
    /// the crate does not build, which is why no caller needs a fallback.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Frozen => "frozen",
            Self::Starved => "starved",
            Self::Completed => "completed",
            Self::Collapsed => "collapsed",
        }
    }
}

impl fmt::Display for MoleculeStatus {
    /// Renders the `snake_case` label, **honouring the formatter's width and
    /// alignment**.
    ///
    /// `f.pad` rather than `f.write_str`, and the difference is not
    /// cosmetic. A `Display` impl that writes straight to the formatter
    /// silently discards `{:<10}` — the padding is not applied and nothing
    /// warns, because width is advisory and only `pad` consults it. That is
    /// exactly what happened to the canonical fleet snapshot: this enum
    /// replaced a `&str` in a `format!("{:<10}", …)` column (`str`'s own
    /// `Display` calls `pad`), the STATUS column silently lost its padding,
    /// and every column to its right slid out from under its header while
    /// the line still measured 120 bytes wide. A fixed-width contract that
    /// no longer aligns, at full width, with no error.
    ///
    /// Any `Display` for a value that might land in a table must pad.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for MoleculeStatus {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "queued" => Ok(Self::Queued),
            "running" | "active" => Ok(Self::Running),
            "frozen" => Ok(Self::Frozen),
            "starved" => Ok(Self::Starved),
            "completed" => Ok(Self::Completed),
            "collapsed" => Ok(Self::Collapsed),
            _ => Err(ParseEnumError {
                type_name: "MoleculeStatus",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// CollapseCause — structured cause attribution for `cs collapse` (ADR-062)
// ---------------------------------------------------------------------------

/// Structured cause for a collapse event (ADR-062 minimum hook).
///
/// Recorded on `MoleculeData.collapse_cause` and emitted in the event log
/// alongside the free-form `reason` string. The structured form lets `cs
/// peek` and `cs observe` surface the right ghost label
/// ([`crate::run_state::GhostKind::QuotaExhausted`] vs the existing five
/// drift shapes) without re-parsing the reason text.
///
/// `#[non_exhaustive]` so future causes (e.g. `OutOfMemory`,
/// `ProcessKilled`) can land without breaking external matches.
///
/// **Wire format.** Tagged JSON with `kind` and per-variant payload:
///
/// ```json
/// { "kind": "rate_limit", "account": "you", "kind_quota": "max_rolling_5h" }
/// { "kind": "manual" }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CollapseCause {
    /// External authority refused service — a quota was exhausted (ADR-062).
    /// Carries the account alias and the named quota currency that fired.
    ///
    /// Surfaced by `cs peek` as a `QuotaExhausted` ghost; the operator's
    /// repair is **wait or rotate**, never re-prompt.
    RateLimit {
        /// Account alias that hit the cap (e.g. `"you"`). Free-form so
        /// galaxies can carry their own naming convention.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
        /// Named quota currency: `max_rolling_5h`, `max_weekly`,
        /// `api_key_org_monthly`, `financial_usd`, `custody_scoped`, etc.
        /// Free-form to remain extensible across providers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind_quota: Option<String>,
    },
    /// Worker stopped emitting tokens but is still alive — inference
    /// stalled. Repair is a re-prompt or `cs whisper`.
    InferenceStall,
    /// Operator manually collapsed the molecule.
    Manual,
    /// Worker process died (OOM, crash, signal). Repair is restart.
    ProcessDeath,
    /// Cause could not be classified — the legacy default.
    Unknown,
}

impl CollapseCause {
    /// Stable short name for logging and CSV output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RateLimit { .. } => "rate_limit",
            Self::InferenceStall => "inference_stall",
            Self::Manual => "manual",
            Self::ProcessDeath => "process_death",
            Self::Unknown => "unknown",
        }
    }

    /// Returns `true` if this cause is rate-limit / quota-exhaustion shaped.
    /// Used by [`crate::run_state::GhostKind`] derivation.
    #[must_use]
    pub fn is_rate_limit(&self) -> bool {
        matches!(self, Self::RateLimit { .. })
    }
}

impl fmt::Display for CollapseCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CollapseCause {
    type Err = ParseEnumError;

    /// Parse a flag-friendly cause without payload (e.g. from `--cause`).
    /// Use the struct constructors directly when account/kind are known.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rate_limit" | "ratelimit" => Ok(Self::RateLimit {
                account: None,
                kind_quota: None,
            }),
            "inference_stall" | "inferencestall" => Ok(Self::InferenceStall),
            "manual" => Ok(Self::Manual),
            "process_death" | "processdeath" => Ok(Self::ProcessDeath),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseEnumError {
                type_name: "CollapseCause",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Typestate marker types
// ---------------------------------------------------------------------------

mod sealed {
    pub trait Sealed {}
}

/// Trait bound for molecule state markers. Sealed — only the four states
/// defined in this module implement it.
pub trait MoleculeState: sealed::Sealed {
    /// The corresponding serializable status variant.
    fn status() -> MoleculeStatus;
}

/// Pending state — molecule nucleated, no worker assigned yet.
///
/// The only state reachable from [`Molecule::new`]. Lifts the TLA+
/// `Nucleate` action into the type system: the runtime distinction
/// between a freshly-nucleated molecule and a tackled one becomes a
/// compile-time distinction between `Molecule<Pending>` and
/// `Molecule<Active>`. `.evolve()` / `.freeze()` / `.collapse()` are
/// not available on `Molecule<Pending>` by construction — the caller
/// must tackle first.
#[derive(Debug, Clone)]
pub struct Pending {
    _private: PhantomData<()>,
}

/// Active state — molecule is executing steps.
#[derive(Debug, Clone)]
pub struct Active {
    _private: PhantomData<()>,
}

/// Frozen state — molecule execution is suspended.
#[derive(Debug, Clone)]
pub struct Frozen {
    _private: PhantomData<()>,
}

/// Completed state — all steps finished successfully. Terminal
/// in-process; transitions to [`Merged`] when the branch is
/// harvested via [`Molecule::done`].
#[derive(Debug, Clone)]
pub struct Completed {
    _private: PhantomData<()>,
}

/// Merged state — the completed molecule's branch was merged back to
/// its parent. Terminal-terminal. Carries the [`MergeEvidence`] so the
/// branch lineage is inspectable from the typestate; lifts the TLA+
/// `Done` action into the type system. `UnnamedMerge` (I9) becomes a
/// compile error in-band — only out-of-band `BypassMerge` remains, by
/// design the Gödel boundary documented in
/// `responses/tolnay.md` §5.
#[derive(Debug, Clone)]
pub struct Merged {
    evidence: MergeEvidence,
}

/// Collapsed state — molecule encountered an unrecoverable error. Terminal.
///
/// Carries the failure reason and the step at which collapse occurred,
/// eliminating the need for `Option` + `expect()` on the `Molecule` struct.
#[derive(Debug, Clone)]
pub struct Collapsed {
    reason: String,
    step: usize,
}

impl sealed::Sealed for Pending {}
impl sealed::Sealed for Active {}
impl sealed::Sealed for Frozen {}
impl sealed::Sealed for Completed {}
impl sealed::Sealed for Merged {}
impl sealed::Sealed for Collapsed {}

impl MoleculeState for Pending {
    fn status() -> MoleculeStatus {
        MoleculeStatus::Pending
    }
}
impl MoleculeState for Active {
    fn status() -> MoleculeStatus {
        MoleculeStatus::Running
    }
}
impl MoleculeState for Frozen {
    fn status() -> MoleculeStatus {
        MoleculeStatus::Frozen
    }
}
impl MoleculeState for Completed {
    fn status() -> MoleculeStatus {
        MoleculeStatus::Completed
    }
}
/// A [`Merged`] molecule is still persisted as
/// [`MoleculeStatus::Completed`] — the persistence layer does not
/// carry a separate `Merged` variant (ADR-052 §D2 keeps
/// `MoleculeStatus` stable). The merge fact is recorded in
/// [`MergeEvidence`] on the typestate and in `MoleculeData::merged_at`
/// on disk.
impl MoleculeState for Merged {
    fn status() -> MoleculeStatus {
        MoleculeStatus::Completed
    }
}
impl MoleculeState for Collapsed {
    fn status() -> MoleculeStatus {
        MoleculeStatus::Collapsed
    }
}

// ---------------------------------------------------------------------------
// MergeEvidence — witness of the branch merge that retires a molecule
// ---------------------------------------------------------------------------

/// Proof-of-merge carried by [`Molecule<Merged>`].
///
/// A [`Molecule<Completed>`] can only reach [`Molecule<Merged>`] by
/// presenting a `MergeEvidence` — this is what forces the `Done`
/// action through the type system. Compare with the out-of-band
/// pathway (`git merge` outside `cs done`), which the type system
/// cannot observe and which surfaces at runtime as
/// [`crate::run_state::GhostKind::UnnamedMerge`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeEvidence {
    /// SHA of the merge commit on the parent branch.
    pub commit: String,
    /// Wall-clock timestamp at which the merge landed.
    pub merged_at: DateTime<Utc>,
}

impl MergeEvidence {
    /// Construct evidence with an explicit timestamp.
    #[must_use]
    pub fn new(commit: impl Into<String>, merged_at: DateTime<Utc>) -> Self {
        Self {
            commit: commit.into(),
            merged_at,
        }
    }

    /// Construct evidence stamped at `Utc::now()`.
    #[must_use]
    pub fn now(commit: impl Into<String>) -> Self {
        Self {
            commit: commit.into(),
            merged_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Log entry
// ---------------------------------------------------------------------------

/// A timestamped log entry recording a molecule lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Human-readable description of the event.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Molecule<S>
// ---------------------------------------------------------------------------

/// A molecule instance parameterised by its lifecycle state.
///
/// Common fields are shared across all states. State-specific data
/// (current step, failure reason, etc.) lives in dedicated fields that
/// are only accessible through the state-specific `impl` blocks.
///
/// The `PhantomData<S>` marker ensures the compiler tracks the state type
/// even though `S` is zero-sized. This is what makes invalid transitions
/// (e.g., `Molecule<Frozen>::evolve()`) into compile errors.
#[derive(Debug, Clone)]
pub struct Molecule<S: MoleculeState> {
    id: MoleculeId,
    formula_id: FormulaId,
    variables: HashMap<String, String>,
    assigned_worker: Option<WorkerId>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    log: Vec<LogEntry>,
    links: Vec<String>,
    // Step tracking
    total_steps: usize,
    current_step: usize,
    completed_steps: Vec<StepId>,
    // Typestate marker — stores the state directly so terminal states like
    // `Collapsed` can carry data (failure reason, step) without `Option`.
    state: S,
}

// --- Shared accessors (all states) ---

impl<S: MoleculeState> Molecule<S> {
    /// The molecule's unique identifier.
    #[must_use]
    pub fn id(&self) -> &MoleculeId {
        &self.id
    }

    /// The formula this molecule was instantiated from.
    #[must_use]
    pub fn formula_id(&self) -> &FormulaId {
        &self.formula_id
    }

    /// Variables bound when the molecule was created.
    #[must_use]
    pub fn variables(&self) -> &HashMap<String, String> {
        &self.variables
    }

    /// The worker currently assigned to this molecule, if any.
    #[must_use]
    pub fn assigned_worker(&self) -> Option<&WorkerId> {
        self.assigned_worker.as_ref()
    }

    /// When the molecule was created.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// When the molecule was last updated.
    #[must_use]
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    /// The lifecycle event log.
    #[must_use]
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// Links associated with this molecule.
    #[must_use]
    pub fn links(&self) -> &[String] {
        &self.links
    }

    /// The serializable status corresponding to the current typestate.
    #[must_use]
    #[allow(clippy::unused_self)] // &self makes the API consistent: mol.status()
    pub fn status(&self) -> MoleculeStatus {
        S::status()
    }

    /// The current step index (0-based).
    #[must_use]
    pub fn current_step(&self) -> usize {
        self.current_step
    }

    /// Steps that have been completed.
    #[must_use]
    pub fn completed_steps(&self) -> &[StepId] {
        &self.completed_steps
    }

    /// Total number of steps in this molecule's formula.
    #[must_use]
    pub fn total_steps(&self) -> usize {
        self.total_steps
    }

    // Internal: transfer common fields into a new state.
    fn transition_to<T: MoleculeState>(self, new_state: T) -> Molecule<T> {
        Molecule {
            id: self.id,
            formula_id: self.formula_id,
            variables: self.variables,
            assigned_worker: self.assigned_worker,
            created_at: self.created_at,
            updated_at: Utc::now(),
            log: self.log,
            links: self.links,
            total_steps: self.total_steps,
            current_step: self.current_step,
            completed_steps: self.completed_steps,
            state: new_state,
        }
    }

    fn push_log(&mut self, message: impl Into<String>) {
        self.log.push(LogEntry {
            timestamp: Utc::now(),
            message: message.into(),
        });
    }
}

// ---------------------------------------------------------------------------
// Molecule<Pending> — the only state reachable from `new`
// ---------------------------------------------------------------------------

impl Molecule<Pending> {
    /// Create a new molecule in the Pending state at step 0.
    ///
    /// Nucleate lands here, not in [`Active`]: the TLA+ `Nucleate →
    /// Tackle` chain is lifted into the type system as `Molecule::new
    /// → Molecule<Pending>::tackle`. `.evolve()` on a `Pending`
    /// molecule is a compile error, eliminating the class of runtime
    /// drift where a worker advanced a molecule before it was
    /// tackled.
    ///
    /// `total_steps` must be >= 1 (a molecule with zero steps is meaningless).
    ///
    /// # Panics
    /// Panics if `total_steps` is 0.
    #[must_use]
    pub fn new(id: MoleculeId, formula_id: FormulaId, total_steps: usize) -> Self {
        assert!(total_steps > 0, "a molecule must have at least one step");
        let now = Utc::now();
        let mut mol = Self {
            id,
            formula_id,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            log: Vec::new(),
            links: Vec::new(),
            total_steps,
            current_step: 0,
            completed_steps: Vec::new(),
            state: Pending {
                _private: PhantomData,
            },
        };
        mol.push_log("molecule created");
        mol
    }

    /// Add a variable binding during nucleation.
    pub fn set_variable(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.variables.insert(key.into(), value.into());
    }

    /// Entangle this molecule with another entity via a link.
    pub fn entangle(&mut self, link: impl Into<String>) {
        self.links.push(link.into());
    }

    /// Tackle the molecule — assign a worker and transition Pending →
    /// Active. This is the only path from `Pending` to `Active`; the
    /// type system forbids shortcuts.
    #[must_use]
    pub fn tackle(mut self, worker: WorkerId) -> Molecule<Active> {
        self.assigned_worker = Some(worker);
        self.push_log("tackled");
        self.transition_to(Active {
            _private: PhantomData,
        })
    }
}

// ---------------------------------------------------------------------------
// Molecule<Active>
// ---------------------------------------------------------------------------

/// Result of evolving an active molecule: either still active or completed.
#[derive(Debug)]
pub enum EvolveOutcome {
    /// Evolved to the next step; molecule remains active.
    Active(Molecule<Active>),
    /// Evolved past the last step; molecule is now completed.
    Completed(Molecule<Completed>),
}

impl Molecule<Active> {
    /// Evolve to the next step, marking the current step as completed.
    ///
    /// If the current step is the last step, the molecule transitions to
    /// `Completed`. Otherwise it remains `Active` at the next step.
    #[must_use]
    pub fn evolve(mut self, step_id: StepId) -> EvolveOutcome {
        self.completed_steps.push(step_id);
        self.push_log(format!("completed step {}", self.current_step));

        if self.current_step + 1 >= self.total_steps {
            // Last step — transition to Completed.
            self.push_log("all steps completed");
            let completed = self.transition_to(Completed {
                _private: PhantomData,
            });
            EvolveOutcome::Completed(completed)
        } else {
            self.current_step += 1;
            self.updated_at = Utc::now();
            self.push_log(format!("evolved to step {}", self.current_step));
            EvolveOutcome::Active(self)
        }
    }

    /// Transition to the Collapsed state, recording the reason and step.
    pub fn collapse(mut self, reason: impl Into<String>) -> Molecule<Collapsed> {
        let reason = reason.into();
        let step = self.current_step;
        self.push_log(format!("collapsed at step {step}: {reason}"));
        self.transition_to(Collapsed { reason, step })
    }

    /// Freeze execution. The molecule can later be thawed.
    #[must_use]
    pub fn freeze(mut self) -> Molecule<Frozen> {
        self.push_log(format!("frozen at step {}", self.current_step));
        self.transition_to(Frozen {
            _private: PhantomData,
        })
    }
}

// ---------------------------------------------------------------------------
// Molecule<Frozen>
// ---------------------------------------------------------------------------

impl Molecule<Frozen> {
    /// Thaw execution. Returns to the Active state at the same step.
    #[must_use]
    pub fn thaw(mut self) -> Molecule<Active> {
        self.push_log(format!("thawed at step {}", self.current_step));
        self.transition_to(Active {
            _private: PhantomData,
        })
    }
}

// ---------------------------------------------------------------------------
// Molecule<Collapsed> — terminal, read-only accessors
// ---------------------------------------------------------------------------

impl Molecule<Collapsed> {
    /// The reason the molecule collapsed.
    #[must_use]
    pub fn collapse_reason(&self) -> &str {
        &self.state.reason
    }

    /// The step index at which collapse occurred.
    #[must_use]
    pub fn collapsed_step(&self) -> usize {
        self.state.step
    }
}

// ---------------------------------------------------------------------------
// Molecule<Completed> — terminal in-process, can transition to Merged
// ---------------------------------------------------------------------------

impl Molecule<Completed> {
    /// Retire the molecule by recording that its branch was merged
    /// back to its parent.
    ///
    /// This is the typed form of `cs done`'s in-band merge: the
    /// caller must present a [`MergeEvidence`], which the type then
    /// carries forward. Merging a branch without going through this
    /// method is out-of-band (`git merge` from a random terminal) and
    /// surfaces at runtime as
    /// [`crate::run_state::GhostKind::UnnamedMerge`] — by design, per
    /// `responses/tolnay.md` §5.
    #[must_use]
    pub fn done(mut self, evidence: MergeEvidence) -> Molecule<Merged> {
        self.push_log(format!("merged via {}", evidence.commit));
        self.transition_to(Merged { evidence })
    }
}

// ---------------------------------------------------------------------------
// Molecule<Merged> — terminal-terminal, carries MergeEvidence
// ---------------------------------------------------------------------------

impl Molecule<Merged> {
    /// The evidence of the merge that retired this molecule.
    #[must_use]
    pub fn merge_evidence(&self) -> &MergeEvidence {
        &self.state.evidence
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` must list every variant exactly once. `ordinal`'s match is
    /// what makes adding a variant a compile error rather than a silent
    /// coverage gap; this pins the two to each other, so a variant added to
    /// `ordinal` but not to `ALL` fails here.
    #[test]
    fn all_lists_every_status_exactly_once() {
        for (i, s) in MoleculeStatus::ALL.into_iter().enumerate() {
            assert_eq!(s.ordinal(), i, "{s} is out of position in ALL");
        }
    }

    /// `Display` must honour width and alignment.
    ///
    /// Regression gate for a live defect: this enum landed in a
    /// `format!("{:<10}", …)` column of the canonical fleet snapshot,
    /// replacing a `&str`. Because the impl wrote straight to the formatter
    /// instead of calling `f.pad`, the width was silently ignored — the
    /// STATUS column lost its padding and every column right of it slid out
    /// from under its header, while each line still measured exactly 120
    /// bytes. Nothing failed loudly; a fixed-width contract just stopped
    /// lining up.
    ///
    /// The snapshot test in `cosmon-observability` catches this only
    /// incidentally, and only for the statuses its fixture happens to use.
    /// This pins the property itself, for every variant.
    #[test]
    fn display_honours_width_and_alignment() {
        for s in MoleculeStatus::ALL {
            let label = s.as_str();
            assert_eq!(
                format!("{s:<10}"),
                format!("{label:<10}"),
                "{s} ignores left-alignment — Display must call f.pad, not write_str",
            );
            assert_eq!(
                format!("{s:>12}"),
                format!("{label:>12}"),
                "{s} ignores >12"
            );
            // Unpadded rendering must be unchanged — `pad` only engages when
            // the caller asks for a width.
            assert_eq!(format!("{s}"), label);
        }
    }

    fn test_mol_id() -> MoleculeId {
        MoleculeId::new("test-20260401-abcd").unwrap()
    }

    fn test_formula_id() -> FormulaId {
        FormulaId::new("mol-polecat-work").unwrap()
    }

    fn step(n: usize) -> StepId {
        StepId::new(format!("step-{n}")).unwrap()
    }

    fn test_worker() -> WorkerId {
        WorkerId::new("onyx").unwrap()
    }

    /// Shortcut for tests that don't care about the Pending nuance.
    fn tackled(total_steps: usize) -> Molecule<Active> {
        Molecule::new(test_mol_id(), test_formula_id(), total_steps).tackle(test_worker())
    }

    // -- MoleculeStatus serialization (preserved from prior impl) --

    #[test]
    fn test_molecule_status_display_roundtrip() {
        for status in [
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Starved,
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
        ] {
            let s = status.to_string();
            let parsed: MoleculeStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_molecule_status_starved_is_alive_not_terminal() {
        // ADR-062: Starved is non-terminal — refresh restores Running.
        assert!(!MoleculeStatus::Starved.is_terminal());
        assert!(MoleculeStatus::Starved.is_alive());
        assert!(!MoleculeStatus::Starved.is_evolvable());
    }

    // -- Phase: the named codomain (delib-20260716-a2f1 C4) --

    /// Every status this binary knows. Kept beside the tests that fold over
    /// it; `phase()` is what the compiler holds exhaustive.
    const EVERY_STATUS: [MoleculeStatus; 7] = [
        MoleculeStatus::Pending,
        MoleculeStatus::Queued,
        MoleculeStatus::Running,
        MoleculeStatus::Frozen,
        MoleculeStatus::Starved,
        MoleculeStatus::Completed,
        MoleculeStatus::Collapsed,
    ];

    #[test]
    fn phase_terminality_agrees_with_status_terminality() {
        // The two predicates must never drift: `Phase::is_terminal` is what
        // the filter reads, `MoleculeStatus::is_terminal` is what the rest
        // of the core reads, and an operator seeing one answer from the
        // table and another from `cs observe` is the bug this whole design
        // exists to remove.
        for status in EVERY_STATUS {
            assert_eq!(
                status.is_terminal(),
                status.phase().is_terminal(),
                "{status} disagrees with its phase {} on terminality",
                status.phase(),
            );
        }
    }

    #[test]
    fn starved_phases_blocked_never_done() {
        // The headline regression. `Starved` is the one status whose whole
        // purpose is to summon the operator (ADR-062), and every private
        // classification in `cs peek` used to file it with the archive.
        assert_eq!(MoleculeStatus::Starved.phase(), Phase::Blocked);
        assert!(!MoleculeStatus::Starved.phase().is_terminal());
    }

    #[test]
    fn frozen_phases_parked_and_stays_reachable() {
        // A frozen molecule is one `cs thaw` from running, so it is not
        // terminal and the default view must not hide it.
        assert_eq!(MoleculeStatus::Frozen.phase(), Phase::Parked);
        assert!(!MoleculeStatus::Frozen.phase().is_terminal());
    }

    #[test]
    fn pending_and_queued_share_one_phase() {
        // Two different facts, one operator answer: neither has started and
        // the gesture for both is `cs tackle`.
        assert_eq!(MoleculeStatus::Pending.phase(), Phase::Waiting);
        assert_eq!(MoleculeStatus::Queued.phase(), Phase::Waiting);
    }

    #[test]
    fn only_completed_and_collapsed_are_terminal_phases() {
        let terminal: Vec<_> = EVERY_STATUS
            .into_iter()
            .filter(|s| s.phase().is_terminal())
            .collect();
        assert_eq!(
            terminal,
            vec![MoleculeStatus::Completed, MoleculeStatus::Collapsed],
        );
    }

    #[test]
    fn phase_all_covers_every_status() {
        // `Phase::ALL` is hand-written, so this holds it against the one
        // total function that the compiler does check.
        for status in EVERY_STATUS {
            assert!(
                Phase::ALL.contains(&status.phase()),
                "{status} phases to {}, which is missing from Phase::ALL",
                status.phase(),
            );
        }
    }

    #[test]
    fn stuck_is_frozen_with_a_stamp_and_nothing_else() {
        let stamp = Some(Utc::now());
        assert!(MoleculeStatus::Frozen.is_stuck(stamp));
        // A delivered freeze carries no stamp — the scheduler releases its
        // dependents, so calling it stuck would be a lie.
        assert!(!MoleculeStatus::Frozen.is_stuck(None));
        // And the status the word used to name is not stuck at all.
        assert!(!MoleculeStatus::Starved.is_stuck(stamp));
        for status in EVERY_STATUS {
            if status != MoleculeStatus::Frozen {
                assert!(!status.is_stuck(stamp), "{status} must never read stuck");
            }
        }
    }

    #[test]
    fn test_collapse_cause_rate_limit_roundtrip() {
        let cause = CollapseCause::RateLimit {
            account: Some("you".to_owned()),
            kind_quota: Some("max_rolling_5h".to_owned()),
        };
        assert!(cause.is_rate_limit());
        assert_eq!(cause.as_str(), "rate_limit");

        let json = serde_json::to_string(&cause).unwrap();
        let back: CollapseCause = serde_json::from_str(&json).unwrap();
        assert_eq!(cause, back);
    }

    #[test]
    fn test_collapse_cause_from_str_naked_rate_limit() {
        // `--cause rate_limit` parses without payload; account / kind
        // are then layered in by the CLI from `--account` / `--kind`.
        let cause: CollapseCause = "rate_limit".parse().unwrap();
        assert_eq!(
            cause,
            CollapseCause::RateLimit {
                account: None,
                kind_quota: None
            }
        );
    }

    // -- Typestate tests --

    #[test]
    fn test_evolve_moves_to_next_step() {
        let mol = tackled(3);
        assert_eq!(mol.current_step(), 0);
        assert_eq!(mol.status(), MoleculeStatus::Running);

        match mol.evolve(step(0)) {
            EvolveOutcome::Active(mol) => {
                assert_eq!(mol.current_step(), 1);
                assert_eq!(mol.completed_steps().len(), 1);
                assert_eq!(mol.status(), MoleculeStatus::Running);
            }
            EvolveOutcome::Completed(_) => panic!("should still be active"),
        }
    }

    #[test]
    fn test_evolve_last_step_returns_completed() {
        let mol = tackled(2);

        // Evolve step 0
        let mol = match mol.evolve(step(0)) {
            EvolveOutcome::Active(m) => m,
            EvolveOutcome::Completed(_) => panic!("should still be active after step 0"),
        };
        assert_eq!(mol.current_step(), 1);

        // Evolve step 1 (last step) — should complete
        match mol.evolve(step(1)) {
            EvolveOutcome::Completed(mol) => {
                assert_eq!(mol.status(), MoleculeStatus::Completed);
                assert_eq!(mol.completed_steps().len(), 2);
            }
            EvolveOutcome::Active(_) => panic!("should be completed after last step"),
        }
    }

    #[test]
    fn test_collapse_captures_reason() {
        let mol = tackled(3);

        // Evolve to step 1, then collapse
        let mol = match mol.evolve(step(0)) {
            EvolveOutcome::Active(m) => m,
            EvolveOutcome::Completed(_) => panic!("should still be active"),
        };

        let collapsed = mol.collapse("build broke");
        assert_eq!(collapsed.status(), MoleculeStatus::Collapsed);
        assert_eq!(collapsed.collapse_reason(), "build broke");
        assert_eq!(collapsed.collapsed_step(), 1);
        assert_eq!(collapsed.completed_steps().len(), 1);
    }

    #[test]
    fn test_freeze_thaw_roundtrip() {
        let mol = tackled(3);

        // Evolve to step 1
        let mol = match mol.evolve(step(0)) {
            EvolveOutcome::Active(m) => m,
            EvolveOutcome::Completed(_) => panic!("should still be active"),
        };
        assert_eq!(mol.current_step(), 1);

        // Freeze
        let frozen = mol.freeze();
        assert_eq!(frozen.status(), MoleculeStatus::Frozen);
        assert_eq!(frozen.current_step(), 1);

        // Thaw
        let active = frozen.thaw();
        assert_eq!(active.status(), MoleculeStatus::Running);
        assert_eq!(active.current_step(), 1);
        assert_eq!(active.completed_steps().len(), 1);
    }

    // -- Compile-fail documentation --
    //
    // The following DOES NOT COMPILE, proving typestate safety:
    //
    // ```compile_fail
    // let mol = Molecule::new(test_mol_id(), test_formula_id(), 3);
    // let frozen = mol.freeze();
    // frozen.evolve(step(0)); // ERROR: no method named `evolve` found for `Molecule<Frozen>`
    // ```
    //
    // Similarly, terminal states have no transition methods:
    //
    // ```compile_fail
    // let mol = Molecule::new(test_mol_id(), test_formula_id(), 1);
    // if let EvolveOutcome::Completed(c) = mol.evolve(step(0)) {
    //     c.evolve(step(1)); // ERROR: no method named `evolve` found for `Molecule<Completed>`
    // }
    // ```

    #[test]
    fn test_single_step_molecule_completes_immediately() {
        let mol = tackled(1);
        match mol.evolve(step(0)) {
            EvolveOutcome::Completed(c) => {
                assert_eq!(c.completed_steps().len(), 1);
            }
            EvolveOutcome::Active(_) => panic!("single-step molecule should complete"),
        }
    }

    #[test]
    fn test_new_molecule_fields() {
        let mol = Molecule::new(test_mol_id(), test_formula_id(), 3);
        assert_eq!(mol.id().as_str(), "test-20260401-abcd");
        assert_eq!(mol.formula_id().as_str(), "mol-polecat-work");
        assert!(mol.assigned_worker().is_none());
        assert!(mol.variables().is_empty());
        assert!(mol.links().is_empty());
        assert_eq!(mol.total_steps(), 3);
        assert_eq!(mol.status(), MoleculeStatus::Pending);
        assert!(!mol.log().is_empty()); // has "molecule created" entry
    }

    #[test]
    fn test_tackle_and_variables() {
        let mut mol = Molecule::new(test_mol_id(), test_formula_id(), 2);
        mol.set_variable("base_branch", "main");
        mol.entangle("cs-48l".to_owned());

        let worker = WorkerId::new("onyx").unwrap();
        let active = mol.tackle(worker);
        assert_eq!(active.status(), MoleculeStatus::Running);
        assert_eq!(active.assigned_worker().unwrap().as_str(), "onyx");
        assert_eq!(active.variables().get("base_branch").unwrap(), "main");
        assert_eq!(active.links(), &["cs-48l"]);
    }

    #[test]
    fn test_done_records_merge_evidence() {
        let mol = tackled(1);
        let completed = match mol.evolve(step(0)) {
            EvolveOutcome::Completed(c) => c,
            EvolveOutcome::Active(_) => unreachable!("single-step molecule completes"),
        };
        let evidence = MergeEvidence::new("deadbeef".to_owned(), Utc::now());
        let merged = completed.done(evidence.clone());
        // `Merged` projects to `Completed` in the legacy serde surface
        // so persistence does not grow a new variant.
        assert_eq!(merged.status(), MoleculeStatus::Completed);
        assert_eq!(merged.merge_evidence(), &evidence);
    }

    #[test]
    #[should_panic(expected = "at least one step")]
    fn test_zero_steps_panics() {
        let _ = Molecule::new(test_mol_id(), test_formula_id(), 0);
    }

    // -- can_transition_to --

    // -- can_transition_to --
    //
    // Post-typestate-lift (Chantier 2, task-20260419-a64f):
    // `Pending → Running` was deleted from the allowlist because
    // `Molecule<Pending>::tackle` now enforces it at compile time.
    // Verification-side tolerance of that event lives in
    // `cosmon_verify::invariants` so the on-disk event log keeps
    // passing.

    #[test]
    fn test_valid_transitions() {
        use MoleculeStatus::*;
        let valid = [
            (Pending, Collapsed),
            (Running, Completed),
            (Running, Collapsed),
            (Running, Frozen),
            (Running, Starved),
            (Frozen, Running),
            (Starved, Running),
            (Starved, Collapsed),
        ];
        for (from, to) in valid {
            assert!(from.can_transition_to(to), "{from} → {to} should be valid");
        }
    }

    #[test]
    fn test_invalid_transitions_exhaustive() {
        use MoleculeStatus::*;
        let all = [
            Pending, Queued, Running, Frozen, Starved, Completed, Collapsed,
        ];
        let valid = [
            (Pending, Collapsed),
            (Running, Completed),
            (Running, Collapsed),
            (Running, Frozen),
            (Running, Starved),
            (Frozen, Running),
            (Starved, Running),
            (Starved, Collapsed),
        ];
        for from in all {
            for to in all {
                let expected = valid.contains(&(from, to));
                assert_eq!(
                    from.can_transition_to(to),
                    expected,
                    "{from} → {to}: expected {expected}"
                );
            }
        }
    }

    #[test]
    fn test_pending_to_running_is_compile_time_only() {
        // The typestate lift means Pending→Running is no longer a
        // runtime arm of `can_transition_to` — it is a compile-time
        // transition via `Molecule<Pending>::tackle`. Guard against
        // accidentally re-introducing the runtime arm.
        assert!(!MoleculeStatus::Pending.can_transition_to(MoleculeStatus::Running));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn arb_molecule_status() -> impl Strategy<Value = MoleculeStatus> {
        prop_oneof![
            Just(MoleculeStatus::Pending),
            Just(MoleculeStatus::Queued),
            Just(MoleculeStatus::Running),
            Just(MoleculeStatus::Frozen),
            Just(MoleculeStatus::Starved),
            Just(MoleculeStatus::Completed),
            Just(MoleculeStatus::Collapsed),
        ]
    }

    proptest! {
        /// Terminal states have no valid outgoing transitions.
        #[test]
        fn terminal_states_reject_all_transitions(
            to in arb_molecule_status()
        ) {
            prop_assert!(!MoleculeStatus::Completed.can_transition_to(to));
            prop_assert!(!MoleculeStatus::Collapsed.can_transition_to(to));
        }

        /// Self-transitions are never valid.
        #[test]
        fn self_transitions_are_invalid(
            status in arb_molecule_status()
        ) {
            prop_assert!(!status.can_transition_to(status));
        }

        /// Any valid transition goes from a non-terminal state to a different state.
        #[test]
        fn valid_transitions_come_from_non_terminal(
            from in arb_molecule_status(),
            to in arb_molecule_status()
        ) {
            if from.can_transition_to(to) {
                prop_assert!(!from.is_terminal(), "terminal state {from} should not allow transitions");
                prop_assert_ne!(from, to, "self-transition should not be valid");
            }
        }
    }
}
