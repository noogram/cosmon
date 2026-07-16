// SPDX-License-Identifier: AGPL-3.0-only

//! Transport-layer patrol types.
//!
//! The transport patrol is mechanical: it inspects the fleet, compares
//! against thresholds, and produces a [`PatrolReport`] with
//! [`PatrolAction`] recommendations. No AI reasoning, no token cost —
//! just measurement.
//!
//! See THESIS.md Part VII for the two-layer patrol design rationale.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{MoleculeId, WorkerId};
use crate::molecule::MoleculeStatus;
use crate::run_state::Liveness;

/// The output of a single transport patrol cycle.
///
/// Captures a snapshot of fleet health at a point in time. The patrol
/// logic is a pure function (fleet state in, report out) so that it
/// is testable without I/O.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatrolReport {
    /// When the patrol ran.
    pub timestamp: DateTime<Utc>,
    /// Total number of workers in the fleet.
    pub ensemble_size: usize,
    /// Workers that are idle (Starting or Stopped).
    pub idle_count: usize,
    /// Workers detected as stalled (Stale status).
    pub stalled_workers: Vec<WorkerId>,
    /// Workers in an error state.
    pub error_workers: Vec<WorkerId>,
    /// Active molecules assigned to workers that are dead or missing.
    pub orphaned_molecules: Vec<MoleculeId>,
    /// Recommended corrective actions.
    pub recommendations: Vec<PatrolAction>,
}

/// A corrective action recommended by the transport patrol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PatrolAction {
    /// Restart a worker that is stale or in error.
    RestartWorker {
        /// The worker to restart.
        worker_id: WorkerId,
        /// Why the restart is recommended.
        reason: String,
    },
    /// Reassign a molecule whose worker is dead.
    ReassignMolecule {
        /// The orphaned molecule.
        molecule_id: MoleculeId,
        /// The dead worker it was assigned to.
        dead_worker: WorkerId,
    },
    /// Alert a human — the patrol cannot resolve this automatically.
    AlertHuman {
        /// Description of the situation requiring human attention.
        message: String,
    },
    /// Everything is healthy; no action needed.
    NoAction,
}

impl PatrolReport {
    /// Whether the fleet is fully healthy (no issues detected).
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.stalled_workers.is_empty()
            && self.error_workers.is_empty()
            && self.orphaned_molecules.is_empty()
    }

    /// Total number of issues detected.
    #[must_use]
    pub fn issue_count(&self) -> usize {
        self.stalled_workers.len() + self.error_workers.len() + self.orphaned_molecules.len()
    }
}

// ===========================================================================
// The Witness — molecule-health anomaly classification (ADR-137 §4, §10)
// ===========================================================================
//
// The Witness is the L1 *detect* layer of the molecule-health primitive. It
// is a **pure function**: control-plane facts in (`[MoleculeHealthView]`), a
// classified [`HealthReport`] out. No I/O, no pane reads, no event-format
// coupling — the CLI shell folds `events.jsonl`, the liveness lease, the
// presence registry and the whisper log into the pre-digested boolean and
// timestamp fields of [`MoleculeHealthView`], and this function classifies.
//
// **The load-bearing discipline (ADR-137 §2).** Every signal this module
// consumes is *control-plane* state — a real molecule status, a liveness
// lease, a typed adapter exit code, an authorizing `Done` event. **Never** a
// pane glyph. The catastrophic be1e SEV-1 bug was a guard that recognised its
// target by a string the brief itself displays (`grep 'cs done'`,
// `grep '401'`): stating the rule more clearly *enlarged* the false-positive
// set. The structural cure is that [`MoleculeHealthView`] has **no field for
// rendered pane text** — a worker cannot make this function fire by *printing*
// the glyphs of the rule meant to police it. The deacon watches the state
// machine, not the screen.

/// The class of anomaly the Witness detected, keyed off control-plane state
/// (ADR-137 §4). The `A`-prefixed numbers in the docs map to the catalog rows.
///
/// `SelfDonePoised` (the prototype's "A2") is **intentionally absent**: a
/// worker physically cannot self-`cs done` (the `cs done` perimeter refuses a
/// worker-context caller, ADR-016), so there is nothing to detect — and the
/// prototype's pane-grep detector for it was the be1e SEV-1 bug. The perimeter
/// is the guard; deleting the detector is the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyClass {
    /// **A1** — prompt pasted into the worker but never submitted; the slot is
    /// occupied yet zero work happens. Detected from event-log non-growth
    /// since tackle, never from `grep '[Pasted text'`.
    UnsentPaste,
    /// **A3** — the session is auth-dead (e.g. a 401). Detected from a typed
    /// auth probe / adapter exit code, never from the bare substring `401`.
    AuthDead,
    /// **A4** — molecule `cs complete`d but its session lingers, holding a
    /// slot. Detected from `status == Completed` AND a live session.
    IdleAfterComplete,
    /// **A5** — alive session, no progress past the step's stall budget.
    IdleRunningZombie,
    /// **A6** — overloaded / rate-limited / `Starved`. Backoff, never collapse.
    Overloaded,
    /// **A7** — branch merged or molecule archived with no authorizing `Done`
    /// event from a non-worker caller. An *integrity* alarm — flag, never heal.
    GhostMerge,
    /// **A8** — `status == Completed` but never harvested (`archived == false`),
    /// independent of session presence.
    CompletedUnharvested,
    /// **A9** — worker crashed, `status` stuck `Running`, every downstream
    /// `cs wait` blind. Detected from liveness-lease expiry (ADR-116).
    CrashZombie,
    /// Alive worker with no durable output past the configured output budget.
    /// Advisory only: long reasoning can be legitimate, so this never gates
    /// lifecycle transitions or triggers an autonomous remedy.
    OutputStalled,
}

impl AnomalyClass {
    /// The stable catalog code (`"A1"`..`"A9"`) for this class.
    ///
    /// Used in human-facing output and as the escalation ring-buffer key
    /// (ADR-137 §6). Stable across renames of the variant.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsentPaste => "A1",
            Self::AuthDead => "A3",
            Self::IdleAfterComplete => "A4",
            Self::IdleRunningZombie => "A5",
            Self::Overloaded => "A6",
            Self::GhostMerge => "A7",
            Self::CompletedUnharvested => "A8",
            Self::CrashZombie => "A9",
            Self::OutputStalled => "OutputStalled",
        }
    }

    /// A one-line human description of the anomaly.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::UnsentPaste => "prompt pasted but never submitted (slot occupied, zero work)",
            Self::AuthDead => "session auth-dead (typed auth probe failed)",
            Self::IdleAfterComplete => "completed but session lingers (slot held)",
            Self::IdleRunningZombie => "alive session, no progress past stall budget",
            Self::Overloaded => "overloaded / rate-limited / starved",
            Self::GhostMerge => "merged/archived without an authorizing done event (integrity)",
            Self::CompletedUnharvested => "completed but never harvested",
            Self::CrashZombie => "worker crashed, status stuck running (downstream waiters blind)",
            Self::OutputStalled => {
                "alive session has produced no durable output past the output budget"
            }
        }
    }
}

/// The perimeter-correct remedy the Witness *recommends* (ADR-137 §4). In
/// Phase 1 this is **advisory only** — `cs health` is read-only and mutates
/// nothing. The Deacon (P3+) maps each variant onto an existing perimeter verb
/// behind the §5 no-interference guard.
///
/// This is deliberately a *separate* enum from [`PatrolAction`]: the existing
/// `PatrolAction` variants (`RestartWorker`, `ReassignMolecule`, …) are
/// worker/transport-centric and do not express `collapse`/`done`/`nudge`/
/// `backoff`. Forcing the health remedies into the wrong shape would lie about
/// what the deacon does. Each variant below maps 1:1 to a §4 catalog remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthRemedy {
    /// A1 — the transport re-submits (the 81b2 robust-submit, owned by
    /// `cs tackle`/`cosmon-transport`). The healer only *flags* if still
    /// stalled after re-submit; it never re-implements the Enter-kick.
    TransportResubmit,
    /// A3/A9 — `cs collapse --reason-kind process_death`; re-dispatch is the
    /// orchestrator's call with per-account backoff.
    CollapseProcessDeath,
    /// A4/A8 — orchestrator-only `cs done` (harvest + teardown). A sanctioned
    /// non-worker caller, never a worker self-`done`.
    HarvestDone,
    /// A5 — `cs patrol --nudge` (re-engage, references `briefing.md`),
    /// idempotent (no re-nudge within the cooldown).
    Nudge,
    /// A6 — exponential backoff per account; a runtime hold, never a collapse
    /// or a re-dispatch into the same wall.
    BackoffPerAccount,
    /// A7 — **flag only**, never auto-heal. An integrity alarm surfaced to the
    /// operator and cosmon-ward; the deacon recovers slots, it does not paper
    /// over a broken `done` invariant.
    FlagOnly,
}

/// The auditable control-plane signal that *proved* an anomaly (ADR-137 §10).
///
/// This exists so a `HealthReport` is self-describing: a reviewer can read why
/// each finding fired without re-deriving it. Every variant names a piece of
/// *control-plane* state — never a pane glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum ControlPlaneSignal {
    /// A1 — `status == Running`, session alive, `events.jsonl` did not grow
    /// since `tackled_at` for longer than the boot grace.
    BootStallNoSubmit {
        /// Seconds the slot has been occupied with no event growth.
        idle_secs: i64,
    },
    /// A3 — a typed auth probe / adapter exit code reported auth-dead.
    AuthProbeFailed,
    /// A4 — `status == Completed` and the worker's session is still alive.
    CompletedSessionAlive,
    /// A8 — `status == Completed` and `archived == false`.
    CompletedUnarchived,
    /// A5 — `status == Running`, session alive, `last_progress_at` older than
    /// the active step's stall budget.
    ProgressStalled {
        /// Seconds since the last recorded progress.
        idle_secs: i64,
        /// The stall budget that was exceeded.
        budget_secs: i64,
    },
    /// A6 — `Starved` status or a typed rate-limit signal.
    Overloaded,
    /// A7 — `merged_at`/`archived` set without an authorizing non-worker
    /// `Done` event (a state the worker could not have authored).
    UnauthorizedMerge,
    /// A9 — liveness lease expired (ADR-116), no session, `status == Running`.
    LeaseExpiredNoSession,
    /// An alive worker has not emitted a file/commit/step output within the
    /// configured advisory window.
    OutputStalled {
        /// Seconds since the latest durable output (or tackle when none).
        idle_secs: i64,
        /// The advisory output budget that was exceeded.
        budget_secs: i64,
    },
}

/// Tunable thresholds for the Witness scan (ADR-137 §4 defaults). In
/// production these come from `patrols.toml`; [`Default`] supplies the §4
/// defaults so the scan and its tests are self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthThresholds {
    /// A1 — grace after tackle before a no-event-growth slot is a boot-stall
    /// (default 90 s).
    pub boot_grace: Duration,
    /// A5 — fallback per-step stall budget when a view carries no explicit
    /// `step_timeout` (default 30 min).
    pub default_step_timeout: Duration,
    /// Advisory window after which an alive worker with no durable output is
    /// surfaced as [`AnomalyClass::OutputStalled`] (default 30 min).
    pub output_stall_timeout: Duration,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            boot_grace: Duration::seconds(90),
            default_step_timeout: Duration::minutes(30),
            output_stall_timeout: Duration::minutes(30),
        }
    }
}

/// A pre-digested, control-plane-only view of one molecule's health-relevant
/// state. The CLI shell builds these from `MoleculeData` + the liveness probe
/// + the presence registry + the whisper log; the Witness classifies them.
///
/// **There is deliberately no field for rendered pane text** (ADR-137 §2):
/// glyph-inference is structurally foreclosed. Fields the shell cannot prove
/// should default *conservatively* (e.g. `merge_authorized: true`,
/// `auth_probe_failed: false`) so the read-only report errs toward *missing* a
/// stall rather than the catastrophic false-positive of flagging a compliant
/// worker.
///
/// The flat boolean fields are deliberate: each is one *independent*,
/// pre-digested control-plane fact the shell folds in, not a state machine —
/// collapsing them into enums would obscure that orthogonality and force the
/// shell to encode combinations that cannot occur. Hence the
/// `struct_excessive_bools` allow.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct MoleculeHealthView {
    /// The molecule under inspection.
    pub molecule_id: MoleculeId,
    /// Authoritative `status` from `state.json`.
    pub status: MoleculeStatus,
    /// Transport liveness of the worker's session — a control-plane probe
    /// (`tmux has-session`), **never** pane content.
    pub session: Liveness,
    /// When the molecule was tackled (`tackled_at`), if known.
    pub tackled_at: Option<DateTime<Utc>>,
    /// When progress was last recorded (`last_progress_at`), if known.
    pub last_progress_at: Option<DateTime<Utc>>,
    /// When durable output (file, commit, or completed step) was last
    /// recorded. Heartbeats never advance this timestamp.
    pub last_output_at: Option<DateTime<Utc>>,
    /// Authoritative `updated_at` (fallback clock when finer signals absent).
    pub updated_at: DateTime<Utc>,
    /// The active step's stall budget; `None` ⇒ use
    /// [`HealthThresholds::default_step_timeout`].
    pub step_timeout: Option<Duration>,
    /// Did `events.jsonl` grow since `tackled_at`? Folded by the shell — the
    /// Witness never reads raw events. Conservative default: `true`.
    pub events_advanced_since_tackle: bool,
    /// A typed auth probe (adapter exit code / `ProcessDied` + probe) reported
    /// the session auth-dead. **Never** a `grep '401'`. Default: `false`.
    pub auth_probe_failed: bool,
    /// Overloaded by control-plane signal (`Starved` status OR a typed
    /// rate-limit event). **Never** a pane `API Error` glyph. Default: `false`.
    pub rate_limited: bool,
    /// `merged_at` or `archived` is set on this molecule.
    pub merged_or_archived: bool,
    /// The merge/done was authorized by a non-worker `Done` event (a state the
    /// worker could not author). `false` + merged ⇒ ghost-merge. Conservative
    /// default: `true` (do not raise an integrity alarm without proof).
    pub merge_authorized: bool,
    /// `archived` flag from `state.json`.
    pub archived: bool,
    /// Worker-liveness lease expired (ADR-116): probe stale / no fresh witness.
    pub lease_expired: bool,
    /// A human pilot is actively steering (a live presence row OR a directed
    /// whisper within the quiet-period, ADR-137 §5.1/§5.2). A guard input for
    /// the deacon (P3): `piloted ⇒ no autonomous action`. Surfaced in P1 for
    /// honesty; it does **not** suppress the *finding* (the read-only report
    /// shows piloted anomalies so the operator sees them).
    pub piloted: bool,
}

impl MoleculeHealthView {
    /// Construct the canonical *healthy* view — `Running`, session alive, fresh
    /// progress, nothing merged. The seam tests and the shell build on top of
    /// this so a healthy molecule needs no boilerplate and every anomaly test
    /// states only the field(s) that make it anomalous.
    #[must_use]
    pub fn healthy(molecule_id: MoleculeId, now: DateTime<Utc>) -> Self {
        Self {
            molecule_id,
            status: MoleculeStatus::Running,
            session: Liveness::Alive,
            tackled_at: Some(now),
            last_progress_at: Some(now),
            last_output_at: Some(now),
            updated_at: now,
            step_timeout: None,
            events_advanced_since_tackle: true,
            auth_probe_failed: false,
            rate_limited: false,
            merged_or_archived: false,
            merge_authorized: true,
            archived: false,
            lease_expired: false,
            piloted: false,
        }
    }
}

/// One anomalous molecule, its classified cause, and the recommended (P1:
/// advisory) remedy. Keyed off control-plane state — never pane glyphs
/// (ADR-137 §2, §10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthFinding {
    /// The molecule the finding is about.
    pub molecule_id: MoleculeId,
    /// Which anomaly class fired.
    pub class: AnomalyClass,
    /// The auditable control-plane signal that proved it.
    pub signal: ControlPlaneSignal,
    /// Was a human pilot steering this molecule? Guard input (§5); set ⇒ the
    /// deacon must not auto-act. The finding is still reported in P1.
    pub piloted: bool,
    /// The perimeter-correct remedy (advisory in P1).
    pub remedy: HealthRemedy,
}

/// The Witness output: the worker-aggregate [`PatrolReport`] plus the
/// per-molecule anomaly findings (ADR-137 §10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    /// When the scan ran.
    pub timestamp: DateTime<Utc>,
    /// The fleet-aggregate transport report.
    pub patrol: PatrolReport,
    /// Per-molecule anomaly findings.
    pub findings: Vec<HealthFinding>,
}

impl HealthReport {
    /// Whether the scan found zero anomalies.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.findings.is_empty()
    }

    /// How many findings of a given class fired (the escalation input, §6).
    #[must_use]
    pub fn count_of(&self, class: AnomalyClass) -> usize {
        self.findings.iter().filter(|f| f.class == class).count()
    }
}

/// Classify one molecule view into at most one finding, via a fixed priority
/// cascade. The order matters: a single molecule can satisfy several rows of
/// the §4 catalog at once (e.g. a `Completed` molecule with a live session is
/// both A4 and A8), so the Witness emits the **most specific / highest-stakes**
/// class and stops — keeping the report one-finding-per-molecule and the tests
/// deterministic.
///
/// Priority (high → low): A7 ghost-merge (integrity) ▸ A3 auth-dead ▸ A9
/// crash-zombie ▸ A6 overloaded ▸ A4 idle-after-complete ▸ A8 completed-
/// unharvested ▸ A1 unsent-paste ▸ A5 idle-running-zombie ▸ `OutputStalled`.
fn classify_one(
    view: &MoleculeHealthView,
    now: DateTime<Utc>,
    cfg: &HealthThresholds,
) -> Option<(AnomalyClass, ControlPlaneSignal, HealthRemedy)> {
    use AnomalyClass as A;

    // A7 — ghost-merge / silent-done. Integrity alarm, highest priority: a
    // merge/archive the worker could not have authorized. One occurrence is
    // enough (§6). Flag only.
    if view.merged_or_archived && !view.merge_authorized {
        return Some((
            A::GhostMerge,
            ControlPlaneSignal::UnauthorizedMerge,
            HealthRemedy::FlagOnly,
        ));
    }

    // A3 — auth-dead. A typed probe / adapter exit code, never `grep '401'`.
    if view.auth_probe_failed {
        return Some((
            A::AuthDead,
            ControlPlaneSignal::AuthProbeFailed,
            HealthRemedy::CollapseProcessDeath,
        ));
    }

    // A9 — crash-survived `Running` zombie: lease expired AND no session AND
    // still `Running`. Downstream `cs wait` is blind until a terminal state.
    if view.status == MoleculeStatus::Running
        && matches!(view.session, Liveness::Dead)
        && view.lease_expired
    {
        return Some((
            A::CrashZombie,
            ControlPlaneSignal::LeaseExpiredNoSession,
            HealthRemedy::CollapseProcessDeath,
        ));
    }

    // A6 — overloaded. `Starved` status or a typed rate-limit signal. Backoff,
    // never collapse.
    if view.status == MoleculeStatus::Starved || view.rate_limited {
        return Some((
            A::Overloaded,
            ControlPlaneSignal::Overloaded,
            HealthRemedy::BackoffPerAccount,
        ));
    }

    // A4 — idle-after-complete: completed, session still alive (slot held).
    if view.status == MoleculeStatus::Completed && matches!(view.session, Liveness::Alive) {
        return Some((
            A::IdleAfterComplete,
            ControlPlaneSignal::CompletedSessionAlive,
            HealthRemedy::HarvestDone,
        ));
    }

    // A8 — completed-unharvested: completed, never archived (no live session
    // needed). Decoupled from A4's session presence.
    if view.status == MoleculeStatus::Completed && !view.archived {
        return Some((
            A::CompletedUnharvested,
            ControlPlaneSignal::CompletedUnarchived,
            HealthRemedy::HarvestDone,
        ));
    }

    // A1 — unsent-paste / boot-stall: Running, alive, no event growth since
    // tackle past the boot grace. Folded from the event log, never a pane grep.
    if view.status == MoleculeStatus::Running
        && matches!(view.session, Liveness::Alive)
        && !view.events_advanced_since_tackle
    {
        if let Some(tackled) = view.tackled_at {
            let idle = now.signed_duration_since(tackled);
            if idle > cfg.boot_grace {
                return Some((
                    A::UnsentPaste,
                    ControlPlaneSignal::BootStallNoSubmit {
                        idle_secs: idle.num_seconds(),
                    },
                    HealthRemedy::TransportResubmit,
                ));
            }
        }
    }

    // A5 — idle-running zombie: Running, alive, *some* progress happened but
    // `last_progress_at` is older than the step's stall budget. Distinct from
    // A1, which is a boot-stall (no progress ever).
    if view.status == MoleculeStatus::Running && matches!(view.session, Liveness::Alive) {
        if let Some(last) = view.last_progress_at {
            let budget = view.step_timeout.unwrap_or(cfg.default_step_timeout);
            let idle = now.signed_duration_since(last);
            if idle > budget {
                return Some((
                    A::IdleRunningZombie,
                    ControlPlaneSignal::ProgressStalled {
                        idle_secs: idle.num_seconds(),
                        budget_secs: budget.num_seconds(),
                    },
                    HealthRemedy::Nudge,
                ));
            }
        }
    }

    // Advisory output witness: a live heartbeat proves only that the process
    // exists. It does not count as a file, commit, or step result. Use tackle
    // as the start when legacy/current work has not produced output yet.
    if view.status == MoleculeStatus::Running && matches!(view.session, Liveness::Alive) {
        if let Some(last_output) = view.last_output_at.or(view.tackled_at) {
            let idle = now.signed_duration_since(last_output);
            if idle > cfg.output_stall_timeout {
                return Some((
                    A::OutputStalled,
                    ControlPlaneSignal::OutputStalled {
                        idle_secs: idle.num_seconds(),
                        budget_secs: cfg.output_stall_timeout.num_seconds(),
                    },
                    HealthRemedy::FlagOnly,
                ));
            }
        }
    }

    None
}

/// The Witness — scan a fleet of molecule views into a classified
/// [`HealthReport`] (ADR-137 §10).
///
/// **Pure and I/O-free** (THESIS Part VII): control-plane state in, report
/// out. The CLI shell does the reading (state store, liveness probe, presence,
/// whisper) and folds it into `views`; this function only classifies, so it is
/// property-testable without a filesystem.
///
/// The returned [`HealthReport::patrol`] is a molecule-derived aggregate: the
/// worker-centric vectors stay empty (worker-level classification is the
/// existing `cs patrol` path), `orphaned_molecules` lists the crash-zombies
/// (A9) whose downstream waiters are blind, and `ensemble_size` is the number
/// of views scanned.
///
/// # Examples
///
/// ```
/// use chrono::Utc;
/// use cosmon_core::id::MoleculeId;
/// use cosmon_core::patrol::{scan, HealthThresholds, MoleculeHealthView};
///
/// let now = Utc::now();
/// let id = MoleculeId::new("cs-20260626-aaaa").unwrap();
/// let views = vec![MoleculeHealthView::healthy(id, now)];
/// let report = scan(&views, now, &HealthThresholds::default());
/// assert!(report.is_healthy());
/// ```
#[must_use]
pub fn scan(
    views: &[MoleculeHealthView],
    now: DateTime<Utc>,
    cfg: &HealthThresholds,
) -> HealthReport {
    let mut findings = Vec::new();
    let mut orphaned = Vec::new();

    for view in views {
        if let Some((class, signal, remedy)) = classify_one(view, now, cfg) {
            if class == AnomalyClass::CrashZombie {
                orphaned.push(view.molecule_id.clone());
            }
            findings.push(HealthFinding {
                molecule_id: view.molecule_id.clone(),
                class,
                signal,
                piloted: view.piloted,
                remedy,
            });
        }
    }

    let patrol = PatrolReport {
        timestamp: now,
        ensemble_size: views.len(),
        idle_count: 0,
        stalled_workers: Vec::new(),
        error_workers: Vec::new(),
        orphaned_molecules: orphaned,
        recommendations: vec![PatrolAction::NoAction],
    };

    HealthReport {
        timestamp: now,
        patrol,
        findings,
    }
}

// ===========================================================================
// The no-interference Guard (ADR-137 §5) — the brake, built before the engine
// ===========================================================================
//
// The Deacon must **never** touch a worker a human pilot is actively driving —
// mid-whisper, mid-legitimate-long-operation, mid-debug. Healing a piloted
// worker is worse than the stall it cures: it destroys live human work and
// erodes trust in the whole primitive (ADR-137 §5, §12 "costs accepted").
//
// This module is the **brake** the be1e audit demanded be built *before* the
// engine: the guard predicate exists, is unit-tested in isolation, and gates
// every future autonomous mutation — but **no remediation is wired here**
// (P2 scope, ADR-137 §11). The Deacon's apply-loop (P3+) calls [`heal_gate`]
// per molecule, per finding, and acts only on a [`HealGate::Heal`].
//
// **The conjunction (§5).** A molecule is healable only if *all* clauses pass:
//   1. §5.1 — no live pilot/presence session on it (presence registry).
//   2. §5.2 — the whisper quiet-period has elapsed since the last directed
//      whisper (ADR-038 whisper log is the clearest "hands on the stick"
//      signal; the healer never overrides it).
//   3. §5.3 — no per-molecule do-not-heal marker (tag `health:hold` or a
//      `<molecule_dir>/.no-heal` sentinel).
//   4. §5.4 — the global kill-switch `~/.cosmon/health.off` is absent
//      (re-checked before *every* mutation; the pass may straddle an operator
//      gesture).
//   5. §5.5 — backoff memory: the same remedy is not re-applied within its
//      per-class cooldown, and three consecutive failed remediations on one
//      molecule stop healing it and flag for a human (anti-thrash + the
//      cross-galaxy three-strikes convention).
//
// **Stratification (§2), same as the Witness.** Every input the guard reads is
// *control-plane* state — presence rows, whisper-log timestamps, tags,
// sentinel files, the backoff ledger — folded by the CLI shell into the flat
// fields of [`HealGuardView`]. There is deliberately **no pane-text input**:
// the guard never tries to *guess* "does this look like a human typing?" from
// scrollback glyphs. The deacon watches the state machine, not the screen.

/// Why the no-interference guard blocked an autonomous remediation. Each
/// variant names the §5 clause that failed; carried inside
/// [`HealGate::Blocked`] so a `--dry-run`/`--json` report is self-describing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HealBlockReason {
    /// §5.4 — the global kill-switch `~/.cosmon/health.off` is present; the
    /// whole heal pass is a no-op. Re-checked before every mutation.
    GlobalKillSwitch,
    /// §5.3 — a per-molecule do-not-heal marker is set (tag `health:hold` or a
    /// `<molecule_dir>/.no-heal` sentinel). The operator is babysitting it.
    DoNotHealMarker,
    /// §5.1 — a live pilot/presence session is registered on the molecule.
    LivePilot,
    /// §5.2 — a directed whisper landed within the quiet-period; a human is
    /// steering. The healer waits out the quiet period before acting.
    WhisperQuietPeriod {
        /// Seconds since the last directed whisper.
        secs_since_whisper: i64,
        /// The quiet period that must elapse first.
        quiet_secs: i64,
    },
    /// §5.5 — three consecutive failed remediations on this molecule; stop
    /// healing it and flag for a human (the three-strikes convention).
    ThreeStrikes {
        /// The consecutive-failure count that tripped the limit.
        failures: u32,
    },
    /// §5.5 — the same remedy was applied within its per-class cooldown;
    /// re-applying now would thrash.
    BackoffCooldown {
        /// The remedy that is still cooling down.
        remedy: HealthRemedy,
        /// Seconds since the remedy was last applied.
        secs_since_last: i64,
        /// The per-class cooldown that has not yet elapsed.
        cooldown_secs: i64,
    },
}

/// The guard's verdict for one molecule + one candidate remedy (ADR-137 §5).
///
/// `Heal` means *all* §5 clauses passed and the deacon may apply the remedy;
/// `Blocked` carries the first clause that failed. The guard is evaluated
/// **per molecule, per finding** — a healthy decision on molecule X never
/// licenses an action on molecule Y.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum HealGate {
    /// All §5 clauses passed — the deacon may apply the remedy.
    Heal,
    /// Blocked — one clause failed; the reason names which (§5.x).
    Blocked(HealBlockReason),
}

impl HealGate {
    /// Whether the deacon may act (all §5 clauses passed).
    #[must_use]
    pub fn is_healable(&self) -> bool {
        matches!(self, Self::Heal)
    }

    /// The blocking reason, if any (`None` when healable).
    #[must_use]
    pub fn blocked_reason(&self) -> Option<&HealBlockReason> {
        match self {
            Self::Blocked(r) => Some(r),
            Self::Heal => None,
        }
    }
}

/// Tunable thresholds for the no-interference guard (ADR-137 §5 defaults). In
/// production these come from `patrols.toml`; [`Default`] supplies the §5
/// defaults so the guard and its tests are self-contained.
///
/// The cooldown is **per remedy class** (§5.5): a nudge re-arms in 60 s, a
/// collapse-redispatch waits out a per-account backoff. [`Self::cooldown_for`]
/// maps each [`HealthRemedy`] to its cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardConfig {
    /// §5.2 — the whisper quiet-period: a directed whisper within this window
    /// means a human is steering (default 10 min).
    pub pilot_quiet: Duration,
    /// §5.5 — consecutive failed remediations before the guard gives up on a
    /// molecule and flags for a human (default 3, the three-strikes convention).
    pub max_consecutive_failures: u32,
    /// §5.5 — A5 nudge cooldown (default 60 s, the §4 A5 figure).
    pub nudge_cooldown: Duration,
    /// §5.5 — A4/A8 harvest (`cs done`) cooldown (default 60 s).
    pub harvest_cooldown: Duration,
    /// §5.5 — A1 transport-resubmit cooldown (default 60 s).
    pub resubmit_cooldown: Duration,
    /// §5.5 — A3/A9 collapse-redispatch base cooldown (default 5 min; the
    /// per-account exponential backoff is layered by the deacon on top).
    pub collapse_cooldown: Duration,
    /// §5.5 — A6 overloaded backoff base cooldown (default 5 min).
    pub backoff_cooldown: Duration,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            pilot_quiet: Duration::minutes(10),
            max_consecutive_failures: 3,
            nudge_cooldown: Duration::seconds(60),
            harvest_cooldown: Duration::seconds(60),
            resubmit_cooldown: Duration::seconds(60),
            collapse_cooldown: Duration::minutes(5),
            backoff_cooldown: Duration::minutes(5),
        }
    }
}

impl GuardConfig {
    /// The per-class cooldown for a remedy (§5.5). [`HealthRemedy::FlagOnly`]
    /// never mutates, so its cooldown is zero (the backoff clause is a no-op
    /// for it; the guard is consulted only for mutating remedies in practice).
    #[must_use]
    pub fn cooldown_for(&self, remedy: HealthRemedy) -> Duration {
        match remedy {
            HealthRemedy::Nudge => self.nudge_cooldown,
            HealthRemedy::HarvestDone => self.harvest_cooldown,
            HealthRemedy::TransportResubmit => self.resubmit_cooldown,
            HealthRemedy::CollapseProcessDeath => self.collapse_cooldown,
            HealthRemedy::BackoffPerAccount => self.backoff_cooldown,
            HealthRemedy::FlagOnly => Duration::zero(),
        }
    }
}

/// A pre-digested, control-plane-only view of one molecule's *piloting and
/// backoff* state — the inputs to the §5 guard. The CLI shell builds these
/// from the presence registry, the whisper log, the molecule tags / `.no-heal`
/// sentinel, the global kill-switch file, and the backoff ledger; the guard
/// [`heal_gate`] decides.
///
/// **There is deliberately no field for rendered pane text** (ADR-137 §2): the
/// guard never infers "is a human typing?" from scrollback. Every field is an
/// independent, pre-digested control-plane fact — hence the flat-booleans
/// shape mirroring [`MoleculeHealthView`].
///
/// Fields the shell cannot prove default *conservatively toward letting the
/// healer act* only where that is safe; the *piloting* signals default to
/// "no pilot" because their absence is the normal case (a worker with neither
/// a presence row nor a recent whisper is, by definition, unpiloted — the
/// residual false-negative is the §12 accepted risk, mitigated by the
/// per-molecule `health:hold` sentinel).
#[derive(Debug, Clone)]
pub struct HealGuardView {
    /// The molecule the guard decision is about.
    pub molecule_id: MoleculeId,
    /// §5.1 — a live pilot/presence session is registered against this
    /// molecule (presence registry `.cosmon/state/presence/`). Default in the
    /// all-clear constructor: `false`.
    pub pilot_present: bool,
    /// §5.2 — when the molecule last received a directed whisper (ADR-038),
    /// if ever. `None` ⇒ no whisper on record. Folded from the whisper log,
    /// never a pane read.
    pub last_whisper_at: Option<DateTime<Utc>>,
    /// §5.3 — a per-molecule do-not-heal marker is set: tag `health:hold` OR a
    /// `<molecule_dir>/.no-heal` sentinel. Folded by the shell into one bool.
    pub do_not_heal: bool,
    /// §5.4 — the global kill-switch `~/.cosmon/health.off` is present.
    pub global_kill_switch: bool,
    /// §5.5 — when the deacon last applied a remedy to this molecule, if ever.
    /// Paired with [`Self::last_remedy`] — cooldown is per remedy class.
    pub last_heal_at: Option<DateTime<Utc>>,
    /// §5.5 — which remedy was last applied (cooldown is per-class). `None` ⇒
    /// no prior remediation on record.
    pub last_remedy: Option<HealthRemedy>,
    /// §5.5 — consecutive failed remediations on this molecule. At or above
    /// [`GuardConfig::max_consecutive_failures`] ⇒ stop healing, flag a human.
    pub consecutive_failures: u32,
}

impl HealGuardView {
    /// Construct the canonical *all-clear* view — no pilot, no whisper, no
    /// marker, no kill-switch, no prior remediation. Every guard test builds on
    /// top of this so a healable molecule needs no boilerplate and each block
    /// test states only the field(s) that trip its clause (the same seam
    /// discipline as [`MoleculeHealthView::healthy`]).
    #[must_use]
    pub fn healable(molecule_id: MoleculeId) -> Self {
        Self {
            molecule_id,
            pilot_present: false,
            last_whisper_at: None,
            do_not_heal: false,
            global_kill_switch: false,
            last_heal_at: None,
            last_remedy: None,
            consecutive_failures: 0,
        }
    }
}

/// The no-interference guard (ADR-137 §5) — **the brake, pure and I/O-free**.
///
/// Decides whether the deacon may apply `candidate` to the molecule described
/// by `view`. Returns [`HealGate::Heal`] only when *all* §5 clauses pass;
/// otherwise [`HealGate::Blocked`] naming the first failing clause. No I/O, no
/// mutation, no remediation wired — the CLI shell folds control-plane state
/// into `view`, calls this, and (in P3+) acts only on `Heal`.
///
/// **Clause evaluation order** is fixed and total, so the reported reason is
/// deterministic when several clauses fail at once (high → low precedence):
/// §5.4 global kill-switch ▸ §5.3 per-molecule marker ▸ §5.1 live pilot ▸
/// §5.2 whisper quiet-period ▸ §5.5 three-strikes ▸ §5.5 backoff cooldown.
/// The kill-switches dominate (an operator gesture overrides everything); the
/// piloting clauses come next (protecting live human work is the point); the
/// anti-thrash clauses are last (they only matter once the molecule is
/// otherwise healable).
///
/// # Examples
///
/// ```
/// use chrono::Utc;
/// use cosmon_core::id::MoleculeId;
/// use cosmon_core::patrol::{heal_gate, GuardConfig, HealGuardView, HealthRemedy};
///
/// let now = Utc::now();
/// let id = MoleculeId::new("cs-20260626-bbbb").unwrap();
/// // An all-clear molecule may be nudged.
/// let view = HealGuardView::healable(id);
/// let gate = heal_gate(&view, HealthRemedy::Nudge, now, &GuardConfig::default());
/// assert!(gate.is_healable());
/// ```
#[must_use]
pub fn heal_gate(
    view: &HealGuardView,
    candidate: HealthRemedy,
    now: DateTime<Utc>,
    cfg: &GuardConfig,
) -> HealGate {
    // §5.4 — global kill-switch dominates; re-checked before every mutation so
    // a pass that straddles an operator gesture stops at the next molecule.
    if view.global_kill_switch {
        return HealGate::Blocked(HealBlockReason::GlobalKillSwitch);
    }

    // §5.3 — per-molecule do-not-heal marker: an unconditional operator exempt.
    if view.do_not_heal {
        return HealGate::Blocked(HealBlockReason::DoNotHealMarker);
    }

    // §5.1 — a live pilot/presence session on the molecule: piloted ⇒ skip.
    if view.pilot_present {
        return HealGate::Blocked(HealBlockReason::LivePilot);
    }

    // §5.2 — whisper quiet-period: a directed whisper within `pilot_quiet`
    // means a human is steering. The period has *elapsed* only once
    // `now - last_whisper >= pilot_quiet`.
    if let Some(whisper_at) = view.last_whisper_at {
        let since = now.signed_duration_since(whisper_at);
        if since < cfg.pilot_quiet {
            return HealGate::Blocked(HealBlockReason::WhisperQuietPeriod {
                secs_since_whisper: since.num_seconds(),
                quiet_secs: cfg.pilot_quiet.num_seconds(),
            });
        }
    }

    // §5.5 — three-strikes: stop healing a molecule that keeps failing, flag a
    // human. Checked before the cooldown so the terminal reason is reported.
    if view.consecutive_failures >= cfg.max_consecutive_failures {
        return HealGate::Blocked(HealBlockReason::ThreeStrikes {
            failures: view.consecutive_failures,
        });
    }

    // §5.5 — backoff cooldown: the *same* remedy is not re-applied within its
    // per-class cooldown (anti-thrash). A different remedy is not blocked here.
    if let (Some(last_at), Some(last_remedy)) = (view.last_heal_at, view.last_remedy) {
        if last_remedy == candidate {
            let cooldown = cfg.cooldown_for(candidate);
            let since = now.signed_duration_since(last_at);
            if since < cooldown {
                return HealGate::Blocked(HealBlockReason::BackoffCooldown {
                    remedy: candidate,
                    secs_since_last: since.num_seconds(),
                    cooldown_secs: cooldown.num_seconds(),
                });
            }
        }
    }

    HealGate::Heal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{MoleculeId, WorkerId};

    #[test]
    fn test_healthy_report() {
        let report = PatrolReport {
            timestamp: Utc::now(),
            ensemble_size: 3,
            idle_count: 0,
            stalled_workers: vec![],
            error_workers: vec![],
            orphaned_molecules: vec![],
            recommendations: vec![PatrolAction::NoAction],
        };
        assert!(report.is_healthy());
        assert_eq!(report.issue_count(), 0);
    }

    #[test]
    fn test_unhealthy_report() {
        let report = PatrolReport {
            timestamp: Utc::now(),
            ensemble_size: 5,
            idle_count: 1,
            stalled_workers: vec![WorkerId::new("stale-1").unwrap()],
            error_workers: vec![],
            orphaned_molecules: vec![MoleculeId::new("cs-20260401-orph").unwrap()],
            recommendations: vec![],
        };
        assert!(!report.is_healthy());
        assert_eq!(report.issue_count(), 2);
    }

    #[test]
    fn test_patrol_action_serde_roundtrip() {
        let actions = vec![
            PatrolAction::RestartWorker {
                worker_id: WorkerId::new("quartz").unwrap(),
                reason: "stale heartbeat".to_owned(),
            },
            PatrolAction::ReassignMolecule {
                molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
                dead_worker: WorkerId::new("ghost").unwrap(),
            },
            PatrolAction::AlertHuman {
                message: "too many errors".to_owned(),
            },
            PatrolAction::NoAction,
        ];
        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let back: PatrolAction = serde_json::from_str(&json).unwrap();
            assert_eq!(back, action);
        }
    }

    #[test]
    fn test_patrol_report_serde_roundtrip() {
        let report = PatrolReport {
            timestamp: Utc::now(),
            ensemble_size: 4,
            idle_count: 1,
            stalled_workers: vec![WorkerId::new("stale-w").unwrap()],
            error_workers: vec![WorkerId::new("err-w").unwrap()],
            orphaned_molecules: vec![MoleculeId::new("cs-20260401-orpn").unwrap()],
            recommendations: vec![PatrolAction::RestartWorker {
                worker_id: WorkerId::new("stale-w").unwrap(),
                reason: "unresponsive".to_owned(),
            }],
        };
        let json = serde_json::to_string_pretty(&report).unwrap();
        let back: PatrolReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ensemble_size, report.ensemble_size);
        assert_eq!(back.stalled_workers.len(), 1);
        assert_eq!(back.orphaned_molecules.len(), 1);
    }

    // -----------------------------------------------------------------------
    // The Witness — anomaly classification tests (ADR-137 §4)
    // -----------------------------------------------------------------------

    use chrono::Duration;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    /// Scan a single view with default thresholds and return its lone finding
    /// (or `None`). Every anomaly test funnels through here so each test states
    /// only the field(s) that make the molecule anomalous.
    fn scan_one(view: MoleculeHealthView, now: DateTime<Utc>) -> Option<HealthFinding> {
        let report = scan(&[view], now, &HealthThresholds::default());
        report.findings.into_iter().next()
    }

    #[test]
    fn test_healthy_molecule_yields_no_finding() {
        let now = Utc::now();
        let view = MoleculeHealthView::healthy(mid("cs-20260626-heal"), now);
        assert!(scan_one(view, now).is_none());
    }

    #[test]
    fn test_a1_unsent_paste_boot_stall() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a1aa"), now);
        // Tackled 5 min ago, session alive, but no event growth since tackle.
        view.tackled_at = Some(now - Duration::minutes(5));
        view.events_advanced_since_tackle = false;
        let f = scan_one(view, now).expect("A1 should fire");
        assert_eq!(f.class, AnomalyClass::UnsentPaste);
        assert_eq!(f.remedy, HealthRemedy::TransportResubmit);
        assert!(matches!(
            f.signal,
            ControlPlaneSignal::BootStallNoSubmit { .. }
        ));
    }

    #[test]
    fn test_a1_within_boot_grace_does_not_fire() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a1bb"), now);
        // Only 30 s since tackle — inside the 90 s boot grace.
        view.tackled_at = Some(now - Duration::seconds(30));
        view.events_advanced_since_tackle = false;
        assert!(scan_one(view, now).is_none());
    }

    #[test]
    fn test_a3_auth_dead_from_typed_probe() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a3aa"), now);
        view.auth_probe_failed = true;
        let f = scan_one(view, now).expect("A3 should fire");
        assert_eq!(f.class, AnomalyClass::AuthDead);
        assert_eq!(f.signal, ControlPlaneSignal::AuthProbeFailed);
        assert_eq!(f.remedy, HealthRemedy::CollapseProcessDeath);
    }

    #[test]
    fn test_a4_idle_after_complete() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a4aa"), now);
        view.status = MoleculeStatus::Completed;
        view.session = Liveness::Alive; // session lingers
        view.archived = true; // already harvested-but-session-alive ⇒ still A4
        let f = scan_one(view, now).expect("A4 should fire");
        assert_eq!(f.class, AnomalyClass::IdleAfterComplete);
        assert_eq!(f.signal, ControlPlaneSignal::CompletedSessionAlive);
        assert_eq!(f.remedy, HealthRemedy::HarvestDone);
    }

    #[test]
    fn test_a5_idle_running_zombie() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a5aa"), now);
        // Progress happened once, but 45 min ago — past the 30 min default budget.
        view.last_progress_at = Some(now - Duration::minutes(45));
        let f = scan_one(view, now).expect("A5 should fire");
        assert_eq!(f.class, AnomalyClass::IdleRunningZombie);
        assert_eq!(f.remedy, HealthRemedy::Nudge);
        assert!(matches!(
            f.signal,
            ControlPlaneSignal::ProgressStalled { .. }
        ));
    }

    #[test]
    fn test_a5_respects_per_step_timeout() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a5bb"), now);
        // 45 min idle, but this step's budget is 60 min ⇒ not yet stalled.
        view.last_progress_at = Some(now - Duration::minutes(45));
        view.step_timeout = Some(Duration::minutes(60));
        assert!(scan_one(view, now).is_none());
    }

    #[test]
    fn test_alive_thinking_worker_without_output_is_advisory_output_stalled() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260713-out1"), now);
        // The worker is alive and has just heartbeated/progressed, but its
        // durable output is 43 minutes old. A heartbeat must not hide this.
        view.last_progress_at = Some(now);
        view.last_output_at = Some(now - Duration::minutes(43));

        let f = scan_one(view, now).expect("missing output must be visible");
        assert_eq!(f.class, AnomalyClass::OutputStalled);
        assert_eq!(f.remedy, HealthRemedy::FlagOnly);
        assert!(matches!(f.signal, ControlPlaneSignal::OutputStalled { .. }));
    }

    #[test]
    fn test_a6_overloaded_from_starved_status() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a6aa"), now);
        view.status = MoleculeStatus::Starved;
        let f = scan_one(view, now).expect("A6 should fire");
        assert_eq!(f.class, AnomalyClass::Overloaded);
        assert_eq!(f.signal, ControlPlaneSignal::Overloaded);
        assert_eq!(f.remedy, HealthRemedy::BackoffPerAccount);
    }

    #[test]
    fn test_a6_overloaded_from_rate_limited_flag() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a6bb"), now);
        view.rate_limited = true;
        let f = scan_one(view, now).expect("A6 should fire");
        assert_eq!(f.class, AnomalyClass::Overloaded);
    }

    #[test]
    fn test_a7_ghost_merge_flag_only() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a7aa"), now);
        view.merged_or_archived = true;
        view.merge_authorized = false; // no authorizing Done event
        let f = scan_one(view, now).expect("A7 should fire");
        assert_eq!(f.class, AnomalyClass::GhostMerge);
        assert_eq!(f.signal, ControlPlaneSignal::UnauthorizedMerge);
        assert_eq!(f.remedy, HealthRemedy::FlagOnly);
    }

    #[test]
    fn test_a7_authorized_merge_is_not_a_ghost() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a7bb"), now);
        view.status = MoleculeStatus::Completed;
        view.merged_or_archived = true;
        view.merge_authorized = true; // an authorizing Done event exists
        view.archived = true;
        view.session = Liveness::Dead;
        // A legitimately merged + harvested molecule: no anomaly.
        assert!(scan_one(view, now).is_none());
    }

    #[test]
    fn test_a8_completed_unharvested() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a8aa"), now);
        view.status = MoleculeStatus::Completed;
        view.session = Liveness::Dead; // no live session ⇒ not A4
        view.archived = false; // never harvested
        let f = scan_one(view, now).expect("A8 should fire");
        assert_eq!(f.class, AnomalyClass::CompletedUnharvested);
        assert_eq!(f.signal, ControlPlaneSignal::CompletedUnarchived);
        assert_eq!(f.remedy, HealthRemedy::HarvestDone);
    }

    /// A8 must be **clearable**: once a harvest archives the molecule
    /// (`archived == true`), the very next scan must NOT re-flag it.
    ///
    /// This is the invariant behind task-20260626-eb65: a `no_branch`
    /// molecule (delib / drainage worker / empty-branch task) stays
    /// `Completed` after `cs done` — there is no merge to flip `merged_at` —
    /// so the *only* signal that the harvest happened is `archived`. The fix
    /// makes `cs done` set `archived = true` for that path; this test pins the
    /// classifier's half of the contract: `Completed AND archived` is healthy,
    /// not a permanent A8 phantom.
    #[test]
    fn test_a8_cleared_once_archived() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a8cc"), now);
        view.status = MoleculeStatus::Completed;
        view.session = Liveness::Dead; // no live session ⇒ not A4
        view.archived = true; // harvested — A8 must clear
        assert!(
            scan_one(view, now).is_none(),
            "A8 must clear once the molecule is archived (no_branch harvest)"
        );
    }

    #[test]
    fn test_a9_crash_zombie() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a9aa"), now);
        view.status = MoleculeStatus::Running;
        view.session = Liveness::Dead;
        view.lease_expired = true;
        let f = scan_one(view, now).expect("A9 should fire");
        assert_eq!(f.class, AnomalyClass::CrashZombie);
        assert_eq!(f.signal, ControlPlaneSignal::LeaseExpiredNoSession);
        assert_eq!(f.remedy, HealthRemedy::CollapseProcessDeath);
    }

    #[test]
    fn test_a9_listed_as_orphaned_in_patrol_aggregate() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-a9bb"), now);
        view.session = Liveness::Dead;
        view.lease_expired = true;
        let report = scan(&[view], now, &HealthThresholds::default());
        assert_eq!(report.patrol.orphaned_molecules.len(), 1);
        assert_eq!(report.patrol.ensemble_size, 1);
    }

    /// **The be1e use/mention regression guard (ADR-137 §2).**
    ///
    /// The drainage prototype's SEV-1 bug was a guard that grepped panes for
    /// `cs done` / `401` — and so killed the compliant worker whose *brief*
    /// merely *displays* those strings. The structural cure is that the Witness
    /// has no pane-text input at all: a molecule whose prompt/brief mentions
    /// `cs done` and `401` is classified purely on its control-plane state. A
    /// healthy worker stays healthy no matter what its scrollback says.
    #[test]
    fn test_be1e_use_mention_guard_brief_mentions_cs_done_and_401() {
        let now = Utc::now();
        // This molecule's brief (not modelled here — by design there is no
        // field for it) loudly forbids self-`cs done` and discusses `401`
        // auth errors. The view is otherwise perfectly healthy: Running,
        // session alive, fresh progress, nothing merged, no typed auth probe.
        let view = MoleculeHealthView::healthy(mid("cs-20260626-be1e"), now);
        // The Witness sees no anomaly: it never read the pane. The brief's
        // glyphs are powerless to trip it — the only inputs are state machine
        // facts. (Were the witness a glyph-grepper, the mention of `cs done`
        // and `401` would have produced a false A3/self-done finding here.)
        assert!(
            scan_one(view, now).is_none(),
            "a healthy worker whose brief mentions 'cs done'/'401' must NOT be flagged"
        );

        // And the structural proof: there is no way to *give* the Witness pane
        // text — `MoleculeHealthView` carries only typed control-plane state.
        // Auth-dead requires a typed probe, not a substring:
        let mut auth = MoleculeHealthView::healthy(mid("cs-20260626-be1f"), now);
        auth.auth_probe_failed = true; // the ONLY way to assert A3
        assert_eq!(
            scan_one(auth, now).map(|f| f.class),
            Some(AnomalyClass::AuthDead)
        );
    }

    #[test]
    fn test_priority_ghost_merge_beats_completed() {
        let now = Utc::now();
        // A molecule that is BOTH Completed-unharvested AND an unauthorized
        // merge: the integrity alarm (A7) must win.
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-prio"), now);
        view.status = MoleculeStatus::Completed;
        view.session = Liveness::Dead;
        view.archived = false;
        view.merged_or_archived = true;
        view.merge_authorized = false;
        let f = scan_one(view, now).expect("a finding should fire");
        assert_eq!(f.class, AnomalyClass::GhostMerge);
    }

    #[test]
    fn test_piloted_flag_is_surfaced_not_suppressed() {
        let now = Utc::now();
        let mut view = MoleculeHealthView::healthy(mid("cs-20260626-plt0"), now);
        view.last_progress_at = Some(now - Duration::minutes(45)); // A5
        view.piloted = true; // a human is steering
        let f = scan_one(view, now).expect("finding still reported in P1");
        assert_eq!(f.class, AnomalyClass::IdleRunningZombie);
        assert!(
            f.piloted,
            "piloted is surfaced for the P3 guard, not hidden"
        );
    }

    #[test]
    fn test_anomaly_class_codes_are_stable() {
        assert_eq!(AnomalyClass::UnsentPaste.code(), "A1");
        assert_eq!(AnomalyClass::AuthDead.code(), "A3");
        assert_eq!(AnomalyClass::IdleAfterComplete.code(), "A4");
        assert_eq!(AnomalyClass::IdleRunningZombie.code(), "A5");
        assert_eq!(AnomalyClass::Overloaded.code(), "A6");
        assert_eq!(AnomalyClass::GhostMerge.code(), "A7");
        assert_eq!(AnomalyClass::CompletedUnharvested.code(), "A8");
        assert_eq!(AnomalyClass::CrashZombie.code(), "A9");
    }

    #[test]
    fn test_health_finding_serde_roundtrip() {
        let f = HealthFinding {
            molecule_id: mid("cs-20260626-serd"),
            class: AnomalyClass::CrashZombie,
            signal: ControlPlaneSignal::LeaseExpiredNoSession,
            piloted: false,
            remedy: HealthRemedy::CollapseProcessDeath,
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: HealthFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn test_health_report_count_of() {
        let now = Utc::now();
        let mut a = MoleculeHealthView::healthy(mid("cs-20260626-cnt1"), now);
        a.rate_limited = true;
        let mut b = MoleculeHealthView::healthy(mid("cs-20260626-cnt2"), now);
        b.status = MoleculeStatus::Starved;
        let healthy = MoleculeHealthView::healthy(mid("cs-20260626-cnt3"), now);
        let report = scan(&[a, b, healthy], now, &HealthThresholds::default());
        assert_eq!(report.count_of(AnomalyClass::Overloaded), 2);
        assert_eq!(report.findings.len(), 2);
        assert!(!report.is_healthy());
    }

    // -----------------------------------------------------------------------
    // The no-interference Guard — §5 conjunction tests (ADR-137 §5)
    //
    // Built and tested IN ISOLATION before any remediation is wired (the be1e
    // lesson: build the brake before the engine). Each clause has a "blocks"
    // test and (where a boundary exists) an "allows past the boundary" test;
    // the conjunction is exercised by the all-pass and the precedence tests.
    // -----------------------------------------------------------------------

    /// Gate one healable-by-default view for a `Nudge`, stating only the
    /// blocking field — mirrors `scan_one`'s seam discipline for the Witness.
    fn gate(view: &HealGuardView, now: DateTime<Utc>) -> HealGate {
        heal_gate(view, HealthRemedy::Nudge, now, &GuardConfig::default())
    }

    #[test]
    fn test_all_clauses_pass_yields_heal() {
        let now = Utc::now();
        let view = HealGuardView::healable(mid("cs-20260626-g000"));
        let g = gate(&view, now);
        assert!(g.is_healable());
        assert_eq!(g, HealGate::Heal);
        assert!(g.blocked_reason().is_none());
    }

    // --- §5.1 live pilot --------------------------------------------------

    #[test]
    fn test_clause_5_1_live_pilot_blocks() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g510"));
        view.pilot_present = true;
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::LivePilot)
        );
    }

    // --- §5.2 whisper quiet-period ---------------------------------------

    #[test]
    fn test_clause_5_2_recent_whisper_blocks() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g520"));
        // A directed whisper landed 2 min ago — inside the 10 min quiet period.
        view.last_whisper_at = Some(now - Duration::minutes(2));
        match gate(&view, now) {
            HealGate::Blocked(HealBlockReason::WhisperQuietPeriod {
                secs_since_whisper,
                quiet_secs,
            }) => {
                assert_eq!(secs_since_whisper, 120);
                assert_eq!(quiet_secs, 600);
            }
            other => panic!("expected WhisperQuietPeriod, got {other:?}"),
        }
    }

    #[test]
    fn test_clause_5_2_whisper_past_quiet_period_allows() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g521"));
        // 11 min ago — the quiet period has elapsed; the worker is abandoned.
        view.last_whisper_at = Some(now - Duration::minutes(11));
        assert!(gate(&view, now).is_healable());
    }

    #[test]
    fn test_clause_5_2_whisper_exactly_at_boundary_allows() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g522"));
        // Exactly 10 min: the period has *elapsed* (>=), so the gate opens.
        view.last_whisper_at = Some(now - Duration::minutes(10));
        assert!(gate(&view, now).is_healable());
    }

    // --- §5.3 per-molecule do-not-heal marker ----------------------------

    #[test]
    fn test_clause_5_3_do_not_heal_marker_blocks() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g530"));
        view.do_not_heal = true; // tag health:hold OR .no-heal sentinel
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::DoNotHealMarker)
        );
    }

    // --- §5.4 global kill-switch -----------------------------------------

    #[test]
    fn test_clause_5_4_global_kill_switch_blocks() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g540"));
        view.global_kill_switch = true; // ~/.cosmon/health.off present
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::GlobalKillSwitch)
        );
    }

    // --- §5.5 backoff cooldown (anti-thrash) -----------------------------

    #[test]
    fn test_clause_5_5_same_remedy_within_cooldown_blocks() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g550"));
        // Nudged 30 s ago — inside the 60 s nudge cooldown.
        view.last_heal_at = Some(now - Duration::seconds(30));
        view.last_remedy = Some(HealthRemedy::Nudge);
        match heal_gate(&view, HealthRemedy::Nudge, now, &GuardConfig::default()) {
            HealGate::Blocked(HealBlockReason::BackoffCooldown {
                remedy,
                secs_since_last,
                cooldown_secs,
            }) => {
                assert_eq!(remedy, HealthRemedy::Nudge);
                assert_eq!(secs_since_last, 30);
                assert_eq!(cooldown_secs, 60);
            }
            other => panic!("expected BackoffCooldown, got {other:?}"),
        }
    }

    #[test]
    fn test_clause_5_5_same_remedy_past_cooldown_allows() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g551"));
        // Nudged 90 s ago — past the 60 s nudge cooldown; re-arm permitted.
        view.last_heal_at = Some(now - Duration::seconds(90));
        view.last_remedy = Some(HealthRemedy::Nudge);
        assert!(heal_gate(&view, HealthRemedy::Nudge, now, &GuardConfig::default()).is_healable());
    }

    #[test]
    fn test_clause_5_5_different_remedy_is_not_cooled_down() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g552"));
        // Just nudged 5 s ago, but the candidate now is a *different* remedy
        // (harvest) — the per-class cooldown does not apply across classes.
        view.last_heal_at = Some(now - Duration::seconds(5));
        view.last_remedy = Some(HealthRemedy::Nudge);
        assert!(heal_gate(
            &view,
            HealthRemedy::HarvestDone,
            now,
            &GuardConfig::default()
        )
        .is_healable());
    }

    #[test]
    fn test_clause_5_5_per_class_cooldown_differs_by_remedy() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g553"));
        // 90 s since a collapse: past the 60 s nudge cooldown, but inside the
        // 5 min collapse cooldown. Proves the cooldown is keyed to the remedy.
        view.last_heal_at = Some(now - Duration::seconds(90));
        view.last_remedy = Some(HealthRemedy::CollapseProcessDeath);
        let g = heal_gate(
            &view,
            HealthRemedy::CollapseProcessDeath,
            now,
            &GuardConfig::default(),
        );
        assert!(matches!(
            g,
            HealGate::Blocked(HealBlockReason::BackoffCooldown {
                remedy: HealthRemedy::CollapseProcessDeath,
                ..
            })
        ));
    }

    // --- §5.5 three-strikes ----------------------------------------------

    #[test]
    fn test_clause_5_5_three_strikes_blocks_and_flags() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g554"));
        view.consecutive_failures = 3; // == default threshold
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::ThreeStrikes { failures: 3 })
        );
    }

    #[test]
    fn test_clause_5_5_below_three_strikes_allows() {
        let now = Utc::now();
        let mut view = HealGuardView::healable(mid("cs-20260626-g555"));
        view.consecutive_failures = 2; // one short of the limit
        assert!(gate(&view, now).is_healable());
    }

    // --- the conjunction: precedence when several clauses fail -----------

    #[test]
    fn test_conjunction_kill_switch_dominates_all() {
        let now = Utc::now();
        // Every clause fails at once: the global kill-switch must win — an
        // operator gesture overrides every other signal (§5.4 re-checked first).
        let mut view = HealGuardView::healable(mid("cs-20260626-g560"));
        view.global_kill_switch = true;
        view.do_not_heal = true;
        view.pilot_present = true;
        view.last_whisper_at = Some(now - Duration::seconds(1));
        view.consecutive_failures = 9;
        view.last_heal_at = Some(now);
        view.last_remedy = Some(HealthRemedy::Nudge);
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::GlobalKillSwitch)
        );
    }

    #[test]
    fn test_conjunction_marker_beats_pilot_and_whisper() {
        let now = Utc::now();
        // No kill-switch, but marker + pilot + whisper all set: §5.3 wins.
        let mut view = HealGuardView::healable(mid("cs-20260626-g561"));
        view.do_not_heal = true;
        view.pilot_present = true;
        view.last_whisper_at = Some(now - Duration::seconds(1));
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::DoNotHealMarker)
        );
    }

    #[test]
    fn test_conjunction_pilot_beats_whisper_and_backoff() {
        let now = Utc::now();
        // Pilot + recent whisper + cooldown all active: the piloting clause
        // (§5.1) is reported ahead of the anti-thrash clauses.
        let mut view = HealGuardView::healable(mid("cs-20260626-g562"));
        view.pilot_present = true;
        view.last_whisper_at = Some(now - Duration::seconds(1));
        view.last_heal_at = Some(now);
        view.last_remedy = Some(HealthRemedy::Nudge);
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::LivePilot)
        );
    }

    #[test]
    fn test_conjunction_three_strikes_beats_backoff_cooldown() {
        let now = Utc::now();
        // Both anti-thrash clauses fire; the terminal three-strikes (§5.5
        // give-up) is reported ahead of the transient cooldown.
        let mut view = HealGuardView::healable(mid("cs-20260626-g563"));
        view.consecutive_failures = 5;
        view.last_heal_at = Some(now - Duration::seconds(1));
        view.last_remedy = Some(HealthRemedy::Nudge);
        assert_eq!(
            gate(&view, now),
            HealGate::Blocked(HealBlockReason::ThreeStrikes { failures: 5 })
        );
    }

    // --- stratification: the guard has no pane-text input (§2) ------------

    #[test]
    fn test_guard_reads_only_control_plane_no_pane_inference() {
        // Structural proof, the guard half of the be1e use/mention lesson:
        // `HealGuardView` carries only typed control-plane facts (presence,
        // whisper timestamp, tags, kill-switch, backoff). There is no field a
        // worker could fill by *printing* glyphs, so no scrollback can make the
        // guard open or close. A molecule whose pane loudly says "a human is
        // piloting me!" is still healable unless a real presence row or whisper
        // timestamp says so.
        let now = Utc::now();
        let view = HealGuardView::healable(mid("cs-20260626-g570"));
        assert!(gate(&view, now).is_healable());
    }

    // --- serde roundtrips (Stable-tier) ----------------------------------

    #[test]
    fn test_heal_gate_serde_roundtrip() {
        let gates = vec![
            HealGate::Heal,
            HealGate::Blocked(HealBlockReason::GlobalKillSwitch),
            HealGate::Blocked(HealBlockReason::DoNotHealMarker),
            HealGate::Blocked(HealBlockReason::LivePilot),
            HealGate::Blocked(HealBlockReason::WhisperQuietPeriod {
                secs_since_whisper: 120,
                quiet_secs: 600,
            }),
            HealGate::Blocked(HealBlockReason::ThreeStrikes { failures: 3 }),
            HealGate::Blocked(HealBlockReason::BackoffCooldown {
                remedy: HealthRemedy::Nudge,
                secs_since_last: 30,
                cooldown_secs: 60,
            }),
        ];
        for g in gates {
            let json = serde_json::to_string(&g).unwrap();
            let back: HealGate = serde_json::from_str(&json).unwrap();
            assert_eq!(back, g);
        }
    }

    #[test]
    fn test_guard_config_cooldown_for_each_remedy() {
        let cfg = GuardConfig::default();
        assert_eq!(cfg.cooldown_for(HealthRemedy::Nudge), Duration::seconds(60));
        assert_eq!(
            cfg.cooldown_for(HealthRemedy::HarvestDone),
            Duration::seconds(60)
        );
        assert_eq!(
            cfg.cooldown_for(HealthRemedy::TransportResubmit),
            Duration::seconds(60)
        );
        assert_eq!(
            cfg.cooldown_for(HealthRemedy::CollapseProcessDeath),
            Duration::minutes(5)
        );
        assert_eq!(
            cfg.cooldown_for(HealthRemedy::BackoffPerAccount),
            Duration::minutes(5)
        );
        assert_eq!(cfg.cooldown_for(HealthRemedy::FlagOnly), Duration::zero());
    }
}
