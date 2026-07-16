// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical run-state — one ledger, one writer per field, one witness.
//!
//! This module implements ADR-052 — the single-source-of-truth type for a
//! molecule's runtime state. It retires the three-source drift pattern
//! (`fleet.desired` + `tmux has-session` + `molecule.status`) in favour of a
//! single `RunState { intent, witness }` struct.
//!
//! # The vision sentence
//!
//! *Cosmon is a filesystem that remembers which worker owns which decision,
//! so no one — not even the pilot — can answer in the worker's place.*
//!
//! # Writer discipline (I2 — `SingleWriterPerField`)
//!
//! | Field | Writer |
//! |---|---|
//! | [`Intent`] | pilot only (`cs tackle`, `cs freeze`, `cs stop`) |
//! | [`Witness`] | probe only (`pane-died` hook, `cs patrol` pure-observation) |
//!
//! Writing a field from a role that does not own it is a contract breach.
//! The Rust API enforces this partially via
//! [`RunState::write_intent`] and [`RunState::record_witness`]; the rest is
//! discipline enforced by command perimeters (see `docs/adr/052-*.md` §D3).
//!
//! # Detection, not prevention (I9 — Gödel)
//!
//! [`RunState::ghost`] pattern-matches the observed drift shapes onto the
//! named [`GhostKind`] variants. Every one of the nine ghosts of 18–19 April
//! maps to one of these variants. The function is pure — it takes no I/O —
//! and is meant to be called by the reconciler, the patrol, and the peek
//! TUI at every natural transition.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{MoleculeId, WorkerId};

// ---------------------------------------------------------------------------
// Intent — what the pilot wrote (I2, pilot writer)
// ---------------------------------------------------------------------------

/// Terminal outcome carried by [`Intent::Terminal`].
///
/// Split from [`Intent`] so that non-terminal intents and terminal ones are
/// syntactically distinct at the point of pattern-matching. Downstream
/// reconciliation logic almost always wants to branch on terminal vs.
/// non-terminal first.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Terminus {
    /// The worker finished all its steps successfully.
    Completed,
    /// The worker or the pilot declared the molecule unrecoverable.
    Collapsed,
    /// The molecule's branch was merged into the parent (terminal-terminal).
    Merged,
}

/// The pilot's declared intention for a molecule.
///
/// Persisted. The pilot (human operator or meta-agent) is the only actor
/// allowed to write [`Intent`]. Every path that writes it must go through
/// [`RunState::write_intent`].
///
/// `Terminal` carries the outcome so the ledger can distinguish
/// `Completed`, `Collapsed`, and `Merged` without a second field.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// The pilot wants the worker to be running and evolving.
    Run,
    /// The pilot wants the worker frozen (graceful shutdown preserving state).
    Pause,
    /// The pilot wants the worker gone (teardown without preservation).
    Stop,
    /// The pilot has recorded a terminal decision.
    Terminal(Terminus),
}

impl Intent {
    /// `true` if this intent is one of the terminal variants.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

// ---------------------------------------------------------------------------
// Witness — what an external probe saw (I2, probe writer; I8, emission rule)
// ---------------------------------------------------------------------------

/// Process liveness as reported by an external probe.
///
/// A reader seeing `Alive` must consult [`Witness::observed_at`] and reject
/// the reading as `Unknown` if the probe is staler than its TTL
/// (cf. I10 — `SilenceIsSignal`). `Alive` is never strictly true — it is
/// "true as of N seconds ago".
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Probe observed the worker process alive.
    Alive,
    /// Probe observed the worker process dead or missing.
    Dead,
    /// Probe ran but could not determine (backend down, I/O error).
    Unknown,
}

/// Branch merge state as reported by an external probe (typically `git`).
///
/// The provenance of a merge is recorded via the ledger (I8 — probes emit
/// `*Probed` events before action). An unnamed merge — a merge commit with
/// no corresponding `Completed` molecule in the ledger — is the c1cb ghost.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchState {
    /// The worker's branch exists and has not been merged into parent.
    Unmerged,
    /// The worker's branch has been merged into parent.
    Merged,
    /// The worker's branch does not exist (never created, or deleted
    /// after merge).
    Absent,
}

/// A single external observation of a molecule's runtime reality.
///
/// Emitted *only* by external probes:
///
/// * the tmux `pane-died` hook writes a witness with `process = Dead`;
/// * `cs patrol` in pure-observation mode writes witnesses at each sweep;
/// * `cs project` may consult `git` to set `branch`.
///
/// A witness is a trace — it is never the source of intent. Writing a
/// witness from a pilot path is a contract breach.
///
/// # Freshness
///
/// `observed_at` is load-bearing: a consumer must check it against a TTL
/// before treating `Alive` as authoritative (I10).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    /// Wall-clock timestamp of the observation.
    pub observed_at: DateTime<Utc>,
    /// What the probe saw about the worker process.
    pub process: Liveness,
    /// What the probe saw about the worker's branch.
    pub branch: BranchState,
}

impl Witness {
    /// Construct a fresh witness at `observed_at = now`.
    #[must_use]
    pub fn new(process: Liveness, branch: BranchState) -> Self {
        Self {
            observed_at: Utc::now(),
            process,
            branch,
        }
    }

    /// Construct a witness with an explicit `observed_at` timestamp.
    ///
    /// Used by migration code and tests where the timestamp must be
    /// reconstructed from legacy state. Prefer [`Witness::new`] for
    /// fresh probe emissions.
    #[must_use]
    pub fn at(observed_at: DateTime<Utc>, process: Liveness, branch: BranchState) -> Self {
        Self {
            observed_at,
            process,
            branch,
        }
    }

    /// `true` if the witness is older than `ttl`.
    ///
    /// Used by consumers to decide whether a recorded `Alive` should be
    /// demoted to `Unknown` (I10 — silence-as-signal).
    #[must_use]
    pub fn is_stale(&self, now: DateTime<Utc>, ttl: Duration) -> bool {
        match now.signed_duration_since(self.observed_at).to_std() {
            Ok(age) => age > ttl,
            // Negative duration (witness from the future?) — treat as fresh.
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// RunState — the canonical type (ADR-052 §D2)
// ---------------------------------------------------------------------------

/// The single authoritative runtime state of a molecule.
///
/// Replaces the three pre-existing truth sources (`fleet.desired`,
/// `tmux has-session`, `molecule.status`) by separating *intention* (what
/// the pilot declared) from *measurement* (what a probe saw). The two
/// fields have one writer each; any attempt to act on [`Intent::Run`]
/// without a fresh [`Witness`] is a [`GhostKind::VanishedWorker`].
///
/// # JSON shape
///
/// ```json
/// {
///   "intent": "run",
///   "witness": {
///     "observed_at": "2026-04-19T14:23:00Z",
///     "process": "alive",
///     "branch": "unmerged"
///   }
/// }
/// ```
///
/// Terminal intents serialize as tagged enums:
///
/// ```json
/// { "intent": { "terminal": "completed" }, "witness": null }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    /// What the pilot wrote. Persisted. See [`Intent`].
    pub intent: Intent,
    /// Last observation recorded by an external probe. `None` means the
    /// system has never observed this molecule's reality.
    pub witness: Option<Witness>,
}

impl RunState {
    /// Construct a fresh `RunState` at the given intent with no witness yet.
    #[must_use]
    pub fn new(intent: Intent) -> Self {
        Self {
            intent,
            witness: None,
        }
    }

    /// Start a molecule whose pilot has just declared `Intent::Run`.
    ///
    /// Convenience for the common case (`cs tackle`).
    #[must_use]
    pub fn running() -> Self {
        Self::new(Intent::Run)
    }

    /// Construct a `RunState` that has already been observed.
    #[must_use]
    pub fn with_witness(intent: Intent, witness: Witness) -> Self {
        Self {
            intent,
            witness: Some(witness),
        }
    }

    // ---- writer-discipline helpers ---------------------------------------

    /// Write a new intent — pilot role only.
    ///
    /// The runtime must never call this directly from a worker path; see
    /// ADR-052 §D3 and the command-perimeter table in
    /// `docs/architectural-invariants.md`. The function itself is a pure
    /// setter — the writer-role boundary is enforced by which binary
    /// crosses it, not by a runtime check.
    pub fn write_intent(&mut self, intent: Intent) {
        self.intent = intent;
    }

    /// Record a fresh witness — probe role only.
    ///
    /// Replaces any existing witness. Consumers that want monotonic witness
    /// history read the event ledger, not this field.
    pub fn record_witness(&mut self, witness: Witness) {
        self.witness = Some(witness);
    }

    // ---- queries ---------------------------------------------------------

    /// `true` if the pilot's current intent is terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.intent.is_terminal()
    }

    /// Returns the terminus if `intent` is terminal, else `None`.
    #[must_use]
    pub fn terminus(&self) -> Option<Terminus> {
        match self.intent {
            Intent::Terminal(t) => Some(t),
            _ => None,
        }
    }

    /// Detect drift by pattern-matching the intent/witness pair against the
    /// nine named ghost shapes.
    ///
    /// Pure, total, O(1). Every one of the 18–19 April ghosts maps to a
    /// `GhostKind` variant (see `docs/adr/052-*.md` §D2 and the
    /// [`crate::run_state::tests`] nine-ghost regression suite).
    ///
    /// `now` and `probe_ttl` gate the [`GhostKind::StaleProbe`] variant —
    /// a witness older than `probe_ttl` is stale, regardless of what it
    /// reported. `probe_ttl` is a policy knob; a safe default is 90 s for
    /// patrol-driven probes and 10 s for hook-driven probes.
    ///
    /// Returns `None` when the run-state is internally consistent.
    #[must_use]
    pub fn ghost(&self, now: DateTime<Utc>, probe_ttl: Duration) -> Option<GhostKind> {
        // I5 — UnHarvested: Terminal::Completed with branch still Unmerged.
        if let Intent::Terminal(Terminus::Completed) = self.intent {
            if let Some(w) = &self.witness {
                if matches!(w.branch, BranchState::Unmerged) {
                    return Some(GhostKind::UnHarvested);
                }
            }
        }

        // I9 — UnnamedMerge: a merged branch with non-terminal intent
        //      (someone merged outside the state machine; c1cb).
        if let Some(w) = &self.witness {
            if matches!(w.branch, BranchState::Merged) && !self.intent.is_terminal() {
                return Some(GhostKind::UnnamedMerge);
            }
        }

        // I4 — DeadPane: pilot wants Run but the last witness said Dead
        //      (the "alarm clock buzzing on the empty apron"; dfd8).
        if matches!(self.intent, Intent::Run) {
            if let Some(w) = &self.witness {
                if matches!(w.process, Liveness::Dead) {
                    return Some(GhostKind::DeadPane);
                }
            }
        }

        // I10 — StaleProbe: Intent::Run, witness reported Alive, but the
        //       witness is older than its TTL — we can no longer trust it.
        if matches!(self.intent, Intent::Run) {
            if let Some(w) = &self.witness {
                if matches!(w.process, Liveness::Alive) && w.is_stale(now, probe_ttl) {
                    return Some(GhostKind::StaleProbe);
                }
            }
        }

        // I3 — VanishedWorker: Intent::Run with no witness at all
        //      (the registry believes the worker is alive but nobody ever
        //       asked; 192a-like).
        if matches!(self.intent, Intent::Run) && self.witness.is_none() {
            return Some(GhostKind::VanishedWorker);
        }

        None
    }
}

// ---------------------------------------------------------------------------
// GhostKind — the five detected drift shapes
// ---------------------------------------------------------------------------

/// A named drift shape surfaced by [`RunState::ghost`].
///
/// Every one of the 9 ghosts of 18–19 April maps to exactly one variant.
/// Pattern-exhaustive matching is intentionally NOT guaranteed —
/// [`#[non_exhaustive]`] keeps this open for future empirical shapes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhostKind {
    /// I4 — Pilot intent is `Run`, last witness reports `Dead`
    /// (e.g. worker OOM-killed, pane closed).
    DeadPane,
    /// I3 — Pilot intent is `Run`, but no witness has ever been recorded
    /// (e.g. fleet entry alive, tmux gone).
    VanishedWorker,
    /// I5 — Pilot intent is `Terminal(Completed)`, branch still `Unmerged`
    /// (the morning-after ghost: status completed, nobody called
    /// `cs done`).
    UnHarvested,
    /// I10 — Pilot intent is `Run`, last witness reported `Alive`, but the
    /// witness is older than its TTL.
    StaleProbe,
    /// I9 — Branch is `Merged` but pilot intent is not terminal
    /// (e.g. pilot rebased + force-pushed outside the state machine, or an
    /// inline-reply shortcut that bypasses the contract).
    UnnamedMerge,
    /// `I_QuotaProgress` — molecule is in `MoleculeStatus::Starved` or
    /// was collapsed with `CollapseCause::RateLimit`. External authority
    /// refused service (e.g. a provider's rolling rate-limit cap). Repair
    /// is **wait or rotate**, never
    /// re-prompt — re-prompting compounds the throttle and burns more
    /// quota. ADR-062.
    QuotaExhausted,
}

impl GhostKind {
    /// Human-readable short name — used in CLI and log output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeadPane => "dead-pane",
            Self::VanishedWorker => "vanished-worker",
            Self::UnHarvested => "un-harvested",
            Self::StaleProbe => "stale-probe",
            Self::UnnamedMerge => "unnamed-merge",
            Self::QuotaExhausted => "quota-exhausted",
        }
    }
}

impl std::fmt::Display for GhostKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// DriftError — drift as Result::Err at the API boundary
// ---------------------------------------------------------------------------

/// Drift surfaced at an API boundary — the error form of [`GhostKind`].
///
/// Thrown by higher-level crates (`cosmon-state`, `cosmon-cli`) when a
/// command cannot proceed because the run-plane is inconsistent. Keeps the
/// `Result`-returning API contract explicit (clippy `result_large_err`
/// friendly — each variant carries a small payload).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriftError {
    /// Pilot intent is [`Intent::Run`] but no recent witness exists —
    /// corresponds to [`GhostKind::DeadPane`] or [`GhostKind::VanishedWorker`]
    /// or [`GhostKind::StaleProbe`] depending on what the witness says.
    #[error(
        "intent is {intent:?} for worker {worker:?} but witness is missing or stale (last_seen={last_seen:?})"
    )]
    IntentWithoutWitness {
        /// Optional worker id — not every intent writes carry a worker.
        worker: Option<WorkerId>,
        /// The intent that has no backing witness.
        intent: Intent,
        /// Timestamp of the most recent witness, if any.
        last_seen: Option<DateTime<Utc>>,
    },
    /// Pilot recorded [`Terminus::Completed`] but the branch is not merged —
    /// corresponds to [`GhostKind::UnHarvested`].
    #[error("molecule {molecule} is Completed but branch {branch} is not merged")]
    TerminalUnmerged {
        /// The completed molecule.
        molecule: MoleculeId,
        /// Branch name that remains un-merged.
        branch: String,
    },
    /// Two probes tried to record a witness at the same time — event-log
    /// integrity violation (I7 — `SingleEventWriter`). The caller should
    /// retry after the file-lock releases.
    #[error("concurrent witness writers on {path}: {writers} contenders")]
    ConcurrentWitness {
        /// The events.jsonl or state file under contention.
        path: PathBuf,
        /// Number of concurrent writers observed.
        writers: u32,
    },
}

// ---------------------------------------------------------------------------
// Legacy bridges — conversions from the three pre-existing truth sources.
//
// These live here (rather than on the legacy types) so that (a) the legacy
// modules can stay oblivious of `RunState`, and (b) there is a single place
// to audit the projection during the migration window.
// ---------------------------------------------------------------------------

use crate::molecule::MoleculeStatus;
use crate::worker::{DesiredState, TransportState, WorkerStatus};

impl From<MoleculeStatus> for Intent {
    fn from(s: MoleculeStatus) -> Self {
        match s {
            // Pending is technically "no intent written yet" — but the
            // migration path from fleet.json treats Pending as "the pilot
            // hasn't tackled yet", which is not the same as Run. We map it
            // to Pause (the most conservative non-terminal intent) so the
            // `ghost()` check does not flag VanishedWorker on freshly-
            // nucleated pending molecules.
            MoleculeStatus::Pending | MoleculeStatus::Queued | MoleculeStatus::Frozen => {
                Intent::Pause
            }
            // Starved keeps `Intent::Run` — the pilot still wants the
            // molecule to advance; the QuotaExhausted ghost is surfaced
            // separately by status-aware detectors (ADR-062).
            MoleculeStatus::Running | MoleculeStatus::Starved => Intent::Run,
            MoleculeStatus::Completed => Intent::Terminal(Terminus::Completed),
            MoleculeStatus::Collapsed => Intent::Terminal(Terminus::Collapsed),
        }
    }
}

impl From<DesiredState> for Intent {
    fn from(d: DesiredState) -> Self {
        match d {
            DesiredState::Running => Intent::Run,
            DesiredState::Paused => Intent::Pause,
            DesiredState::Stopped => Intent::Stop,
        }
    }
}

impl From<TransportState> for Liveness {
    fn from(t: TransportState) -> Self {
        match t {
            TransportState::Alive => Liveness::Alive,
            TransportState::Dead => Liveness::Dead,
            TransportState::Unknown => Liveness::Unknown,
        }
    }
}

/// Project a [`WorkerStatus`] onto [`Liveness`].
///
/// Used during the migration to recover a witness from legacy fleet.json
/// entries that only carry `WorkerStatus`. The projection is conservative:
/// anything that could indicate "alive" maps to `Alive`; explicit stopped
/// states map to `Dead`; everything else is `Unknown`.
#[must_use]
pub fn liveness_from_worker_status(s: &WorkerStatus) -> Liveness {
    match s {
        WorkerStatus::Active | WorkerStatus::Starting | WorkerStatus::Paused => Liveness::Alive,
        WorkerStatus::Stopped | WorkerStatus::Stopping | WorkerStatus::Stale => Liveness::Dead,
        WorkerStatus::Unresponsive | WorkerStatus::Error(_) => Liveness::Unknown,
    }
}

/// Reverse projection — the legacy [`MoleculeStatus`] derivable from a
/// [`RunState`].
///
/// Used while `MoleculeStatus` remains the persisted field (pre-migration).
/// The projection is lossy: `Pending` and `Queued` cannot be distinguished
/// from `Pause` without extra context.
#[must_use]
pub fn molecule_status_from_run_state(rs: &RunState) -> MoleculeStatus {
    match rs.intent {
        Intent::Run => MoleculeStatus::Running,
        Intent::Pause => MoleculeStatus::Frozen,
        Intent::Stop => MoleculeStatus::Pending,
        Intent::Terminal(Terminus::Completed | Terminus::Merged) => MoleculeStatus::Completed,
        Intent::Terminal(Terminus::Collapsed) => MoleculeStatus::Collapsed,
    }
}

/// Project a [`RunState`] from the three legacy truth sources for display.
///
/// Used by `cs ensemble` and `cs observe` to surface [`GhostKind`] markers
/// in the operator view without persisting a `RunState` on disk yet — the
/// migration of `MoleculeStatus` → `RunState` on the storage side is the
/// separate ADR-052 child #3 (events.jsonl integrity) and child #1
/// (migration in progress). Until that lands, display-side projection is
/// the only honest way to reuse the detection surface.
///
/// Inputs:
/// * `molecule_status` — what the persisted state says the pilot wrote.
/// * `transport` — the last tmux liveness probe.
/// * `branch_merged_at` — `Some(_)` iff the molecule's branch was merged
///   back to its parent; sourced from `MoleculeData::merged_at`.
/// * `observed_at` — timestamp of the probe. Callers that are observing
///   now pass `Utc::now()`.
#[must_use]
pub fn project_run_state(
    molecule_status: MoleculeStatus,
    transport: TransportState,
    branch_merged_at: Option<DateTime<Utc>>,
    observed_at: DateTime<Utc>,
) -> RunState {
    let intent = Intent::from(molecule_status);
    let process = Liveness::from(transport);
    let branch = if branch_merged_at.is_some() {
        BranchState::Merged
    } else {
        BranchState::Unmerged
    };
    let witness = Witness::at(observed_at, process, branch);
    RunState::with_witness(intent, witness)
}

// ---------------------------------------------------------------------------
// Tests — the 9-ghost regression suite (ADR-052 Consequences child #2)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ttl() -> Duration {
        Duration::from_secs(90)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn alive_now() -> Witness {
        Witness::new(Liveness::Alive, BranchState::Unmerged)
    }

    fn dead_now() -> Witness {
        Witness::new(Liveness::Dead, BranchState::Unmerged)
    }

    // -- Writer-role basics ---------------------------------------------------

    #[test]
    fn run_state_running_has_no_witness() {
        let rs = RunState::running();
        assert_eq!(rs.intent, Intent::Run);
        assert!(rs.witness.is_none());
    }

    #[test]
    fn run_state_write_intent_mutates_only_intent() {
        let mut rs = RunState::with_witness(Intent::Run, alive_now());
        let w_before = rs.witness.clone();
        rs.write_intent(Intent::Pause);
        assert_eq!(rs.intent, Intent::Pause);
        assert_eq!(rs.witness, w_before);
    }

    #[test]
    fn run_state_record_witness_mutates_only_witness() {
        let mut rs = RunState::running();
        let intent_before = rs.intent;
        rs.record_witness(alive_now());
        assert_eq!(rs.intent, intent_before);
        assert!(rs.witness.is_some());
    }

    #[test]
    fn run_state_record_witness_replaces() {
        let mut rs = RunState::running();
        rs.record_witness(alive_now());
        rs.record_witness(dead_now());
        assert_eq!(rs.witness.as_ref().unwrap().process, Liveness::Dead);
    }

    // -- Serde round-trip -----------------------------------------------------

    #[test]
    fn run_state_json_roundtrip_running() {
        let rs = RunState::running();
        let json = serde_json::to_string(&rs).unwrap();
        let back: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rs);
    }

    #[test]
    fn run_state_json_roundtrip_with_witness() {
        let rs = RunState::with_witness(Intent::Run, alive_now());
        let json = serde_json::to_string(&rs).unwrap();
        let back: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rs);
    }

    #[test]
    fn run_state_json_roundtrip_terminal() {
        let rs = RunState::new(Intent::Terminal(Terminus::Completed));
        let json = serde_json::to_string(&rs).unwrap();
        assert!(
            json.contains("\"terminal\""),
            "terminal intent should serialize as tagged enum: {json}"
        );
        let back: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rs);
    }

    #[test]
    fn witness_is_stale_after_ttl() {
        let past = Utc::now() - chrono::Duration::seconds(600);
        let w = Witness {
            observed_at: past,
            process: Liveness::Alive,
            branch: BranchState::Unmerged,
        };
        assert!(w.is_stale(Utc::now(), Duration::from_secs(60)));
    }

    #[test]
    fn witness_is_fresh_within_ttl() {
        let w = alive_now();
        assert!(!w.is_stale(Utc::now(), Duration::from_secs(60)));
    }

    // -- The 9 ghosts of 18–19 April (regression suite) -----------------------
    //
    // Each of the nine empirical ghosts in the 18–19 April log maps to
    // exactly one [`GhostKind`] variant. Every incident gets its own named
    // test — not because the shapes differ (the six mailroom `/ask`
    // ghosts share a shape), but because the audit trail from ADR-052 must
    // be readable cargo-test-side: an operator reading `cargo test` output
    // should see every molecule id that contributed to the invariant being
    // exercised.

    /// Ghost #1 — runtime emits Evolve against a phantom session. Pilot
    /// still says Run, last probe says Dead.
    /// *"The alarm clock buzzes on the empty apron."* (I4)
    #[test]
    fn ghost_1_cosmon_dfd8_dead_pane() {
        let rs = RunState::with_witness(Intent::Run, dead_now());
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::DeadPane));
    }

    /// Ghost #2 — mode-mismatch — molecule
    /// Completed, fleet still Registered, branch never merged. The
    /// morning-after ghost. (I5)
    #[test]
    fn ghost_2_cosmon_192a_un_harvested() {
        let w = Witness {
            observed_at: Utc::now(),
            process: Liveness::Alive,
            branch: BranchState::Unmerged,
        };
        let rs = RunState::with_witness(Intent::Terminal(Terminus::Completed), w);
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::UnHarvested));
    }

    /// Ghost #3 — pilot rebased + force-pushed
    /// inline, merge outside the state machine. Intent is Run (not
    /// terminal), branch shows Merged. *The* Gödel sentence (I9).
    #[test]
    fn ghost_3_cosmon_c1cb_unnamed_merge() {
        let w = Witness {
            observed_at: Utc::now(),
            process: Liveness::Alive,
            branch: BranchState::Merged,
        };
        let rs = RunState::with_witness(Intent::Run, w);
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::UnnamedMerge));
    }

    // -- Mailroom `/ask` six (ghosts #4–#9) --------------------------------
    //
    // Six molecules, identical shape: `cs nucleate` ran, `cs tackle` did
    // not, the pilot (Claude) read the transcripts and wrote the outbox
    // JSON by hand. From the cosmon-side projection: intent Run, no
    // witness ever recorded → [`GhostKind::VanishedWorker`]. Each id gets
    // its own named test so the 18–19 April audit trail is visible in
    // `cargo test` output.

    /// Ghost #4 — mailroom `/ask` d902: pilot answered without a worker.
    #[test]
    fn ghost_4_mailroom_d902_vanished_worker() {
        let rs = RunState::running();
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::VanishedWorker));
    }

    /// Ghost #5 — mailroom `/ask` 93a7: same shape, different id.
    #[test]
    fn ghost_5_mailroom_93a7_vanished_worker() {
        let rs = RunState::running();
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::VanishedWorker));
    }

    /// Ghost #6 — mailroom `/ask` af87: same shape, different id.
    #[test]
    fn ghost_6_mailroom_af87_vanished_worker() {
        let rs = RunState::running();
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::VanishedWorker));
    }

    /// Ghost #7 — mailroom `/ask` ffc1: same shape, different id.
    #[test]
    fn ghost_7_mailroom_ffc1_vanished_worker() {
        let rs = RunState::running();
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::VanishedWorker));
    }

    /// Ghost #8 — mailroom `/ask` b387: same shape, different id.
    #[test]
    fn ghost_8_mailroom_b387_vanished_worker() {
        let rs = RunState::running();
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::VanishedWorker));
    }

    /// Ghost #9 — mailroom `/ask` f2a3: same shape, different id.
    #[test]
    fn ghost_9_mailroom_f2a3_vanished_worker() {
        let rs = RunState::running();
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::VanishedWorker));
    }

    /// Ghost #10 (bonus) — `StaleProbe`: witness says Alive but the probe
    /// is older than its TTL. Not in the 18–19 April log, but the type
    /// admits it by construction and the variant must be reachable.
    #[test]
    fn ghost_bonus_stale_probe() {
        let past = Utc::now() - chrono::Duration::seconds(600);
        let w = Witness {
            observed_at: past,
            process: Liveness::Alive,
            branch: BranchState::Unmerged,
        };
        let rs = RunState::with_witness(Intent::Run, w);
        assert_eq!(
            rs.ghost(now(), Duration::from_secs(60)),
            Some(GhostKind::StaleProbe)
        );
    }

    /// Ghost #11 (ADR-062) — `QuotaExhausted`: surfaced by the
    /// status-aware detector when the molecule is `Starved` or was
    /// collapsed with `CollapseCause::RateLimit`. The variant itself is
    /// not derivable from `(intent, witness)` alone — the rate-limit
    /// fixture sat with `Intent::Run` + `Liveness::Alive`,
    /// indistinguishable from a healthy worker by the run-state alone.
    /// This test pins `as_str()` and the wire format so detectors agree
    /// on the spelling.
    #[test]
    fn ghost_11_quota_exhausted_wire_format() {
        assert_eq!(GhostKind::QuotaExhausted.as_str(), "quota-exhausted");
        let json = serde_json::to_string(&GhostKind::QuotaExhausted).unwrap();
        assert_eq!(json, "\"quota_exhausted\"");
        let back: GhostKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, GhostKind::QuotaExhausted);
    }

    /// Coverage sanity: the 9 named incident tests above reach exactly 3
    /// distinct `GhostKind` variants (`DeadPane`, `UnHarvested`,
    /// `VanishedWorker`). The bonus test covers the fourth variant
    /// (`StaleProbe`). `UnnamedMerge` is covered by ghost #3.
    /// Collectively, all five variants are exercised.
    #[test]
    fn nine_ghost_suite_covers_every_named_variant() {
        use std::collections::HashSet;

        // Reproduce each ghost from the 18–19 April log.
        let ghosts: Vec<GhostKind> = vec![
            // dfd8 (DeadPane), 192a (UnHarvested), c1cb (UnnamedMerge).
            {
                let rs = RunState::with_witness(Intent::Run, dead_now());
                rs.ghost(now(), ttl()).unwrap()
            },
            {
                let w = Witness::new(Liveness::Alive, BranchState::Unmerged);
                let rs = RunState::with_witness(Intent::Terminal(Terminus::Completed), w);
                rs.ghost(now(), ttl()).unwrap()
            },
            {
                let w = Witness::new(Liveness::Alive, BranchState::Merged);
                let rs = RunState::with_witness(Intent::Run, w);
                rs.ghost(now(), ttl()).unwrap()
            },
            // mailroom d902, 93a7, af87, ffc1, b387, f2a3 — six
            // VanishedWorker, one shape.
            RunState::running().ghost(now(), ttl()).unwrap(),
            RunState::running().ghost(now(), ttl()).unwrap(),
            RunState::running().ghost(now(), ttl()).unwrap(),
            RunState::running().ghost(now(), ttl()).unwrap(),
            RunState::running().ghost(now(), ttl()).unwrap(),
            RunState::running().ghost(now(), ttl()).unwrap(),
        ];

        assert_eq!(ghosts.len(), 9, "the 18–19 April log has nine incidents");
        let uniq: HashSet<_> = ghosts.iter().copied().collect();
        assert!(
            uniq.contains(&GhostKind::DeadPane),
            "dfd8 must map to DeadPane"
        );
        assert!(
            uniq.contains(&GhostKind::UnHarvested),
            "192a must map to UnHarvested"
        );
        assert!(
            uniq.contains(&GhostKind::UnnamedMerge),
            "c1cb must map to UnnamedMerge"
        );
        assert!(
            uniq.contains(&GhostKind::VanishedWorker),
            "mailroom six must map to VanishedWorker"
        );
        assert_eq!(
            uniq.len(),
            4,
            "the 9 empirical ghosts collapse to 4 named variants"
        );
    }

    // -- Happy-path regression: healthy state has no ghost --------------------

    #[test]
    fn healthy_running_is_not_ghost() {
        let rs = RunState::with_witness(Intent::Run, alive_now());
        assert_eq!(rs.ghost(now(), ttl()), None);
    }

    #[test]
    fn clean_completed_merged_is_not_ghost() {
        let w = Witness {
            observed_at: Utc::now(),
            process: Liveness::Dead,
            branch: BranchState::Merged,
        };
        let rs = RunState::with_witness(Intent::Terminal(Terminus::Merged), w);
        assert_eq!(rs.ghost(now(), ttl()), None);
    }

    #[test]
    fn paused_with_no_witness_is_not_ghost() {
        let rs = RunState::new(Intent::Pause);
        assert_eq!(rs.ghost(now(), ttl()), None);
    }

    #[test]
    fn stopped_with_no_witness_is_not_ghost() {
        let rs = RunState::new(Intent::Stop);
        assert_eq!(rs.ghost(now(), ttl()), None);
    }

    // -- Terminus semantics ---------------------------------------------------

    #[test]
    fn terminal_intent_is_terminal() {
        for t in [Terminus::Completed, Terminus::Collapsed, Terminus::Merged] {
            assert!(Intent::Terminal(t).is_terminal());
            let rs = RunState::new(Intent::Terminal(t));
            assert!(rs.is_terminal());
            assert_eq!(rs.terminus(), Some(t));
        }
    }

    #[test]
    fn non_terminal_intent_is_not_terminal() {
        for i in [Intent::Run, Intent::Pause, Intent::Stop] {
            assert!(!i.is_terminal());
            let rs = RunState::new(i);
            assert!(!rs.is_terminal());
            assert_eq!(rs.terminus(), None);
        }
    }

    // -- Legacy projection tests (migration safety) ---------------------------

    #[test]
    fn from_molecule_status_covers_every_variant() {
        use MoleculeStatus::*;
        assert_eq!(Intent::from(Pending), Intent::Pause);
        assert_eq!(Intent::from(Queued), Intent::Pause);
        assert_eq!(Intent::from(Running), Intent::Run);
        assert_eq!(Intent::from(Frozen), Intent::Pause);
        assert_eq!(
            Intent::from(Completed),
            Intent::Terminal(Terminus::Completed)
        );
        assert_eq!(
            Intent::from(Collapsed),
            Intent::Terminal(Terminus::Collapsed)
        );
    }

    #[test]
    fn from_desired_state_covers_every_variant() {
        assert_eq!(Intent::from(DesiredState::Running), Intent::Run);
        assert_eq!(Intent::from(DesiredState::Paused), Intent::Pause);
        assert_eq!(Intent::from(DesiredState::Stopped), Intent::Stop);
    }

    #[test]
    fn from_transport_state_covers_every_variant() {
        assert_eq!(Liveness::from(TransportState::Alive), Liveness::Alive);
        assert_eq!(Liveness::from(TransportState::Dead), Liveness::Dead);
        assert_eq!(Liveness::from(TransportState::Unknown), Liveness::Unknown);
    }

    #[test]
    fn liveness_from_worker_status_conservative() {
        assert_eq!(
            liveness_from_worker_status(&WorkerStatus::Active),
            Liveness::Alive
        );
        assert_eq!(
            liveness_from_worker_status(&WorkerStatus::Stopped),
            Liveness::Dead
        );
        assert_eq!(
            liveness_from_worker_status(&WorkerStatus::Unresponsive),
            Liveness::Unknown
        );
        assert_eq!(
            liveness_from_worker_status(&WorkerStatus::Error("oops".into())),
            Liveness::Unknown
        );
    }

    #[test]
    fn project_run_state_maps_running_alive_to_unmerged_run() {
        let rs = project_run_state(
            MoleculeStatus::Running,
            TransportState::Alive,
            None,
            Utc::now(),
        );
        assert_eq!(rs.intent, Intent::Run);
        let w = rs.witness.as_ref().unwrap();
        assert_eq!(w.process, Liveness::Alive);
        assert_eq!(w.branch, BranchState::Unmerged);
        assert_eq!(rs.ghost(now(), ttl()), None);
    }

    #[test]
    fn project_run_state_running_dead_is_dead_pane() {
        let rs = project_run_state(
            MoleculeStatus::Running,
            TransportState::Dead,
            None,
            Utc::now(),
        );
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::DeadPane));
    }

    #[test]
    fn project_run_state_completed_unmerged_is_un_harvested() {
        let rs = project_run_state(
            MoleculeStatus::Completed,
            TransportState::Alive,
            None,
            Utc::now(),
        );
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::UnHarvested));
    }

    #[test]
    fn project_run_state_running_merged_is_unnamed_merge() {
        let rs = project_run_state(
            MoleculeStatus::Running,
            TransportState::Alive,
            Some(Utc::now()),
            Utc::now(),
        );
        assert_eq!(rs.ghost(now(), ttl()), Some(GhostKind::UnnamedMerge));
    }

    #[test]
    fn project_run_state_pending_without_worker_is_not_ghost() {
        let rs = project_run_state(
            MoleculeStatus::Pending,
            TransportState::Dead,
            None,
            Utc::now(),
        );
        // Pending maps to Pause; Pause + dead probe is not a ghost (the
        // pilot never said Run).
        assert_eq!(rs.ghost(now(), ttl()), None);
    }

    #[test]
    fn molecule_status_reverse_projection_covers_every_intent() {
        assert_eq!(
            molecule_status_from_run_state(&RunState::new(Intent::Run)),
            MoleculeStatus::Running
        );
        assert_eq!(
            molecule_status_from_run_state(&RunState::new(Intent::Pause)),
            MoleculeStatus::Frozen
        );
        assert_eq!(
            molecule_status_from_run_state(&RunState::new(Intent::Stop)),
            MoleculeStatus::Pending
        );
        assert_eq!(
            molecule_status_from_run_state(&RunState::new(Intent::Terminal(Terminus::Completed))),
            MoleculeStatus::Completed
        );
        assert_eq!(
            molecule_status_from_run_state(&RunState::new(Intent::Terminal(Terminus::Merged))),
            MoleculeStatus::Completed
        );
        assert_eq!(
            molecule_status_from_run_state(&RunState::new(Intent::Terminal(Terminus::Collapsed))),
            MoleculeStatus::Collapsed
        );
    }

    // -- Drift error shape ---------------------------------------------------

    #[test]
    fn drift_error_display_includes_fields() {
        let err = DriftError::IntentWithoutWitness {
            worker: None,
            intent: Intent::Run,
            last_seen: None,
        };
        let s = err.to_string();
        assert!(s.contains("Run"));
    }

    // -- GhostKind text surface ---------------------------------------------

    #[test]
    fn ghost_kind_names_are_distinct_and_stable() {
        use GhostKind::*;
        let names = [
            DeadPane.as_str(),
            VanishedWorker.as_str(),
            UnHarvested.as_str(),
            StaleProbe.as_str(),
            UnnamedMerge.as_str(),
        ];
        let mut uniq: Vec<&&str> = names.iter().collect();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), 5, "all ghost names must be distinct");
    }

    #[test]
    fn ghost_kind_serde_roundtrip() {
        for g in [
            GhostKind::DeadPane,
            GhostKind::VanishedWorker,
            GhostKind::UnHarvested,
            GhostKind::StaleProbe,
            GhostKind::UnnamedMerge,
        ] {
            let s = serde_json::to_string(&g).unwrap();
            let back: GhostKind = serde_json::from_str(&s).unwrap();
            assert_eq!(back, g);
        }
    }
}
