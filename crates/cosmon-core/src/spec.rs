// SPDX-License-Identifier: AGPL-3.0-only

//! Pure Rust mirror of `docs/specs/CosmonRun.tla` — single-molecule `SpecState`.
//!
//! This module exists solely as a **refinement target** for the impl-side
//! proptest conformance gate (`crates/cosmon-core/tests/spec_conformance.rs`).
//! It is a byte-for-byte translation of the 13-action disjunction in
//! `CosmonRun.tla` §`Next` (lines 60–148) and the 7 state variables declared
//! on lines 36–42. The spec file is the authority: when the two diverge,
//! fix this module — not the TLA+.
//!
//! # What this mirrors
//!
//! The TLA+ spec ranges over `Mol`, a bounded set of molecules. The
//! conformance harness only needs the single-molecule case (all documented
//! counterexamples in `docs/specs/VALIDATION-REPORT.md` have single-molecule
//! witnesses up to renaming), so `SpecState` collapses the `Mol -> X`
//! functions to plain fields. The single-molecule entry point is
//! [`SpecState::init_single`].
//!
//! # Non-goals
//!
//! * This is **not** the runtime state used by the CLI. That lives in
//!   [`crate::run_state`] (refined type, witness/intent split).
//! * This module must not grow I/O, fleet lookups, or CLI wiring. It is
//!   a pure transition system. Adding anything else is a refactor bug.
//! * No multi-molecule ordering tests — `stateright` is the right tool
//!   when those become necessary.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// State enums — the codomains of the 7 TLA+ variables
// ---------------------------------------------------------------------------

/// Mirror of the TLA+ `StatusValues` codomain (line 214).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Status {
    /// Molecule does not yet exist in the ledger.
    Absent,
    /// `Nucleate` fired; molecule is queued but not tackled.
    Pending,
    /// `Tackle` fired; a worker is driving the molecule.
    Running,
    /// `Complete` fired; worker has signalled success.
    Completed,
    /// `Collapse` fired; molecule is terminal-failed.
    Collapsed,
    /// `Freeze` fired; molecule is suspended mid-flight.
    Frozen,
}

impl Status {
    /// `true` iff status ∈ {Completed, Collapsed} — the Gödel-sanctioned
    /// terminal set of `I9_BranchMergedOnlyIfCompleted`.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Collapsed)
    }
}

/// Mirror of the TLA+ `FleetValues` codomain (line 215).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FleetValue {
    /// Fleet carries no registration for this molecule.
    None,
    /// Fleet has a live registration — `Tackle` sets this.
    Registered,
}

/// Mirror of the TLA+ `LockValues` codomain (line 216).
///
/// Two-valued by construction — this is the structural proof of I7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LockValue {
    /// No writer holds the lock.
    None,
    /// The worker holds the lock (set by `Evolve`, cleared by `LockRelease`).
    Worker,
}

// ---------------------------------------------------------------------------
// Action — the 13 variants of the TLA+ `Next` disjunction (lines 152–155)
// ---------------------------------------------------------------------------

/// One of the 13 actions in the TLA+ `Next` disjunction.
///
/// The ordering matches `CosmonRun.tla:153–155` verbatim: the ten in-band
/// cosmon CLI actions first, then the three out-of-band environment actions.
/// Adding a variant means the TLA+ spec grew a disjunct and a fresh model
/// run is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Action {
    /// `Nucleate(m)` — lines 58–61.
    Nucleate,
    /// `Tackle(m)` — lines 63–68.
    Tackle,
    /// `Evolve(m)` — lines 70–77.
    Evolve,
    /// `Complete(m)` — lines 79–84.
    Complete,
    /// `Done(m)` — lines 88–92.
    Done,
    /// `Collapse(m)` — lines 94–99.
    Collapse,
    /// `Freeze(m)` — lines 101–104.
    Freeze,
    /// `Thaw(m)` — lines 106–109.
    Thaw,
    /// `LockRelease(m)` — lines 111–114.
    LockRelease,
    /// `Purge(m)` — lines 118–123.
    Purge,
    /// `TmuxCrash(m)` — lines 127–131. Out-of-band environment action.
    TmuxCrash,
    /// `ProcessCrash(m)` — lines 133–137. Out-of-band environment action.
    ProcessCrash,
    /// `BypassMerge(m)` — lines 144–148. Adversarial environment action;
    /// when enabled, falsifies `I9_BranchMergedOnlyIfCompleted` in 2 steps.
    BypassMerge,
}

impl Action {
    /// All 13 actions in spec order — used by proptest strategies.
    pub const ALL: [Action; 13] = [
        Action::Nucleate,
        Action::Tackle,
        Action::Evolve,
        Action::Complete,
        Action::Done,
        Action::Collapse,
        Action::Freeze,
        Action::Thaw,
        Action::LockRelease,
        Action::Purge,
        Action::TmuxCrash,
        Action::ProcessCrash,
        Action::BypassMerge,
    ];

    /// The 12-action alphabet Σ ∖ {`BypassMerge`}. The positive form of I9
    /// (a branch merges only after the molecule has completed) holds as a
    /// theorem over this sanctioned set: it is only the adversarial
    /// `BypassMerge` action, excluded here, that can falsify it.
    pub const SANCTIONED: [Action; 12] = [
        Action::Nucleate,
        Action::Tackle,
        Action::Evolve,
        Action::Complete,
        Action::Done,
        Action::Collapse,
        Action::Freeze,
        Action::Thaw,
        Action::LockRelease,
        Action::Purge,
        Action::TmuxCrash,
        Action::ProcessCrash,
    ];
}

// ---------------------------------------------------------------------------
// SpecState — single-molecule projection of the 7 TLA+ variables
// ---------------------------------------------------------------------------

/// Single-molecule mirror of the 7 TLA+ state variables (lines 36–42).
///
/// `SpecState` is the unit of conformance: every proptest trace runs on
/// one fresh `SpecState::init_single()` and applies a sequence of actions.
/// Mol-indexed tests can allocate a `Vec<SpecState>` and run per-index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecState {
    status: Status,
    fleet_desired: FleetValue,
    tmux_session: bool,
    worker_pid_alive: bool,
    branch_merged: bool,
    events_seqno: u32,
    events_writer_lock: LockValue,
    max_seqno: u32,
}

impl SpecState {
    /// Default bound for `events_seqno` — matches `MaxSeqno` in the TLC
    /// configs (large enough that random traces stay unblocked without
    /// letting the variable diverge in long-running nightly runs).
    pub const DEFAULT_MAX_SEQNO: u32 = 64;

    /// Fresh state for a single molecule. Every field matches the TLA+
    /// `Init` predicate (lines 47–54).
    #[must_use]
    pub fn init_single() -> Self {
        Self::with_max_seqno(Self::DEFAULT_MAX_SEQNO)
    }

    /// Like [`Self::init_single`] but with a caller-chosen `MaxSeqno`.
    #[must_use]
    pub fn with_max_seqno(max_seqno: u32) -> Self {
        Self {
            status: Status::Absent,
            fleet_desired: FleetValue::None,
            tmux_session: false,
            worker_pid_alive: false,
            branch_merged: false,
            events_seqno: 0,
            events_writer_lock: LockValue::None,
            max_seqno,
        }
    }

    // ---- Accessors (one per TLA+ variable) --------------------------------

    /// `mol_status[m]` — the molecule's lifecycle status.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status
    }

    /// `fleet_desired[m]` — does the fleet hold a registration?
    #[must_use]
    pub fn fleet_desired(&self) -> FleetValue {
        self.fleet_desired
    }

    /// `tmux_session[m]` — is a tmux session alive?
    #[must_use]
    pub fn tmux_session(&self) -> bool {
        self.tmux_session
    }

    /// `worker_pid_alive[m]` — is the worker process alive?
    #[must_use]
    pub fn worker_pid_alive(&self) -> bool {
        self.worker_pid_alive
    }

    /// `branch_merged[m]` — has the worker's branch landed on `main`?
    #[must_use]
    pub fn branch_merged(&self) -> bool {
        self.branch_merged
    }

    /// `events_seqno[m]` — monotone event counter, bounded by `MaxSeqno`.
    #[must_use]
    pub fn events_seqno(&self) -> u32 {
        self.events_seqno
    }

    /// `events_writer_lock[m]` — the two-valued lock cell (I7).
    #[must_use]
    pub fn lock(&self) -> LockValue {
        self.events_writer_lock
    }

    // ---- Transition relation ---------------------------------------------

    /// Mirrors each TLA+ action's left-hand side (the action's enabling
    /// conjunction). 1:1 with `CosmonRun.tla:60–148`. The environment
    /// gates `AsyncCrashesEnabled` and `OutOfBandEnabled` are implicitly
    /// `TRUE` here: callers filter the action set themselves (see
    /// [`Action::SANCTIONED`]).
    // Allow identical-body match arms: preserving the 1:1 mapping to the
    // TLA+ `Next` disjunction is the whole point of this function. Merging
    // `Complete` + `Freeze` (both guarded by `status = Running`) would
    // hide the spec correspondence and break refactor safety.
    #[allow(clippy::match_same_arms)]
    #[must_use]
    pub fn enabled(&self, action: Action) -> bool {
        match action {
            Action::Nucleate => self.status == Status::Absent,
            Action::Tackle => self.status == Status::Pending,
            Action::Evolve => {
                self.status == Status::Running
                    && self.worker_pid_alive
                    && matches!(self.events_writer_lock, LockValue::None)
                    && self.events_seqno < self.max_seqno
            }
            Action::Complete => self.status == Status::Running,
            Action::Done => self.status == Status::Completed && !self.branch_merged,
            Action::Collapse => matches!(
                self.status,
                Status::Pending | Status::Running | Status::Frozen
            ),
            Action::Freeze => self.status == Status::Running,
            Action::Thaw => self.status == Status::Frozen,
            Action::LockRelease => !matches!(self.events_writer_lock, LockValue::None),
            Action::Purge => {
                matches!(self.fleet_desired, FleetValue::Registered) && !self.worker_pid_alive
            }
            Action::TmuxCrash => self.tmux_session,
            Action::ProcessCrash => self.worker_pid_alive,
            Action::BypassMerge => !self.branch_merged,
        }
    }

    /// Apply `action` to the state, mirroring each TLA+ action's primed
    /// assignment. If `action` is not [`enabled`](Self::enabled), the call
    /// is a no-op — matching TLA+ semantics where a disabled disjunct
    /// cannot fire.
    // Same rationale as `enabled`: keep the 1:1 mapping to TLA+ actions.
    // `Done` and `BypassMerge` share the `branch_merged = true` body but
    // are semantically distinct (sanctioned vs out-of-band writer).
    #[allow(clippy::match_same_arms)]
    pub fn step(&mut self, action: Action) {
        if !self.enabled(action) {
            return;
        }
        match action {
            Action::Nucleate => {
                self.status = Status::Pending;
            }
            Action::Tackle => {
                self.status = Status::Running;
                self.fleet_desired = FleetValue::Registered;
                self.tmux_session = true;
                self.worker_pid_alive = true;
            }
            Action::Evolve => {
                self.events_writer_lock = LockValue::Worker;
                self.events_seqno += 1;
            }
            Action::Complete => {
                self.status = Status::Completed;
                self.fleet_desired = FleetValue::None;
                self.tmux_session = false;
                self.worker_pid_alive = false;
            }
            Action::Done => {
                self.branch_merged = true;
            }
            Action::Collapse => {
                self.status = Status::Collapsed;
                self.fleet_desired = FleetValue::None;
                self.tmux_session = false;
                self.worker_pid_alive = false;
            }
            Action::Freeze => {
                self.status = Status::Frozen;
            }
            Action::Thaw => {
                self.status = Status::Running;
            }
            Action::LockRelease => {
                self.events_writer_lock = LockValue::None;
            }
            Action::Purge => {
                self.fleet_desired = FleetValue::None;
                self.tmux_session = false;
            }
            Action::TmuxCrash => {
                self.tmux_session = false;
            }
            Action::ProcessCrash => {
                self.worker_pid_alive = false;
            }
            Action::BypassMerge => {
                self.branch_merged = true;
            }
        }
    }
}

impl Default for SpecState {
    fn default() -> Self {
        Self::init_single()
    }
}

// ---------------------------------------------------------------------------
// Unit tests — the TLC counterexample traces as regression fixtures.
// Kept in-module so doc coverage and `cargo test -p cosmon-core` pick them
// up without requiring the integration harness in
// `tests/spec_conformance.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Init matches the TLA+ `Init` predicate verbatim.
    #[test]
    fn init_single_mirrors_tla_init() {
        let s = SpecState::init_single();
        assert_eq!(s.status(), Status::Absent);
        assert_eq!(s.fleet_desired(), FleetValue::None);
        assert!(!s.tmux_session());
        assert!(!s.worker_pid_alive());
        assert!(!s.branch_merged());
        assert_eq!(s.events_seqno(), 0);
        assert_eq!(s.lock(), LockValue::None);
    }

    /// Action alphabet has exactly 13 variants, matching `Next` disjunction.
    #[test]
    fn action_alphabet_has_thirteen_variants() {
        assert_eq!(Action::ALL.len(), 13);
        assert_eq!(Action::SANCTIONED.len(), 12);
        assert!(!Action::SANCTIONED.contains(&Action::BypassMerge));
    }

    /// Happy path: Nucleate → Tackle → Evolve → Complete → Done.
    #[test]
    fn happy_path_nucleate_to_done() {
        let mut s = SpecState::init_single();
        for a in [
            Action::Nucleate,
            Action::Tackle,
            Action::Evolve,
            Action::LockRelease,
            Action::Complete,
            Action::Done,
        ] {
            assert!(s.enabled(a), "{a:?} should be enabled");
            s.step(a);
        }
        assert_eq!(s.status(), Status::Completed);
        assert!(s.branch_merged());
    }
}
