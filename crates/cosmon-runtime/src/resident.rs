// SPDX-License-Identifier: AGPL-3.0-only

//! `resident` — ADR-095 Resident Runtime skeleton.
//!
//! # Role in the architecture
//!
//! This module is the **CLI-client** loop body sketched by the
//! [ADR-095][adr] build camp (torvalds, forgemaster, karpathy). It is
//! deliberately distinct from the legacy [`crate::DagPolicy`] struct
//! (ADR-022 native scheduler, pre-IFBDD) because the two carry different
//! invariants:
//!
//! - Legacy `DagPolicy` mutates the [`cosmon_state::StateStore`] directly
//!   from inside the runtime process.
//! - ADR-095 `RuntimeLoop` (this module) is a *client* of the transactional
//!   core: it shells out to the `cs` CLI the same way a human would, and
//!   never imports a state-mutating crate at its module boundary. Removing
//!   this module from the workspace leaves [`cosmon_core`] green by
//!   construction (invariant RR-3).
//!
//! The mapping between the brief and the type names ratified by ADR-095:
//!
//! | brief vocabulary       | type in this module     |
//! |------------------------|-------------------------|
//! | "`DagPolicy` trait"    | [`ResidentScheduler`]   |
//! | "`RuntimeLoop`"        | [`RuntimeLoop`]         |
//!
//! The names were shifted to avoid collision with the legacy
//! `cosmon_runtime::DagPolicy` struct re-exported at the crate root.
//!
//! # Five named invariants (ADR-095 §2)
//!
//! - **RR-1 Client of the transactional core.** Every mutation goes through
//!   the `cs` binary on `PATH`. No `cosmon_state` / `cosmon_filestore`
//!   imports inside this module.
//! - **RR-2 Owns no state.** Decisions are derived from `cs ensemble --json`
//!   output; in-memory caches are advisory and re-derivable.
//! - **RR-3 Deletable as a single Cargo target.** Removing
//!   `crates/cosmon-runtime` leaves `cosmon-core` green; the only callers
//!   are `cosmon-cli` (the `cs run` adapter).
//! - **RR-4 JSON-on-disk authoritative.** A peer running `cs observe` in
//!   another shell sees the same state — the loop never mints a "true" view.
//! - **RR-5 Failure-mode observability baked in from day one.** Every
//!   decision and shell-out emits an NDJSON line to
//!   `.cosmon/state/runtime-trace.jsonl` *before* the side-effect, so an
//!   audit can reconstruct what the loop saw and what it decided. The four
//!   `RuntimeReadDecideWrite` / `RuntimeShelledOut` / `RuntimeMergeDispatched`
//!   / `RuntimeWorktreeClaimed` variants from `cosmon_core::EventV2`
//!   travel through the same trace stream.
//!
//! [adr]: https://github.com/noogram/cosmon/blob/main/docs/adr/095-resident-runtime-ifbdd-path.md
#![allow(clippy::module_name_repetitions)]

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use chrono::Utc;
use cosmon_core::event_v2::EventV2;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rand::Rng;
use serde::{Deserialize, Serialize};

/// Failure modes of the resident loop. Soft-contract: every error is also
/// captured in the NDJSON trace so a post-mortem reads the full record.
#[derive(Debug, thiserror::Error)]
pub enum ResidentError {
    /// I/O error opening / writing the trace file.
    #[error("trace I/O: {0}")]
    Trace(#[from] std::io::Error),

    /// A child `cs` subprocess could not be spawned or exited non-zero.
    #[error("cs {verb} failed for {mol_id}: {reason}")]
    CsInvocation {
        /// Which CLI verb was shelled out (`tackle`, `done`, …).
        verb: String,
        /// Target molecule id (may be empty for global verbs).
        mol_id: String,
        /// Underlying reason.
        reason: String,
    },

    /// `cs tackle` refused to dispatch a **briefless** molecule (exit
    /// [`cosmon_core::dispatch_refusal::BRIEFLESS_DISPATCH`], the
    /// task-20260711-919a guard). Unlike a generic `CsInvocation` failure,
    /// this refusal is **permanent**: the molecule carries no operator intent
    /// (its formula's required, default-free variables are missing or blank),
    /// so `cs tackle` will refuse it identically on every retry. The resident
    /// loop treats this variant specially — it **parks** the molecule (keeps
    /// the optimistic dispatch mark) instead of retracting it and re-emitting
    /// the dispatch next tick, which would busy-loop `cs tackle` forever. The
    /// refusal is still recorded on the decision trace (never silent);
    /// recovery is an operator restoring the brief from the molecule's
    /// `prompt.md` frontmatter, or collapsing it (task-20260711-4310).
    #[error("cs tackle refused briefless molecule {mol_id} (exit {code}): {reason}")]
    TackleRefusedBriefless {
        /// Molecule the runtime refused to dispatch briefless.
        mol_id: String,
        /// The refusal exit code (the briefless-dispatch guard code).
        code: i32,
        /// `cs tackle`'s stderr tail explaining the refusal.
        reason: String,
    },

    /// `cs ensemble --json` returned non-JSON or schema-incompatible output.
    #[error("ensemble parse error: {0}")]
    EnsembleParse(String),

    /// A child `cs` subprocess was killed by a signal (typically SIGINT or
    /// SIGTERM propagated to our process group during shutdown). The loop
    /// treats this as a benign shutdown handshake, not a real subprocess
    /// failure — the visible NDJSON trace should be the eventual `shutdown`
    /// line, not a spurious `{verb}` error whose stderr is empty because
    /// the child never got to print anything. Carries the verb so the
    /// post-mortem can still tell *which* subprocess was in flight when
    /// the signal landed.
    #[error("cs {verb} interrupted by signal (likely shutdown propagating to process group)")]
    SubprocessInterrupted {
        /// Which CLI verb was in flight (`ensemble`, `tackle`, `done`, …).
        verb: String,
    },

    /// FS watcher setup failed (notify backend).
    #[error("notify: {0}")]
    Notify(String),
}

impl From<notify::Error> for ResidentError {
    fn from(e: notify::Error) -> Self {
        Self::Notify(e.to_string())
    }
}

/// One molecule as the resident loop sees it through `cs ensemble --json`.
///
/// The shape is intentionally a thin projection — only the fields the
/// scheduler must inspect to decide *tackle* / *done* / *wait*. Adding
/// fields here is cheap; the loop ignores everything it doesn't read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EnsembleMolecule {
    /// Molecule id (e.g. `task-20260101-abcd`).
    pub id: String,
    /// Status as a `snake_case` string (`pending`, `running`, `completed`, …).
    pub status: String,
    /// Cognitive kind projected by `cs ensemble --json`. Decisions are
    /// reserved for the operator unless explicitly tagged `auto:ok`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Operational labels projected by `cs ensemble --json`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// IDs the molecule is blocked by. Empty for ready molecules.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// Merge stamp when `status == "completed"` — the merge-before-dispatch
    /// discriminant, carried as an RFC-3339 string (only its *presence* is
    /// read). A completed blocker clears its dependents **iff** this is set,
    /// mirroring `cosmon_state::frontier` (frontier.rs:214). `None` when the
    /// molecule has not merged (or the field is absent from the JSON). Kept as
    /// `Option<String>` rather than a parsed `DateTime` because the scheduler
    /// only needs `is_some()` — a stable, allocation-cheap presence check
    /// robust to any timestamp shape the CLI emits.
    #[serde(default)]
    pub merged_at: Option<String>,
    /// Stuck stamp when `status == "frozen"` — the load-bearing discriminant
    /// between the two Frozen species (convoy-cascade fix). A `cs stuck` freeze
    /// carries `Some(_)` and MUST hold its dependents; a *delivered* freeze
    /// (`freeze_on_last_step`) carries `None` and releases them. Mirrors
    /// `cosmon_state::frontier` (frontier.rs:210). `None` when not stuck (or
    /// absent from the JSON). See [`Self::merged_at`] for why it is a string.
    #[serde(default)]
    pub stuck_at: Option<String>,
    /// Adapter chosen before this runtime tick by a directional routing
    /// policy. The runtime carries it through to `cs tackle` unchanged.
    #[serde(default)]
    pub adapter: Option<String>,
}

/// Snapshot of the fleet handed to a [`ResidentScheduler`].
#[derive(Debug, Clone, Default)]
pub struct EnsembleSnapshot {
    /// All molecules currently visible to the runtime.
    pub molecules: Vec<EnsembleMolecule>,
}

impl EnsembleSnapshot {
    /// Parse the JSON emitted by `cs ensemble --json` into the per-molecule
    /// projection the scheduler reasons over.
    ///
    /// **Schema impedance.** The real `cs ensemble --json`
    /// output ships *two* molecule-shaped fields side by side:
    ///
    /// * `molecules` — a `MoleculeStatus → count` summary dict for the
    ///   operator dashboard ("how many running?").
    /// * `molecule_states` — an array of `{id, status, blocked_by}` for
    ///   machine readers ("which IDs are pending, with which blockers?").
    ///
    /// This parser tries the array forms in order:
    ///
    /// 1. Top-level `molecule_states: [...]` — the canonical shape produced
    ///    by the live CLI (added 2026-05-18).
    /// 2. Top-level `molecules: [...]` — the historical synthetic shape used
    ///    by this crate's own unit tests; preserved for backward compat so
    ///    the test corpus keeps passing without rewriting every fixture.
    ///
    /// If neither key carries an array, this is a real schema mismatch and
    /// we emit [`ResidentError::EnsembleParse`] — the loop will log it on
    /// the `runtime-trace.jsonl` line under `decision_basis:
    /// ensemble-read-failed` and try again next tick.
    ///
    /// # Errors
    ///
    /// Returns [`ResidentError::EnsembleParse`] if the input is not valid
    /// JSON or neither `molecule_states` nor `molecules` carries an array.
    pub fn from_json(text: &str) -> Result<Self, ResidentError> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ResidentError::EnsembleParse(e.to_string()))?;
        let arr = v
            .get("molecule_states")
            .and_then(|m| m.as_array())
            .or_else(|| v.get("molecules").and_then(|m| m.as_array()))
            .ok_or_else(|| {
                ResidentError::EnsembleParse(
                    "missing 'molecule_states' (or legacy 'molecules') array".into(),
                )
            })?;
        let mut molecules = Vec::with_capacity(arr.len());
        for entry in arr {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let status = entry
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let blocked_by = entry
                .get("blocked_by")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let kind = entry
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let tags = entry
                .get("tags")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            // Presence-only reads: the scheduler discriminates the two
            // terminal-but-gating species (completed→merged_at,
            // frozen→stuck_at) exactly as `cosmon_state::frontier` does.
            let merged_at = entry
                .get("merged_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let stuck_at = entry
                .get("stuck_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let adapter = entry
                .get("adapter")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            if id.is_empty() {
                continue;
            }
            molecules.push(EnsembleMolecule {
                id,
                status,
                kind,
                tags,
                blocked_by,
                merged_at,
                stuck_at,
                adapter,
            });
        }
        Ok(Self { molecules })
    }
}

/// Whether the operator has reserved this molecule from autonomous dispatch.
///
/// This is deliberately a pure snapshot predicate: the scheduler never reads
/// state itself, so a restart or a stale process cannot bypass a persisted
/// human hold. Decisions are reserved by default; `auto:ok` is the explicit
/// opt-in that permits the runtime to tackle one.
fn reserved_for_human(molecule: &EnsembleMolecule) -> bool {
    molecule.tags.iter().any(|tag| tag == "hold:human")
        || (molecule.kind.as_deref() == Some("decision")
            && !molecule.tags.iter().any(|tag| tag == "auto:ok"))
}

/// Whether the operator has reserved this molecule's *harvest* — the merge of
/// its completed branch — as a manual gesture, so the runtime must not
/// auto-`cs done` it to the trunk.
///
/// # Why this is distinct from [`reserved_for_human`]
///
/// `reserved_for_human` (`hold:human`) reserves the molecule from *all*
/// autonomous action, including the initial `cs tackle`. It must be set
/// *before* the worker runs. The harvest brake answers a different, later
/// need: a molecule the runtime was allowed to *work*, but whose *merge target*
/// the operator wants to control — because they have parked the whole line on
/// a spore branch pending validation, or want to route the merge elsewhere.
///
/// Without this brake, the resident loop's first sweep auto-harvests every
/// completed molecule to `main`, silently undoing an operator park (observed
/// 2026-07-20: `task-20260720-90d2` merged into `main` as `63fc899` despite the
/// math-attack line being parked on `spore/math-attack`). See ADR-156
/// (resident-runtime-safety-envelope) and the molecule `task-20260720-820a`
/// outcomes.
///
/// # The two forms of the brake
///
/// * **`no-auto-harvest`** — the explicit "reserve harvest as an operator
///   gesture" flag. The runtime leaves the completed molecule alone; a human
///   runs `cs done` (or merges by hand) when the park is lifted.
/// * **`harvest_to:<branch>`** — a *routing* intent naming a non-trunk merge
///   target. The resident loop can only ever merge to the trunk (its `cs done`
///   shell-out carries no branch argument), so any `harvest_to:` — regardless
///   of the branch named — is honored here as "not the runtime's to harvest":
///   the merge is reserved for the operator gesture that can route it. This is
///   the falsifier this molecule closes — *a runtime harvest event merging to
///   main for a molecule whose `harvest_to` points elsewhere* — held closed by
///   never emitting a `Done` for a `harvest_to`-tagged molecule.
///
/// Like [`reserved_for_human`], this is a pure snapshot predicate: a restart or
/// a stale process re-reads the persisted tag and cannot bypass the hold.
fn harvest_reserved(molecule: &EnsembleMolecule) -> bool {
    molecule
        .tags
        .iter()
        .any(|tag| tag == "no-auto-harvest" || tag.starts_with("harvest_to:"))
}

/// Whether merging this molecule requires an independent review verdict.
fn requires_review(molecule: &EnsembleMolecule) -> bool {
    molecule.tags.iter().any(|tag| {
        tag == "needs-review"
            || tag == "needs-review-cross-provider"
            || tag == "security"
            || tag.starts_with("security:")
    })
}

/// Whether the snapshot carries a review confirmation suitable for automatic
/// merge.
///
/// RR-SAFE-2 intentionally has no self-approval path yet: C2 holds a
/// review-required completed molecule for `cs done` by a human. The later
/// committee mechanism will project its independently-written verdict into
/// this snapshot and change this predicate; until then, fail closed.
fn review_confirmed_on_disk(_molecule: &EnsembleMolecule) -> bool {
    false
}

/// A decision produced by a [`ResidentScheduler`] for the loop to enact.
///
/// Variants map one-to-one with `cs` verbs so the trace and the audit
/// match on the same vocabulary the operator types in their shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Shell out `cs tackle <id>`, preserving an optional adapter choice.
    ///
    /// `None` is retained for externally supplied schedulers that deliberately
    /// delegate to the transactional CLI. [`ReadyFrontierScheduler`] always
    /// supplies the safe local floor for legacy snapshots. `Some` is an
    /// explicit partial incarnation and must never be silently replaced by
    /// the configured default.
    Tackle {
        /// Molecule to dispatch.
        molecule_id: String,
        /// Adapter selected by directional routing, if any.
        adapter: Option<String>,
    },
    /// Shell out `cs done <id>` to merge a completed molecule's branch.
    Done(String),
}

impl Decision {
    fn verb(&self) -> &'static str {
        match self {
            Self::Tackle { .. } => "tackle",
            Self::Done(_) => "done",
        }
    }

    fn molecule_id(&self) -> &str {
        match self {
            Self::Tackle { molecule_id, .. } | Self::Done(molecule_id) => molecule_id,
        }
    }

    fn adapter(&self) -> Option<&str> {
        match self {
            Self::Tackle { adapter, .. } => adapter.as_deref(),
            Self::Done(_) => None,
        }
    }
}

/// The pluggable scheduling policy for the resident loop.
///
/// Renamed from the brief's *"`DagPolicy` trait"* to avoid collision with the
/// legacy struct [`crate::DagPolicy`]. The contract is identical in spirit:
/// given a snapshot, return zero or more decisions for the loop to enact.
///
/// Implementations must be pure functions of the snapshot plus their own
/// monotone bookkeeping (e.g. "molecules I have already tackled") — they
/// must not perform I/O. The loop owns all side-effects.
pub trait ResidentScheduler: Send {
    /// Return the decisions to enact this tick, in dispatch order.
    fn next_decisions(&mut self, snapshot: &EnsembleSnapshot) -> Vec<Decision>;

    /// Tell the scheduler that a decision it emitted for `id` was **not
    /// enacted** — the loop's pre-dispatch anti-preemption recheck vetoed it,
    /// or the `cs` shell-out failed transiently.
    ///
    /// # Why this exists — the orphan-on-skip deadlock
    ///
    /// [`next_decisions`](Self::next_decisions) optimistically records a
    /// molecule as dispatched *the moment it emits the decision*, so that
    /// re-ticking on the same (up-to-one-poll-stale) snapshot does not
    /// double-dispatch. But the loop sits a `recheck_tackle_candidate` gate
    /// **between** the decision and the shell-out (anti-preemption lease),
    /// and that gate can *skip* the dispatch — on a human
    /// claim, a status flip, or, under CPU contention, a transient
    /// `cs observe` spawn/read failure (`TackleRecheck::SkipReadFailed`).
    /// The `cs tackle` shell-out itself can also fail transiently.
    ///
    /// Without a way to retract the optimistic mark, a skipped molecule is
    /// **orphaned**: it is still `pending` on disk, but the scheduler believes
    /// it is already tackled, so it never re-emits the `Tackle`. Its dependents
    /// never unblock, the DAG never drains, and the loop runs to its
    /// `max_runtime` ([`ExitReason::Deadline`]). This is the deterministic
    /// 60 s hang observed on a loaded dev machine: not a
    /// missed `fleet.json` write, but a vetoed dispatch the scheduler could not
    /// take back.
    ///
    /// Implementations that keep optimistic per-decision bookkeeping must drop
    /// it for `id` here so the molecule re-enters the ready frontier on the
    /// next tick. Stateless schedulers have nothing to forget — the default is
    /// a no-op.
    fn forget_dispatch(&mut self, _id: &str) {}
}

/// The default scheduler: walks the ready frontier exactly once per molecule.
///
/// Behaviour:
/// - A `pending` molecule whose `blocked_by` is empty (or all-completed)
///   yields a `Tackle`.
/// - A `completed` molecule that has not yet been done'd yields a `Done`.
/// - The scheduler tracks dispatched ids in monotone sets so a re-tick
///   never re-fires the same `Tackle` / `Done`. This is the bookkeeping
///   that survives a `kill -9` because it is re-derivable from the
///   on-disk status (RR-4): `tackled` ≈ "has ever transitioned to running",
///   `merged` ≈ "branch already gone". A restarted scheduler reconstructs
///   both from the snapshot.
#[derive(Debug, Default, Clone)]
pub struct ReadyFrontierScheduler {
    tackled: std::collections::HashSet<String>,
    merged: std::collections::HashSet<String>,
    /// Explicit, opt-in run-wide adapter directive (`cs run --adapter <name>`).
    ///
    /// A rung-1 flag intent the scheduler owns run-wide: when `Some`, every
    /// **pin-less** molecule this run dispatches is stamped with it — static
    /// frontier nodes AND children a worker nucleates dynamically mid-run (the
    /// frontier is re-derived from the fresh ensemble each tick, so a molecule
    /// that only appears later inherits the directive by construction). A
    /// per-molecule pin ([`EnsembleMolecule::adapter`]) still wins over it.
    /// `None` (the default) stamps nothing: the shelled `cs tackle` then runs
    /// the full canonical precedence chain itself (formula step →
    /// `$COSMON_DEFAULT_ADAPTER` → config → the `local` floor), so the operator's
    /// live env/config intent is honoured rather than masked (COSMON-DEV #21).
    run_adapter: Option<String>,
}

impl ReadyFrontierScheduler {
    /// Build an empty scheduler. Idempotent — every restart rebuilds
    /// `tackled` / `merged` from the snapshot on the first tick.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the explicit, opt-in run-wide adapter directive.
    ///
    /// `Some(name)` (from `cs run --adapter <name>`) stamps pin-less molecules
    /// dispatched this run with that adapter; `None` stamps nothing, letting the
    /// child `cs tackle` resolve the full canonical chain (env, config, then the
    /// `local` floor) on its own. This is the deliberate opt-in that lets a
    /// resident run drive cognitive nodes on a paid adapter without breaking the
    /// "never *silently* inherit a paid default" invariant (the operator chose
    /// it consciously, per invocation). A per-molecule pin still overrides the
    /// directive.
    #[must_use]
    pub fn with_run_adapter(mut self, run_adapter: Option<String>) -> Self {
        self.run_adapter = run_adapter;
        self
    }
}

impl ResidentScheduler for ReadyFrontierScheduler {
    fn next_decisions(&mut self, snapshot: &EnsembleSnapshot) -> Vec<Decision> {
        // A blocker is *cleared* (no longer gating its dependents) once it
        // reaches a terminal state whose semantics say "successors may run".
        // This MUST mirror `cosmon_state::frontier::compute_from_molecules`
        // (frontier.rs:181-220) exactly — the DEFAULT `cs run` path already
        // routes through that reducer (dag_policy.rs:664), and a coarse
        // status-string match here is precisely the divergence that
        // regressed the convoy-cascade fix (F-C11-1): a `cs stuck`'d blocker
        // ("NE PAS EXÉCUTER") was read as delivered and its successors were
        // flung at the runtime under `--resident`.
        //
        // - `collapsed` — releases successors unconditionally (the collapse
        //   cascade frees the lateral axis).
        // - `frozen` — TWO disjoint species that gate oppositely; the
        //   discriminant is `stuck_at` (frontier.rs:210). A *delivered* freeze
        //   (`stuck_at == None`, e.g. a mission-plan mission that decomposed)
        //   releases; a *stuck* freeze (`cs stuck` → `stuck_at.is_some()`,
        //   "do not execute — hold dependents") stays blocking.
        // - `completed` — cleared only when its branch is merged
        //   (`merged_at.is_some()`, merge-before-dispatch, frontier.rs:214) so
        //   the dependent's worktree carries the committed output. In the
        //   resident flow the loop's own `Done` sweep merges-and-tears-down a
        //   completed blocker; the dependent then chains on the next tick via
        //   the *absent* path below (`!present`), immediately since the
        //   teardown fires an FS event.
        let cleared: std::collections::HashSet<&str> = snapshot
            .molecules
            .iter()
            .filter(|m| match m.status.as_str() {
                "collapsed" => true,
                "frozen" => m.stuck_at.is_none(),
                "completed" => m.merged_at.is_some(),
                _ => false,
            })
            .map(|m| m.id.as_str())
            .collect();
        // Ids still present in the snapshot. A blocker absent from this set
        // has been torn down by `cs done` (which merges before removing), so
        // it no longer blocks — treating a torn-down blocker as still-blocking
        // is the BUG-2 drain: `cs run` auto-`cs done`s each stage, so the next
        // stage's blockers vanish from the ensemble before it can chain.
        let present: std::collections::HashSet<&str> =
            snapshot.molecules.iter().map(|m| m.id.as_str()).collect();
        // Re-derive `tackled` from disk state on every tick — a molecule
        // that has ever transitioned past `pending` is structurally
        // tackled regardless of what our in-memory set says.
        for m in &snapshot.molecules {
            if m.status != "pending" {
                self.tackled.insert(m.id.clone());
            }
        }
        let mut out = Vec::new();
        // First sweep: merge ready completed molecules (so dependents see
        // their branches in the worktree on the next tackle).
        for m in &snapshot.molecules {
            if m.status == "completed" && !self.merged.contains(&m.id) {
                if reserved_for_human(m) {
                    continue;
                }
                // Harvest brake: the operator has parked this molecule's merge
                // (or routed it to a non-trunk branch). The runtime can only
                // merge to the trunk, so it must keep its hands off and leave
                // the harvest as an operator gesture. See `harvest_reserved`.
                if harvest_reserved(m) {
                    continue;
                }
                if requires_review(m) && !review_confirmed_on_disk(m) {
                    continue;
                }
                out.push(Decision::Done(m.id.clone()));
                self.merged.insert(m.id.clone());
            }
        }
        // Second sweep: dispatch pending molecules with all blockers done.
        for m in &snapshot.molecules {
            if m.status != "pending" || self.tackled.contains(&m.id) {
                continue;
            }
            if reserved_for_human(m) {
                continue;
            }
            // A blocker is satisfied when it is cleared (terminal) OR no
            // longer present (torn down by `cs done`). This is what lets the
            // loop *chain* across stage boundaries instead of draining the
            // moment a completed blocker is merged-and-removed.
            let unblocked = m
                .blocked_by
                .iter()
                .all(|b| cleared.contains(b.as_str()) || !present.contains(b.as_str()));
            if unblocked {
                out.push(Decision::Tackle {
                    molecule_id: m.id.clone(),
                    // The scheduler owns exactly the two rung-1 flag intents and
                    // NOTHING below them (COSMON-DEV #21, G1 contract §1):
                    //   1. a per-molecule pin (`m.adapter`),
                    //   2. an explicit opt-in run directive (`self.run_adapter`,
                    //      from `cs run --adapter <name>`).
                    // When BOTH are absent it emits `None` — no `--adapter`
                    // flag on the shelled `cs tackle` — so the child runs the
                    // *full* canonical precedence chain (formula step →
                    // `$COSMON_DEFAULT_ADAPTER` → per-galaxy config → global
                    // config → the `local` floor). Substituting the floor here
                    // would render `--adapter local`, occupy rung 1, and mask
                    // the operator's live env/config intent — the exact defect
                    // #21 reports. The floor is not deleted; it moves to its
                    // correct owner, the canonical resolver's rung 6
                    // (`BUILTIN_FLOOR_ADAPTER`), reached by the child iff no
                    // higher rung speaks.
                    adapter: m.adapter.clone().or_else(|| self.run_adapter.clone()),
                });
                self.tackled.insert(m.id.clone());
            }
        }
        out
    }

    /// Drop the optimistic dispatch mark for `id` so the next tick re-derives
    /// readiness purely from on-disk state. Removing from both `tackled` and
    /// `merged` covers the two decision kinds the loop can veto or fail —
    /// a vetoed `Tackle` (the orphan deadlock this method exists to break) and
    /// a failed `Done`. Re-derivation at the top of [`Self::next_decisions`]
    /// re-adds the mark immediately if the molecule actually did move past
    /// `pending` on disk, so a successful-but-reported-failed dispatch is not
    /// re-fired.
    fn forget_dispatch(&mut self, id: &str) {
        self.tackled.remove(id);
        self.merged.remove(id);
    }
}

/// Configuration for a [`RuntimeLoop`].
#[derive(Debug, Clone)]
pub struct RuntimeLoopConfig {
    /// Project root containing `.cosmon/`. Passed as the cwd for every
    /// shelled `cs` call so it discovers the right state directory.
    pub cwd: PathBuf,
    /// Fall-back polling interval. The loop also wakes on FS events under
    /// `.cosmon/state/`; this is the heartbeat that fires when nothing
    /// changes on disk (e.g. the watcher is stalled).
    pub poll_interval: Duration,
    /// Maximum total runtime before the loop exits regardless of state.
    /// `None` means run forever (until SIGTERM / drained).
    pub max_runtime: Option<Duration>,
    /// Path to the `cs` binary. Defaults to `"cs"` resolved on `PATH`.
    pub cs_binary: PathBuf,
}

impl RuntimeLoopConfig {
    /// Build a default config rooted at `cwd`. Poll interval defaults to
    /// 1s, max runtime is unbounded, `cs` resolved on PATH.
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            poll_interval: Duration::from_secs(1),
            max_runtime: None,
            cs_binary: PathBuf::from("cs"),
        }
    }
}

/// Why a tick of the loop exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    /// SIGTERM / Ctrl-C set the shutdown flag.
    Shutdown,
    /// The ensemble has no `pending` and no `running` molecules left.
    Drained,
    /// The `max_runtime` budget was exhausted.
    Deadline,
    /// **Config-honoring dispatch.** The on-disk
    /// config changed since launch: the launch seal
    /// `H = BLAKE3(resolved_config)` no longer matches the seal recomputed
    /// before a dispatch. The loop **halted fail-closed** rather than
    /// dispatch on a stale launch snapshot. The `cs run` adapter maps this
    /// to a non-zero process exit so a supervisor relaunches a fresh
    /// process that re-derives engine + config from disk
    /// (bounded-ephemeral — never self-repair).
    ///
    /// **The seal does NOT include the `cs` binary image.**
    /// It used to: `H = BLAKE3(config ⊕
    /// binary-image-id)`. But `cs done`'s post-merge hook runs
    /// `just install`, which overwrites the very `cs` binary the runtime
    /// sealed against — so the propulsion *died every time it successfully
    /// drained a DAG* (a merge reinstalls the binary → next tick
    /// `current_seal ≠ launch_seal` → exit 75). A runtime cannot survive
    /// its own success if it seals against its own binary. The fix is to
    /// **seal the config, not the binary**: a binary reinstall mid-run is
    /// the *expected* steady state, not drift.
    ConfigDrift,
}

/// Summary of one `RuntimeLoop::run` invocation.
#[derive(Debug, Clone)]
pub struct RunSummary {
    /// Number of `cs tackle` shell-outs the loop performed.
    pub tackles: u32,
    /// Number of `cs done` shell-outs the loop performed.
    pub dones: u32,
    /// Number of phantom-running reap sweeps the loop fired.
    /// Each sweep shells `cs patrol --auto-collapse`
    /// to terminally collapse `Running` molecules whose worker is provably
    /// dead, so a stalled DAG drains instead of waiting on a corpse forever.
    pub reaps: u32,
    /// Number of complete ticks (snapshot → decisions → side-effects).
    pub ticks: u32,
    /// Number of **briefless** molecules the loop parked instead of
    /// dispatching — molecules `cs tackle` refused with the briefless-dispatch
    /// guard exit code (task-20260711-919a). Each is counted once per
    /// refusal, not once per tick: the whole point of parking is that a
    /// briefless molecule is *not* re-attempted every tick (task-20260711-4310).
    /// A non-zero value means the operator has molecules that need a brief
    /// restored (from `prompt.md` frontmatter) or a collapse.
    pub briefless_parked: u32,
    /// Why the loop exited.
    pub exit: ExitReason,
}

/// The ADR-095 resident loop body.
///
/// One loop = one process. The owner constructs the loop with a
/// scheduler and a config, then calls [`RuntimeLoop::run`] passing a
/// shutdown flag that an out-of-band SIGTERM handler can flip to `true`.
pub struct RuntimeLoop {
    config: RuntimeLoopConfig,
    scheduler: Box<dyn ResidentScheduler>,
    trace: TraceWriter,
    /// **Config-honoring dispatch.** The seal
    /// `H = BLAKE3(resolved_config)` sealed at launch. This is the
    /// runtime's *witness obligation*: it carries the digest of the config
    /// it derived its dispatch behaviour from, and re-checks it before
    /// every dispatch. Empty until [`RuntimeLoop::run`] computes it (the
    /// seal reads files, so it is set at launch rather than in the
    /// infallible constructor). See [`config_seal`].
    ///
    /// **The seal deliberately omits the `cs` binary image.**
    /// Sealing against the binary made the runtime
    /// self-poison: `cs done`'s post-merge `just install` rewrote the
    /// sealed binary, tripping the seal on the very next tick after a
    /// successful drain. See [`ExitReason::ConfigDrift`].
    launch_seal: String,
}

impl RuntimeLoop {
    /// Build a new loop. The trace file is opened (and appended-to) on
    /// the first call to [`RuntimeLoop::run`], not here, so the
    /// constructor stays infallible.
    #[must_use]
    pub fn new(config: RuntimeLoopConfig, scheduler: Box<dyn ResidentScheduler>) -> Self {
        let trace_path = config
            .cwd
            .join(".cosmon")
            .join("state")
            .join("runtime-trace.jsonl");
        Self {
            config,
            scheduler,
            trace: TraceWriter::new(trace_path),
            launch_seal: String::new(),
        }
    }

    /// Path the NDJSON trace will be written to.
    #[must_use]
    pub fn trace_path(&self) -> &Path {
        self.trace.path()
    }

    /// Drive the loop until the shutdown flag fires, the DAG drains, or
    /// the deadline is reached.
    ///
    /// # Errors
    ///
    /// Returns the first unrecoverable error the loop encountered.
    /// Per-tick errors (e.g. a single `cs tackle` exit non-zero) are
    /// logged into the trace and the loop continues — only setup errors
    /// (trace open, watcher start) bubble up here.
    // The body is one tight state machine that reads more naturally as a
    // single function than split across helpers (every branch threads the
    // same `summary` / `rx` / `shutdown` triple). Same allow pattern as
    // `cosmon_filestore::migrate_legacy_line` (efb34b531).
    #[allow(clippy::too_many_lines)]
    pub fn run(&mut self, shutdown: &Arc<AtomicBool>) -> Result<RunSummary, ResidentError> {
        self.trace.open()?;
        // Config-honoring dispatch (delib-20260531-c761): seal the config the
        // runtime is about to derive its dispatch behaviour from. This is the
        // launch half of the witness obligation; the pre-dispatch half
        // re-checks it before every `cs tackle` / `cs done`. The `cs` binary
        // image is deliberately NOT sealed (task-20260608-1c59) — `cs done`'s
        // post-merge `just install` reinstalls it on every successful drain,
        // and a runtime that sealed against its own binary could not survive
        // its own success.
        self.launch_seal = config_seal(&self.config);
        self.trace.write_tick(
            "launch",
            "config-seal-sealed",
            Some(&self.launch_seal),
            Some(&self.launch_seal),
            None,
        )?;
        let (tx, rx) = mpsc::channel::<()>();
        let _watcher = spawn_watcher(&self.config.cwd, tx)?;
        let started = Instant::now();
        let mut summary = RunSummary {
            tackles: 0,
            dones: 0,
            reaps: 0,
            ticks: 0,
            briefless_parked: 0,
            exit: ExitReason::Drained,
        };
        // Phantom-running reap gate (task-20260606-21d4, DoD a). Counts
        // consecutive ticks where the loop had *no* decisions yet the DAG was
        // *not* drained — i.e. only `running`/`queued` molecules remain and
        // none are advancing. A live worker mid-step produces exactly this
        // shape too, so the count alone never reaps: it merely rate-limits how
        // often we fire the *liveness-judged* sweep. The sweep itself
        // (`cs patrol --auto-collapse`) is a no-op for any molecule whose
        // worker still has a live tmux session, so a slow-but-alive worker is
        // never collapsed (ADR-116 "the trap").
        let mut consecutive_stall_ticks: u32 = 0;

        loop {
            if shutdown.load(Ordering::SeqCst) {
                summary.exit = ExitReason::Shutdown;
                self.trace
                    .write_tick("shutdown", "operator-signal", None, None, None)?;
                break;
            }
            if let Some(budget) = self.config.max_runtime {
                if started.elapsed() >= budget {
                    summary.exit = ExitReason::Deadline;
                    self.trace
                        .write_tick("deadline", "max-runtime-exhausted", None, None, None)?;
                    break;
                }
            }

            let hash_before = state_hash(&self.config.cwd);
            let snapshot = match read_ensemble(&self.config) {
                Ok(s) => s,
                Err(ResidentError::SubprocessInterrupted { .. }) => {
                    // SIGINT / SIGTERM hit the in-flight `cs ensemble`
                    // child before our own handler flipped the shutdown
                    // flag. Treat it as the shutdown handshake — write the
                    // shutdown trace and break, rather than a spurious
                    // `ensemble-read-failed` line that misleads
                    // post-mortems (task-20260518-eb67).
                    summary.exit = ExitReason::Shutdown;
                    self.trace
                        .write_tick("shutdown", "operator-signal", None, None, None)?;
                    break;
                }
                Err(e) => {
                    // Belt-and-suspenders: a non-signal failure that
                    // *happens* to coincide with a shutdown flag is still
                    // a clean shutdown — re-check the flag here so the
                    // last line in the trace remains `shutdown`, not
                    // `ensemble-read-failed`.
                    if shutdown.load(Ordering::SeqCst) {
                        summary.exit = ExitReason::Shutdown;
                        self.trace
                            .write_tick("shutdown", "operator-signal", None, None, None)?;
                        break;
                    }
                    self.trace.write_tick(
                        "tick",
                        "ensemble-read-failed",
                        Some(&hash_before),
                        Some(&hash_before),
                        Some(&e.to_string()),
                    )?;
                    // Drain the FS-event channel so we don't spin if
                    // the watcher fired during the failed read.
                    drain_rx(&rx);
                    wait_for_event_or_timeout(&rx, self.config.poll_interval);
                    continue;
                }
            };
            let decisions = self.scheduler.next_decisions(&snapshot);

            if decisions.is_empty() {
                let drained = !snapshot
                    .molecules
                    .iter()
                    .any(|m| m.status == "pending" || m.status == "running");
                let hash_after = state_hash(&self.config.cwd);
                self.trace.write_tick(
                    if drained { "drained" } else { "tick" },
                    if drained {
                        "no-pending-no-running"
                    } else {
                        "no-decisions"
                    },
                    Some(&hash_before),
                    Some(&hash_after),
                    None,
                )?;
                if drained {
                    summary.exit = ExitReason::Drained;
                    break;
                }
                // Not drained, no decisions: the loop is stalled on molecules
                // that are `running`/`queued` but not advancing. Accumulate
                // stall ticks; once the gate trips, fire one phantom-running
                // reap sweep (task-20260606-21d4, DoD a). Without this the loop
                // waits forever on a molecule whose worker died — the reservoir
                // never drains and the operator has to hand-collapse corpses.
                consecutive_stall_ticks = consecutive_stall_ticks.saturating_add(1);
                if consecutive_stall_ticks >= STALL_TICKS_BEFORE_REAP {
                    let reaped = reap_phantom_running(&self.config);
                    let hash_post = state_hash(&self.config.cwd);
                    match reaped {
                        Ok(n) => {
                            let detail = (n > 0).then(|| format!("collapsed {n} phantom-running"));
                            self.trace.write_tick(
                                "reap",
                                "phantom-running-sweep",
                                Some(&hash_after),
                                Some(&hash_post),
                                detail.as_deref(),
                            )?;
                            if n > 0 {
                                summary.reaps = summary.reaps.saturating_add(1);
                            }
                        }
                        Err(e) => {
                            let detail = e.to_string();
                            self.trace.write_tick(
                                "reap",
                                "phantom-running-sweep-failed",
                                Some(&hash_after),
                                Some(&hash_post),
                                Some(&detail),
                            )?;
                        }
                    }
                    // Reset so the next sweep is another full gate-window away
                    // — the sweep is liveness-judged and idempotent, but firing
                    // it every tick would needlessly re-probe the whole fleet.
                    consecutive_stall_ticks = 0;
                }
                summary.ticks = summary.ticks.saturating_add(1);
                drain_rx(&rx);
                wait_for_event_or_timeout(&rx, self.config.poll_interval);
                continue;
            }
            // Any tick that produced decisions means the frontier is moving —
            // reset the phantom-running stall gate.
            consecutive_stall_ticks = 0;

            let mut interrupted = false;
            let mut drifted = false;
            for d in decisions {
                // Config-honoring dispatch (delib-20260531-c761): re-derive
                // the seal from the *current* on-disk config and refuse
                // to FORM the dispatch if it drifted from launch. This is
                // carnot's irreversibility boundary — we never let the wrong
                // request exist, rather than catching it after it is sent.
                // The only sound move on drift is to *stop* and let a fresh
                // launch re-derive from disk (godel: a running process cannot
                // prove "I am currently fresh" while still running); we never
                // reload in place.
                let current_seal = config_seal(&self.config);
                if current_seal != self.launch_seal {
                    let event = EventV2::ConfigDriftDetected {
                        launch_seal: self.launch_seal.clone(),
                        current_seal: current_seal.clone(),
                        refused_verb: d.verb().to_owned(),
                        refused_molecule: Some(d.molecule_id().to_owned()),
                    };
                    self.trace
                        .write_drift(&self.launch_seal, &current_seal, &event)?;
                    summary.exit = ExitReason::ConfigDrift;
                    drifted = true;
                    break;
                }
                let invocation = invocation_uuid();
                // Anti-preemption lease (task-20260531-a12f): for a Tackle,
                // re-read the candidate fresh from disk RIGHT BEFORE the
                // shell-out. The scheduler's snapshot is up to one
                // poll-interval stale; this read closes the race where the
                // runtime raffles a molecule a human just tackled. Skip a
                // candidate that is no longer `pending` or carries a sticky
                // `human` claim, recording the reason in the trace.
                if let Decision::Tackle { molecule_id, .. } = &d {
                    let verdict = recheck_tackle_candidate(&self.config, molecule_id);
                    if verdict != TackleRecheck::Dispatch {
                        let hash_now = state_hash(&self.config.cwd);
                        self.trace.write_decision(&DecisionRecord {
                            verb: "skip",
                            molecule_id,
                            invocation: &invocation,
                            basis: verdict.basis(),
                            before: &hash_before,
                            after: &hash_now,
                            error: None,
                        })?;
                        // The dispatch was vetoed — retract the scheduler's
                        // optimistic tackle mark so the molecule re-enters the
                        // ready frontier next tick. Skipping this is the
                        // orphan-on-skip deadlock (task-20260601-4c03): the
                        // molecule stays `pending` on disk yet is never
                        // re-emitted, so the DAG runs to `max_runtime`.
                        self.scheduler.forget_dispatch(molecule_id);
                        continue;
                    }
                }
                let basis = format!("ready-frontier:{}", d.verb());
                let result = shell_out(&self.config, &d);
                let hash_after = state_hash(&self.config.cwd);
                // SIGINT / SIGTERM in flight: suppress the spurious
                // `cs done failed for <id>: exit -1: ` decision record
                // and let the post-loop shutdown branch own the trace
                // (task-20260518-eb67). The decision *was attempted* —
                // but the only honest signal is "we're shutting down".
                if matches!(&result, Err(ResidentError::SubprocessInterrupted { .. })) {
                    interrupted = true;
                    break;
                }
                let err = result.as_ref().err().map(ToString::to_string);
                self.trace.write_decision(&DecisionRecord {
                    verb: d.verb(),
                    molecule_id: d.molecule_id(),
                    invocation: &invocation,
                    basis: &basis,
                    before: &hash_before,
                    after: &hash_after,
                    error: err.as_deref(),
                })?;
                if result.is_ok() {
                    match d {
                        Decision::Tackle { .. } => {
                            summary.tackles = summary.tackles.saturating_add(1);
                        }
                        Decision::Done(_) => summary.dones = summary.dones.saturating_add(1),
                    }
                } else if matches!(&result, Err(ResidentError::TackleRefusedBriefless { .. })) {
                    // Permanent refusal (task-20260711-919a briefless guard,
                    // read as task-20260711-4310): the molecule carries no
                    // brief, so `cs tackle` will refuse it identically forever.
                    // Do NOT `forget_dispatch` — keep the optimistic mark so
                    // the molecule is *parked* rather than re-emitted every
                    // tick. Retracting it here is exactly the busy-loop this
                    // arm exists to prevent: `cs tackle` spawned each poll
                    // interval, the trace flooded, and — because every tick
                    // then "produces decisions" — the phantom-running stall
                    // gate perpetually reset, starving the reap sweep. The
                    // decision record written just above already carries the
                    // refusal reason, so the park is visible, not silent.
                    // Recovery is an operator restoring the brief (prompt.md
                    // frontmatter) or collapsing the molecule; a fresh `cs run`
                    // re-derives from disk and will attempt it exactly once
                    // more (one warn, then parked again).
                    summary.briefless_parked = summary.briefless_parked.saturating_add(1);
                } else {
                    // The shell-out failed (non-zero, non-signal). Retract the
                    // optimistic mark so a transient `cs tackle` / `cs done`
                    // failure is retried next tick instead of orphaning the
                    // molecule (same deadlock class as the recheck-skip path).
                    // If the verb actually took effect on disk despite the
                    // reported failure, the next tick's disk re-derivation
                    // re-adds the mark, so this never double-fires.
                    self.scheduler.forget_dispatch(d.molecule_id());
                }
            }
            if drifted {
                // Halt fail-closed: the drift trace line is already written
                // (and carries the typed `ConfigDriftDetected` event). Break
                // the loop so the `cs run` adapter can exit non-zero.
                break;
            }
            if interrupted {
                summary.exit = ExitReason::Shutdown;
                self.trace
                    .write_tick("shutdown", "operator-signal", None, None, None)?;
                break;
            }
            summary.ticks = summary.ticks.saturating_add(1);
            drain_rx(&rx);
            wait_for_event_or_timeout(&rx, self.config.poll_interval);
        }

        self.trace.flush()?;
        Ok(summary)
    }
}

// ---------------------------------------------------------------------------
// Helpers (private)
// ---------------------------------------------------------------------------

/// Verdict of the pre-dispatch re-read for a `Tackle` candidate.
///
/// `Dispatch` means the fresh on-disk read confirms the molecule is still
/// `pending` and not pilot-held — the shell-out may proceed. Every other
/// variant is a *skip with a reason* the loop records in the trace.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TackleRecheck {
    /// Re-read confirms `pending` + no human claim or pilot hold — go ahead.
    Dispatch,
    /// Skip — the candidate is no longer `pending` (flipped to Active /
    /// Completed / … since the stale snapshot was taken).
    SkipNotPending(String),
    /// Skip — a human holds the sticky dispatch claim. "Manual always wins."
    SkipHumanClaim,
    /// Skip — a fresh operator hold or reserved decision appeared after the
    /// scheduler snapshot. This closes the final snapshot-to-shell-out race.
    SkipHumanReservation,
    /// Skip — the pilot reserved this molecule before tackling it.
    SkipPilotHold,
    /// Skip — the fresh read itself failed. Conservative: do not dispatch on
    /// an unknown state; the candidate stays in the frontier for next tick.
    SkipReadFailed(String),
}

impl TackleRecheck {
    /// Short, stable token for the NDJSON `decision_basis` field.
    fn basis(&self) -> &'static str {
        match self {
            Self::Dispatch => "recheck:dispatch",
            Self::SkipNotPending(_) => "recheck:skip-not-pending",
            Self::SkipHumanClaim => "recheck:skip-human-claim",
            Self::SkipHumanReservation => "recheck:skip-human-reservation",
            Self::SkipPilotHold => "recheck:skip-pilot-hold",
            Self::SkipReadFailed(_) => "recheck:skip-read-failed",
        }
    }
}

/// Re-read a `Tackle` candidate's state fresh from disk **immediately before
/// dispatch** and decide whether the shell-out may proceed.
///
/// This is the resident-loop half of the anti-preemption lease.
/// The `EnsembleSnapshot` the
/// scheduler reasoned over is up to one poll-interval stale; trusting it lets
/// the runtime raffle a molecule a human `cs tackle`d in the interim — a
/// convoy-cascade-class race. We respect RR-1 (client of the transactional
/// core: no `cosmon_state` import here) by re-reading through
/// `cs observe <id> --json`, the same way the loop already reads
/// `cs ensemble --json`. The shape we need is `status` + `tackled_by` + `tags`.
///
/// "Manual always wins" is a binary owner field, not a clock: any candidate
/// whose `tackled_by == "human"` is skipped, even if it briefly returned to
/// `pending` on a revision.
fn recheck_tackle_candidate(config: &RuntimeLoopConfig, mol_id: &str) -> TackleRecheck {
    let output = match Command::new(&config.cs_binary)
        .args(["observe", mol_id, "--json"])
        .current_dir(&config.cwd)
        .output()
    {
        Ok(o) => o,
        Err(e) => return TackleRecheck::SkipReadFailed(format!("spawn failed: {e}")),
    };
    if !output.status.success() {
        return TackleRecheck::SkipReadFailed(format!(
            "cs observe exit {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    let text = match String::from_utf8(output.stdout) {
        Ok(t) => t,
        Err(e) => return TackleRecheck::SkipReadFailed(format!("non-utf8 stdout: {e}")),
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return TackleRecheck::SkipReadFailed(format!("json parse: {e}")),
    };
    let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if status != "pending" {
        return TackleRecheck::SkipNotPending(status.to_owned());
    }
    // `tackled_by` is the flat string `"human"` / `"runtime:<pid>"`
    // (omitted entirely for never-tackled / legacy molecules). Only a human
    // claim blocks dispatch.
    if value.get("tackled_by").and_then(serde_json::Value::as_str) == Some("human") {
        return TackleRecheck::SkipHumanClaim;
    }
    let tags = value.get("tags").and_then(serde_json::Value::as_array);
    let has_tag =
        |wanted: &str| tags.is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(wanted)));
    if has_tag("hold:human")
        || (value.get("kind").and_then(serde_json::Value::as_str) == Some("decision")
            && !has_tag("auto:ok"))
    {
        return TackleRecheck::SkipHumanReservation;
    }
    // `cs claim` persists this marker before a pilot starts `cs tackle`.
    // Treat every pilot hold as authoritative: the runtime's ambition is
    // advisory, and a positive reservation closes the pending-work race.
    if has_tag("hold:pilot") {
        return TackleRecheck::SkipPilotHold;
    }
    TackleRecheck::Dispatch
}

/// Consecutive no-decision-but-not-drained ticks before the loop fires a
/// phantom-running reap sweep.
///
/// Sized so a *freshly* tackled worker — whose tmux session needs a moment
/// to materialise — is never reaped before it can prove liveness, while a
/// genuinely dead worker is collapsed within a handful of poll intervals.
/// The sweep itself is liveness-judged, so this is a frequency limiter, not
/// the safety guard: a live worker survives no matter how often we sweep.
const STALL_TICKS_BEFORE_REAP: u32 = 5;

/// Fire one phantom-running reap sweep by shelling `cs patrol --auto-collapse`.
///
/// This is the loop's hands-off remedy for the *flotte aveugle* deadlock: a
/// molecule stuck `Running` because its worker died (dead tmux pane, mid-run
/// API loss, OS kill) keeps every downstream waiter blocked forever — the DAG
/// can neither advance past it nor drain. `cs patrol --auto-collapse` is the
/// liveness-judged remediation that already exists in the transactional core
/// (ADR-116 Part B): it transitions an orphaned `Running` molecule — one whose
/// worker is missing or whose tmux session is provably dead — to the terminal
/// `Collapsed`, which unblocks dependents and lets the loop drain. A
/// slow-but-*alive* worker keeps a live session, so the sweep leaves it
/// untouched; that is the ADR's central guard against reaping live work.
///
/// We shell out the same way the loop shells `tackle` / `done` (RR-1: the
/// runtime is a *client* of the transactional core, never an importer of
/// `cosmon_state`). Returns the number of molecules the sweep collapsed,
/// parsed best-effort from `--json`; a parse miss degrades to `0` rather than
/// an error, because the sweep's side effect — not our count of it — is what
/// unblocks the DAG.
///
/// # Errors
///
/// Returns [`ResidentError::CsInvocation`] when the `cs` binary cannot be
/// spawned or exits non-zero. A signal-kill (operator Ctrl-C in flight) maps
/// to [`ResidentError::SubprocessInterrupted`] so the loop's shutdown handshake
/// owns the trace, exactly like [`read_ensemble`].
fn reap_phantom_running(config: &RuntimeLoopConfig) -> Result<u32, ResidentError> {
    let output = Command::new(&config.cs_binary)
        .args(["patrol", "--auto-collapse", "--json"])
        .current_dir(&config.cwd)
        .output()
        .map_err(|e| ResidentError::CsInvocation {
            verb: "patrol".into(),
            mol_id: String::new(),
            reason: format!("spawn failed: {e}"),
        })?;
    if !output.status.success() {
        return Err(if output.status.code().is_none() {
            ResidentError::SubprocessInterrupted {
                verb: "patrol".into(),
            }
        } else {
            ResidentError::CsInvocation {
                verb: "patrol".into(),
                mol_id: String::new(),
                reason: format!(
                    "exit {}: {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }
        });
    }
    // `cs patrol --auto-collapse --json` emits the collapsed set under
    // `auto_transitioned.molecules` (only when non-empty). Count it
    // best-effort; any shape we don't recognise means "nothing reaped".
    let count = String::from_utf8(output.stdout)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("auto_transitioned")
                .and_then(|a| a.get("molecules"))
                .and_then(|m| m.as_array())
                .map(Vec::len)
        })
        .unwrap_or(0);
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

fn read_ensemble(config: &RuntimeLoopConfig) -> Result<EnsembleSnapshot, ResidentError> {
    let output = Command::new(&config.cs_binary)
        .args(["ensemble", "--json"])
        .current_dir(&config.cwd)
        .output()
        .map_err(|e| ResidentError::CsInvocation {
            verb: "ensemble".into(),
            mol_id: String::new(),
            reason: format!("spawn failed: {e}"),
        })?;
    if !output.status.success() {
        // Distinguish signal-kill from non-zero exit. `status.code()` returns
        // `None` only when the child was terminated by a signal — typically
        // SIGINT or SIGTERM propagated to our process group (operator Ctrl-C,
        // `timeout --signal=INT`, etc.). The caller (the loop body) inspects
        // [`ResidentError::SubprocessInterrupted`] to suppress the spurious
        // `ensemble-read-failed` trace line that would otherwise be the last
        // record before the (immediately following) `shutdown` line. See
        // task-20260518-eb67 — the "fix-demi-cuit" sibling of task ‥-8429.
        return Err(if output.status.code().is_none() {
            ResidentError::SubprocessInterrupted {
                verb: "ensemble".into(),
            }
        } else {
            ResidentError::CsInvocation {
                verb: "ensemble".into(),
                mol_id: String::new(),
                reason: format!(
                    "exit {}: {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }
        });
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| ResidentError::EnsembleParse(format!("non-utf8 stdout: {e}")))?;
    EnsembleSnapshot::from_json(&text)
}

fn shell_out(config: &RuntimeLoopConfig, d: &Decision) -> Result<(), ResidentError> {
    let mut cmd = Command::new(&config.cs_binary);
    cmd.args(shell_out_args(d, std::process::id()))
        .current_dir(&config.cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .env("COSMON_RUNTIME_ACTIVE", "1");
    let output = cmd.output().map_err(|e| ResidentError::CsInvocation {
        verb: d.verb().into(),
        mol_id: d.molecule_id().into(),
        reason: format!("spawn failed: {e}"),
    })?;
    if !output.status.success() {
        // Same SIGINT-race protection as [`read_ensemble`]: a child killed
        // by signal almost always means our own process group is shutting
        // down. Surface a distinct variant so the loop body suppresses the
        // spurious `cs done failed for <id>: exit -1: ` trace line that
        // would otherwise be the last record before the `shutdown` line.
        let code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if code.is_none() {
            ResidentError::SubprocessInterrupted {
                verb: d.verb().into(),
            }
        } else if matches!(d, Decision::Tackle { .. })
            && cosmon_core::dispatch_refusal::is_briefless_refusal(code)
        {
            // Permanent refusal (task-20260711-919a briefless guard): the
            // molecule carries no brief and `cs tackle` will refuse it
            // identically on every retry. Surface a distinct variant so the
            // loop parks the molecule instead of re-arming the dispatch
            // busy-loop (task-20260711-4310).
            ResidentError::TackleRefusedBriefless {
                mol_id: d.molecule_id().into(),
                code: code.unwrap_or(cosmon_core::dispatch_refusal::BRIEFLESS_DISPATCH),
                reason: stderr.trim().to_owned(),
            }
        } else {
            ResidentError::CsInvocation {
                verb: d.verb().into(),
                mol_id: d.molecule_id().into(),
                reason: format!("exit {}: {}", code.unwrap_or(-1), stderr.trim()),
            }
        });
    }
    Ok(())
}

/// Render a resident decision into the transactional CLI arguments.
///
/// Kept separate from process creation so the routing boundary has a pure,
/// exact test: an adapter selected by the scheduler must appear verbatim in
/// the `cs tackle` invocation.
fn shell_out_args(d: &Decision, runtime_pid: u32) -> Vec<String> {
    let mut args = vec![d.verb().to_owned(), d.molecule_id().to_owned()];
    // Anti-preemption lease (task-20260531-a12f): tag the dispatch claim as
    // runtime-owned so `cs tackle` does not default to a sticky `human`
    // lease. `<pid>` is this `cs run` process; `cs done` takes no `--by`.
    if matches!(d, Decision::Tackle { .. }) {
        args.extend(["--by".to_owned(), format!("runtime:{runtime_pid}")]);
    }
    if let Some(adapter) = d.adapter() {
        args.extend(["--adapter".to_owned(), adapter.to_owned()]);
    }
    args
}

fn spawn_watcher(cwd: &Path, tx: mpsc::Sender<()>) -> Result<RecommendedWatcher, ResidentError> {
    let state_dir = cwd.join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).ok();
    let mut fs_watch = notify::recommended_watcher(move |res: notify::Result<Event>| {
        // We don't care which event — any change pings the loop.
        if res.is_ok() {
            let _ = tx.send(());
        }
    })?;
    fs_watch.watch(&state_dir, RecursiveMode::Recursive)?;
    Ok(fs_watch)
}

fn wait_for_event_or_timeout(rx: &mpsc::Receiver<()>, timeout: Duration) {
    // recv_timeout returns Err on timeout — both are valid wakeups.
    let _ = rx.recv_timeout(timeout);
}

fn drain_rx(rx: &mpsc::Receiver<()>) {
    while rx.try_recv().is_ok() {}
}

fn invocation_uuid() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes);
    let mut out = String::with_capacity(32);
    for b in bytes {
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Hash the on-disk state into a short BLAKE3 digest.
///
/// The hash mixes every `state.json` path + size + mtime under
/// `.cosmon/state/fleets/`. It is meant as a *change detector*, not a
/// cryptographic anchor — two different mtimes flip the hash even if
/// the contents are identical (which is exactly what RR-5's
/// `RuntimeReadDecideWrite` audit needs).
fn state_hash(cwd: &Path) -> String {
    let root = cwd.join(".cosmon").join("state").join("fleets");
    let mut entries: Vec<(PathBuf, u64, i128)> = Vec::new();
    collect_state(&root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = blake3::Hasher::new();
    for (path, len, mtime) in entries {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&len.to_le_bytes());
        hasher.update(&mtime.to_le_bytes());
        hasher.update(b"\n");
    }
    let mut hex = hasher.finalize().to_hex().to_string();
    hex.truncate(16);
    format!("blake3:{hex}")
}

/// Compute the launch / pre-dispatch seal `H = BLAKE3(resolved_config)`
/// for config-honoring dispatch.
///
/// The digest mixes two on-disk sources, in order:
///
/// 1. the per-galaxy `.cosmon/config.toml` bytes — the authoritative
///    `[adapters.default]` / engine / model / base-url surface a dispatch
///    resolves against;
/// 2. the global `~/.config/cosmon/config.toml` bytes when present (the
///    operator-wide default tier `cs tackle` layers below the per-galaxy
///    config — honoring `XDG_CONFIG_HOME` then `HOME`).
///
/// **The `cs` binary image is deliberately NOT hashed.**
/// It used to be — `H = BLAKE3(config ⊕
/// binary-image-id)` — on the theory that a redeployed binary might change
/// dispatch behaviour. But that made the runtime *self-poison*: `cs done`'s
/// post-merge hook runs `just install`, which overwrites the very `cs`
/// binary the runtime sealed against. The result was that the propulsion
/// **died every time it successfully drained a DAG** — a merge reinstalls
/// the binary, the next tick recomputes `H' ≠ H`, and the loop halted
/// fail-closed (exit 75) on a phantom "drift" that was actually its own
/// success. A runtime cannot seal against its own binary and still survive
/// the install hook that fires on every merge. So the seal now witnesses
/// *configuration* drift only: a binary reinstall mid-run is the expected
/// steady state, not a reason to halt.
///
/// Like [`state_hash`], this is a **change detector**, not a cryptographic
/// anchor: *any* serialized
/// config drift (engine, model, base-url, adapter, prompt template) flips
/// the digest, so the witness obligation catches the whole class with one
/// boring hash-compare. An absent file is hashed as a stable sentinel so an
/// appearing / disappearing config also flips the seal.
///
/// This function performs only reads (RR-1: no `cosmon_state` /
/// `cosmon_filestore` import — paths are resolved inline with `std`).
fn config_seal(config: &RuntimeLoopConfig) -> String {
    let mut hasher = blake3::Hasher::new();
    let per_galaxy = config.cwd.join(".cosmon").join("config.toml");
    hash_file_contents(&mut hasher, &per_galaxy, b"config:per-galaxy:");
    if let Some(global) = global_config_path() {
        hash_file_contents(&mut hasher, &global, b"config:global:");
    }
    let mut hex = hasher.finalize().to_hex().to_string();
    hex.truncate(16);
    format!("blake3:{hex}")
}

/// Hash a file's contents into `hasher`, prefixed by `label`. An absent or
/// unreadable file contributes a stable `<absent>` sentinel so its
/// appearance / disappearance flips the digest.
fn hash_file_contents(hasher: &mut blake3::Hasher, path: &Path, label: &[u8]) {
    hasher.update(label);
    match std::fs::read(path) {
        Ok(bytes) => {
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        Err(_) => {
            hasher.update(b"<absent>");
        }
    }
    hasher.update(b"\n");
}

/// Resolve `~/.config/cosmon/config.toml`, honoring `XDG_CONFIG_HOME` then
/// `HOME`. Returns `None` when neither env var is set (the seal then omits
/// the global tier — stable across launches in the same environment).
fn global_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("cosmon").join("config.toml"));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| {
            PathBuf::from(h)
                .join(".config")
                .join("cosmon")
                .join("config.toml")
        })
}

fn collect_state(dir: &Path, out: &mut Vec<(PathBuf, u64, i128)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_state(&path, out);
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) != Some("state.json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let len = meta.len();
        let mtime: i128 = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0_i128, |d| {
                // u128 → i128: nanos since epoch always fit (year ~5400+ overflow).
                i128::try_from(d.as_nanos()).unwrap_or(i128::MAX)
            });
        out.push((path, len, mtime));
    }
}

// ---------------------------------------------------------------------------
// TraceWriter — NDJSON to .cosmon/state/runtime-trace.jsonl
// ---------------------------------------------------------------------------

struct TraceWriter {
    path: PathBuf,
    file: Option<File>,
}

/// One NDJSON line about a single decision the loop enacted.
///
/// Used internally to keep [`TraceWriter::write_decision`]'s arity low.
struct DecisionRecord<'a> {
    verb: &'a str,
    molecule_id: &'a str,
    invocation: &'a str,
    basis: &'a str,
    before: &'a str,
    after: &'a str,
    error: Option<&'a str>,
}

impl TraceWriter {
    fn new(path: PathBuf) -> Self {
        Self { path, file: None }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn open(&mut self) -> Result<(), ResidentError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.file = Some(file);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ResidentError> {
        if let Some(f) = self.file.as_mut() {
            f.flush()?;
        }
        Ok(())
    }

    fn write_tick(
        &mut self,
        action: &str,
        basis: &str,
        before: Option<&str>,
        after: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), ResidentError> {
        let line = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "action": action,
            "decision_basis": basis,
            "molecule_id": serde_json::Value::Null,
            "invocation_uuid": serde_json::Value::Null,
            "state_hash_before": before,
            "state_hash_after": after,
            "error": error,
        });
        self.write_line(&line)
    }

    /// Write the config-drift halt line. Carries the
    /// launch / current seals in the `state_hash_before` / `state_hash_after`
    /// slots and embeds the typed [`EventV2::ConfigDriftDetected`] under the
    /// `event` key, so the forensic receipt travels through the same NDJSON
    /// trace stream as every other runtime decision (RR-5).
    fn write_drift(
        &mut self,
        launch_seal: &str,
        current_seal: &str,
        event: &EventV2,
    ) -> Result<(), ResidentError> {
        let line = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "action": "config-drift-halt",
            "decision_basis": "config-seal-mismatch",
            "molecule_id": serde_json::Value::Null,
            "invocation_uuid": serde_json::Value::Null,
            "state_hash_before": launch_seal,
            "state_hash_after": current_seal,
            "error": "config or binary changed since launch — halting fail-closed (relaunch for fresh derivation)",
            "event": event,
        });
        self.write_line(&line)
    }

    fn write_decision(&mut self, rec: &DecisionRecord<'_>) -> Result<(), ResidentError> {
        let line = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "action": rec.verb,
            "decision_basis": rec.basis,
            "molecule_id": rec.molecule_id,
            "invocation_uuid": rec.invocation,
            "state_hash_before": rec.before,
            "state_hash_after": rec.after,
            "error": rec.error,
        });
        self.write_line(&line)
    }

    fn write_line(&mut self, value: &serde_json::Value) -> Result<(), ResidentError> {
        let Some(file) = self.file.as_mut() else {
            return Err(ResidentError::Trace(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "trace file not opened",
            )));
        };
        let mut s = serde_json::to_string(value)
            .map_err(|e| ResidentError::Trace(std::io::Error::other(e.to_string())))?;
        s.push('\n');
        file.write_all(s.as_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Terse `EnsembleMolecule` for tests: no merge / stuck stamp. The
    /// discriminant-bearing tests set `merged_at` / `stuck_at` explicitly on
    /// the returned value.
    fn mol(id: &str, status: &str, blocked_by: &[&str]) -> EnsembleMolecule {
        EnsembleMolecule {
            id: id.into(),
            status: status.into(),
            kind: None,
            tags: Vec::new(),
            blocked_by: blocked_by.iter().map(|s| (*s).into()).collect(),
            merged_at: None,
            stuck_at: None,
            adapter: None,
        }
    }

    #[test]
    fn snapshot_parses_minimal_ensemble_json() {
        let json = r#"{"molecules":[{"id":"a","status":"pending","blocked_by":[]},
                                     {"id":"b","status":"completed","blocked_by":["a"]}]}"#;
        let snap = EnsembleSnapshot::from_json(json).unwrap();
        assert_eq!(snap.molecules.len(), 2);
        assert_eq!(snap.molecules[0].id, "a");
        assert_eq!(snap.molecules[1].status, "completed");
        assert_eq!(snap.molecules[1].blocked_by, vec!["a".to_string()]);
        // Absent stamps parse as None (the common case).
        assert!(snap.molecules[1].merged_at.is_none());
        assert!(snap.molecules[1].stuck_at.is_none());
    }

    #[test]
    fn snapshot_parses_merged_and_stuck_stamps() {
        // The two discriminants must survive the JSON round-trip: a completed
        // molecule carries `merged_at`, a `cs stuck` frozen one carries
        // `stuck_at`. The scheduler reads only their presence.
        let json = r#"{"molecule_states":[
            {"id":"a","status":"completed","blocked_by":[],"merged_at":"2026-07-12T10:00:00Z"},
            {"id":"b","status":"frozen","blocked_by":[],"stuck_at":"2026-07-12T11:00:00Z"}
        ]}"#;
        let snap = EnsembleSnapshot::from_json(json).unwrap();
        let a = snap.molecules.iter().find(|m| m.id == "a").unwrap();
        let b = snap.molecules.iter().find(|m| m.id == "b").unwrap();
        assert!(
            a.merged_at.is_some(),
            "completed molecule carries merged_at"
        );
        assert!(a.stuck_at.is_none());
        assert!(
            b.stuck_at.is_some(),
            "cs-stuck frozen molecule carries stuck_at"
        );
        assert!(b.merged_at.is_none());
    }

    #[test]
    fn resident_dispatch_preserves_a_codex_adapter_pin() {
        let snapshot = EnsembleSnapshot::from_json(
            r#"{"molecule_states":[{
                "id":"task-codex",
                "status":"pending",
                "blocked_by":[],
                "adapter":"codex"
            }]}"#,
        )
        .expect("snapshot with routing selection parses");
        let mut scheduler = ReadyFrontierScheduler::new();

        let decisions = scheduler.next_decisions(&snapshot);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "task-codex".into(),
                adapter: Some("codex".into()),
            }]
        );
        assert_eq!(
            shell_out_args(&decisions[0], 42),
            vec![
                "tackle",
                "task-codex",
                "--by",
                "runtime:42",
                "--adapter",
                "codex",
            ]
        );
    }

    #[test]
    fn run_adapter_directive_overrides_local_floor_for_pinless() {
        // DELIVERABLE 1 (F3+F4): an explicit, opt-in `cs run --adapter claude`
        // directive replaces the `local` floor for a PIN-LESS molecule so a
        // resident run can drive cognitive nodes on a paid adapter the operator
        // consciously chose. Without the directive the floor stays local
        // (anti-silent-spend guard preserved).
        let snapshot = EnsembleSnapshot {
            molecules: vec![mol("task-pinless", "pending", &[])],
        };
        let mut scheduler =
            ReadyFrontierScheduler::new().with_run_adapter(Some("claude".to_owned()));
        let decisions = scheduler.next_decisions(&snapshot);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "task-pinless".into(),
                adapter: Some("claude".into()),
            }],
            "an explicit run directive must replace the local floor for a pin-less molecule"
        );
    }

    #[test]
    fn run_adapter_directive_absent_emits_no_adapter_flag() {
        // COSMON-DEV #21 (G1 contract §3.1): with no pin and no run directive
        // the scheduler owns no flag-rung intent, so it MUST emit `None` — no
        // `--adapter` argument on the shelled `cs tackle`. The floor is not
        // stamped here; the child runs the full canonical chain
        // (`$COSMON_DEFAULT_ADAPTER` → config → the `local` floor) itself. This
        // is what preserves the operator's live env/config intent instead of
        // masking it with a rung-1 `--adapter local`.
        let snapshot = EnsembleSnapshot {
            molecules: vec![mol("task-pinless", "pending", &[])],
        };
        let mut scheduler = ReadyFrontierScheduler::new().with_run_adapter(None);
        let decisions = scheduler.next_decisions(&snapshot);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "task-pinless".into(),
                adapter: None,
            }],
            "no pin + no run directive must emit no adapter flag (delegate the full chain to cs tackle)"
        );
        // Concretely: no `--adapter` token reaches the child argv.
        assert!(
            !shell_out_args(&decisions[0], 7).contains(&"--adapter".to_owned()),
            "a pin-less, directive-less dispatch must not render --adapter"
        );
    }

    #[test]
    fn molecule_pin_beats_run_adapter_directive() {
        // Resolution precedence: a per-molecule pin (`m.adapter`) is a stronger,
        // more specific intent than the run-wide directive, so it wins. The
        // directive only ever replaces the *floor*, never a real pin.
        let snapshot = EnsembleSnapshot::from_json(
            r#"{"molecule_states":[{"id":"task-codex","status":"pending","adapter":"codex"}]}"#,
        )
        .unwrap();
        let mut scheduler =
            ReadyFrontierScheduler::new().with_run_adapter(Some("claude".to_owned()));
        let decisions = scheduler.next_decisions(&snapshot);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "task-codex".into(),
                adapter: Some("codex".into()),
            }],
            "a per-molecule pin must beat the run-wide directive"
        );
    }

    #[test]
    fn run_adapter_directive_applies_to_dynamically_nucleated_child() {
        // F4: the floor also hit children nucleated dynamically mid-run by
        // workers (the `converge` loop), making self-drive impossible without
        // per-node pins. Because the directive lives on the scheduler and the
        // frontier is re-derived from the fresh ensemble each tick, a pin-less
        // molecule that only *appears* on a later tick inherits the directive
        // too — no static pin required.
        let mut scheduler =
            ReadyFrontierScheduler::new().with_run_adapter(Some("claude".to_owned()));
        // Tick 1: only the parent is present.
        let tick1 = EnsembleSnapshot {
            molecules: vec![mol("parent", "pending", &[])],
        };
        let _ = scheduler.next_decisions(&tick1);
        // Tick 2: a worker nucleated a pin-less child (blocker torn down).
        let tick2 = EnsembleSnapshot {
            molecules: vec![mol("child", "pending", &[])],
        };
        let decisions = scheduler.next_decisions(&tick2);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "child".into(),
                adapter: Some("claude".into()),
            }],
            "a dynamically-nucleated pin-less child must inherit the run directive"
        );
    }

    #[test]
    fn snapshot_parses_kind_and_tags_for_scheduler_policy() {
        let json = r#"{"molecule_states":[{
            "id":"decision","status":"pending","blocked_by":[],
            "kind":"decision","tags":["hold:human","needs-review"]
        }]}"#;
        let snap = EnsembleSnapshot::from_json(json).unwrap();
        let molecule = &snap.molecules[0];
        assert_eq!(molecule.kind.as_deref(), Some("decision"));
        assert_eq!(molecule.tags, ["hold:human", "needs-review"]);
    }

    #[test]
    fn ready_frontier_dispatches_unblocked_pending_once() {
        let snap = EnsembleSnapshot {
            molecules: vec![mol("a", "pending", &[]), mol("b", "pending", &["a"])],
        };
        let mut sched = ReadyFrontierScheduler::new();
        let d1 = sched.next_decisions(&snap);
        assert_eq!(
            d1,
            vec![Decision::Tackle {
                molecule_id: "a".into(),
                // No pin, no run directive → no flag stamp; the child resolves
                // the full canonical chain (COSMON-DEV #21).
                adapter: None,
            }]
        );
        // Same snapshot → no re-tackle.
        let d2 = sched.next_decisions(&snap);
        assert!(d2.is_empty(), "scheduler must not re-tackle: {d2:?}");
    }

    #[test]
    fn ready_frontier_never_dispatches_human_hold() {
        let mut held = mol("reserved", "pending", &[]);
        held.tags.push("hold:human".into());
        let snap = EnsembleSnapshot {
            molecules: vec![held],
        };

        let decisions = ReadyFrontierScheduler::new().next_decisions(&snap);
        assert!(
            decisions.is_empty(),
            "human hold must not be raffled: {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_dispatches_explicitly_opted_in_decision() {
        let mut decision = mol("decision", "pending", &[]);
        decision.kind = Some("decision".into());
        decision.tags.push("auto:ok".into());
        let snap = EnsembleSnapshot {
            molecules: vec![decision],
        };

        let decisions = ReadyFrontierScheduler::new().next_decisions(&snap);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "decision".into(),
                // Pin-less, directive-less → no flag; child runs the full chain.
                adapter: None,
            }]
        );
    }

    #[test]
    fn ready_frontier_holds_review_required_completed_molecule() {
        let mut review_required = mol("security-fix", "completed", &[]);
        review_required.tags.push("needs-review".into());
        let snap = EnsembleSnapshot {
            molecules: vec![review_required],
        };

        let decisions = ReadyFrontierScheduler::new().next_decisions(&snap);
        assert!(
            !decisions.contains(&Decision::Done("security-fix".into())),
            "a completed review-required molecule without an on-disk verdict must not auto-merge: {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_never_auto_merges_reserved_decision() {
        let mut decision = mol("reserved-decision", "completed", &[]);
        decision.kind = Some("decision".into());
        let decisions = ReadyFrontierScheduler::new().next_decisions(&EnsembleSnapshot {
            molecules: vec![decision],
        });
        assert!(
            !decisions.contains(&Decision::Done("reserved-decision".into())),
            "the runtime must not auto-merge a human-reserved decision: {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_never_auto_harvests_no_auto_harvest() {
        // The explicit harvest brake: a completed molecule the operator has
        // flagged `no-auto-harvest` reserves its merge as an operator gesture.
        // The runtime must not emit `Done` for it.
        let mut parked = mol("parked", "completed", &[]);
        parked.tags.push("no-auto-harvest".into());
        let decisions = ReadyFrontierScheduler::new().next_decisions(&EnsembleSnapshot {
            molecules: vec![parked],
        });
        assert!(
            !decisions.contains(&Decision::Done("parked".into())),
            "a `no-auto-harvest` completed molecule must not be auto-merged: {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_never_auto_harvests_harvest_to_branch() {
        // The falsifier this molecule closes: a runtime harvest event merging
        // to main for a molecule whose `harvest_to` points elsewhere. The
        // resident loop can only merge to the trunk, so any `harvest_to:` tag
        // reserves the harvest for the operator gesture that can route it.
        let mut routed = mol("math-attack", "completed", &[]);
        routed.tags.push("harvest_to:spore/math-attack".into());
        let decisions = ReadyFrontierScheduler::new().next_decisions(&EnsembleSnapshot {
            molecules: vec![routed],
        });
        assert!(
            !decisions.contains(&Decision::Done("math-attack".into())),
            "a `harvest_to:<branch>` molecule must never be auto-merged to the trunk: {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_holds_dependents_of_harvest_reserved_blocker() {
        // A parked completed blocker never merges (`merged_at` stays None and
        // the runtime issues no `Done`), so it stays present and un-cleared —
        // its dependents must remain blocked, mirroring the operator's park of
        // the whole line rather than draining past it.
        let mut parked = mol("root", "completed", &[]);
        parked.tags.push("no-auto-harvest".into());
        let snap = EnsembleSnapshot {
            molecules: vec![parked, mol("child", "pending", &["root"])],
        };
        let decisions = ReadyFrontierScheduler::new().next_decisions(&snap);
        assert!(
            decisions.is_empty(),
            "a harvest-reserved blocker must neither merge nor release its \
             dependents, got {decisions:?}"
        );
    }

    #[test]
    fn snapshot_adapter_pin_routes_without_local_fallback() {
        let snapshot = EnsembleSnapshot::from_json(
            r#"{"molecule_states":[{"id":"routed","status":"pending","adapter":"anthropic"}]}"#,
        )
        .unwrap();
        let decisions = ReadyFrontierScheduler::new().next_decisions(&snapshot);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "routed".into(),
                adapter: Some("anthropic".into()),
            }],
            "a snapshot adapter pin must reach the tackle decision unchanged"
        );
    }

    #[test]
    fn ready_frontier_done_then_tackle_dependent() {
        // A completed blocker that is ALREADY merged (its branch landed on
        // `main`, teardown pending) clears its dependent under
        // merge-before-dispatch: the same-tick `Done` is the idempotent
        // teardown a restarted scheduler re-issues (self.merged rebuilt empty),
        // and the dependent tackles because `merged_at` is set. Models the
        // realistic post-restart snapshot; the un-merged case is covered by
        // `ready_frontier_holds_dependent_of_unmerged_completed_blocker`.
        let mut sched = ReadyFrontierScheduler::new();
        let mut a = mol("a", "completed", &[]);
        a.merged_at = Some("2026-07-12T10:00:00Z".into());
        let snap = EnsembleSnapshot {
            molecules: vec![a, mol("b", "pending", &["a"])],
        };
        let decisions = sched.next_decisions(&snap);
        assert_eq!(
            decisions,
            vec![
                Decision::Done("a".into()),
                Decision::Tackle {
                    molecule_id: "b".into(),
                    // Pin-less, directive-less → no flag; child runs the chain.
                    adapter: None,
                },
            ]
        );
    }

    #[test]
    fn ready_frontier_holds_dependent_of_unmerged_completed_blocker() {
        // Merge-before-dispatch (frontier.rs:214, mirrored): a blocker that is
        // `completed` but whose branch has NOT merged (`merged_at == None`)
        // must NOT release its dependent — the dependent's worktree would miss
        // the committed output. The loop still issues `Done(a)` (which merges
        // and tears a down); the dependent then chains on the next tick via
        // the *absent* path, immediately because teardown fires an FS event.
        let mut sched = ReadyFrontierScheduler::new();
        let snap = EnsembleSnapshot {
            molecules: vec![mol("a", "completed", &[]), mol("b", "pending", &["a"])],
        };
        let decisions = sched.next_decisions(&snap);
        assert_eq!(
            decisions,
            vec![Decision::Done("a".into())],
            "un-merged completed blocker must be Done'd but NOT release its \
             dependent same-tick, got {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_chains_past_torn_down_blocker() {
        // BUG 2 (task-20260604-6056): `cs run` auto-`cs done`s each stage, so
        // a fan-in node's earlier blockers vanish from the ensemble before
        // the last one completes. The scheduler must treat an *absent*
        // blocker as satisfied, or it drains with the fan-in still pending.
        let mut sched = ReadyFrontierScheduler::new();
        // b1 already torn down (absent). b2 completed AND merged (its branch
        // landed, teardown pending) — the merge-before-dispatch discriminant
        // is satisfied, so the fan-in may chain.
        let mut b2 = mol("b2", "completed", &[]);
        b2.merged_at = Some("2026-07-12T10:00:00Z".into());
        let snap = EnsembleSnapshot {
            molecules: vec![
                b2,
                // fan-in blocked by b1 (gone) and b2 (completed+merged).
                mol("redteam", "pending", &["b1", "b2"]),
            ],
        };
        let decisions = sched.next_decisions(&snap);
        assert!(
            decisions.contains(&Decision::Tackle {
                molecule_id: "redteam".into(),
                // Pin-less, directive-less → no flag; child runs the chain.
                adapter: None,
            }),
            "fan-in must chain when one blocker is torn down and the other \
             completed, got {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_chains_past_frozen_blocker() {
        // BUG 1 (task-20260604-6056): a *delivered* frozen mission
        // (`stuck_at == None`, the `freeze_on_last_step` species) must release
        // its children even though it is neither "completed" nor torn down.
        let mut sched = ReadyFrontierScheduler::new();
        let snap = EnsembleSnapshot {
            // Default `mol` leaves `stuck_at = None` → delivered freeze.
            molecules: vec![
                mol("mission", "frozen", &[]),
                mol("architect", "pending", &["mission"]),
            ],
        };
        let decisions = sched.next_decisions(&snap);
        assert_eq!(
            decisions,
            vec![Decision::Tackle {
                molecule_id: "architect".into(),
                // Pin-less, directive-less → no flag; child runs the chain.
                adapter: None,
            }],
            "child must tackle behind a delivered (stuck_at=None) frozen \
             mission, got {decisions:?}"
        );
    }

    #[test]
    fn ready_frontier_holds_dependent_of_stuck_frozen_blocker() {
        // F-C11-1 — the convoy-cascade regression this molecule fixes. A
        // blocker frozen via `cs stuck` ("[IDÉE — NE PAS EXÉCUTER TEL QUEL]")
        // carries `stuck_at = Some(_)` and has NOT delivered its work. The
        // resident scheduler used a coarse status-string match that read it as
        // delivered and flung its successors at the fleet under `--resident` —
        // the exact class `cosmon_state::frontier` (frontier.rs:210) closed in
        // the DEFAULT `cs run` path. A blocker that says "do not execute" must
        // HOLD its dependents. The discriminant is `stuck_at`.
        let mut sched = ReadyFrontierScheduler::new();
        let mut blocker = mol("task-20260710-6174", "frozen", &[]);
        blocker.stuck_at = Some("2026-07-12T10:00:00Z".into());
        let snap = EnsembleSnapshot {
            molecules: vec![
                blocker,
                mol("task-20260710-5f33", "pending", &["task-20260710-6174"]),
            ],
        };
        let decisions = sched.next_decisions(&snap);
        assert!(
            decisions.is_empty(),
            "successor of a `cs stuck` (stuck_at=Some) blocker must NOT \
             dispatch under --resident, got {decisions:?}"
        );
    }

    #[test]
    fn invocation_uuid_is_32_hex_chars() {
        let u = invocation_uuid();
        assert_eq!(u.len(), 32);
        assert!(u.chars().all(|c| c.is_ascii_hexdigit()));
    }

    fn seal_config(cwd: &Path, cs_binary: &Path) -> String {
        let mut config = RuntimeLoopConfig::new(cwd);
        config.cs_binary = cs_binary.to_path_buf();
        config_seal(&config)
    }

    #[test]
    fn recheck_refuses_human_hold_added_after_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let cs = tmp.path().join("fake-cs");
        std::fs::write(
            &cs,
            "#!/bin/sh\nprintf '%s\\n' '{\"status\":\"pending\",\"kind\":\"task\",\"tags\":[\"hold:human\"]}'\n",
        )
        .unwrap();
        let status = Command::new("chmod")
            .args(["+x"])
            .arg(&cs)
            .status()
            .unwrap();
        assert!(status.success());
        let mut config = RuntimeLoopConfig::new(tmp.path());
        config.cs_binary = cs;

        assert_eq!(
            recheck_tackle_candidate(&config, "task-held"),
            TackleRecheck::SkipHumanReservation,
            "a hold written after the scheduler snapshot must veto dispatch"
        );
    }

    #[test]
    fn config_seal_is_stable_for_unchanged_inputs() {
        // delib-20260531-c761: the witness obligation only fires on *drift*;
        // two reads of identical config must produce the same seal, or the
        // runtime would halt on every tick (false positive).
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cosmon = cwd.join(".cosmon");
        std::fs::create_dir_all(&cosmon).unwrap();
        std::fs::write(
            cosmon.join("config.toml"),
            b"[adapters]\ndefault = \"local\"\n",
        )
        .unwrap();
        let bin = cwd.join("cs");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        let h1 = seal_config(cwd, &bin);
        let h2 = seal_config(cwd, &bin);
        assert_eq!(h1, h2, "seal must be stable for unchanged inputs");
        assert!(h1.starts_with("blake3:"));
    }

    #[test]
    fn config_seal_flips_when_config_changes() {
        // The load-bearing case: an operator (or a deploy) edits
        // `[adapters.default]` / base-url / model between launch and
        // dispatch. The seal must change so the pre-dispatch re-check halts.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cosmon = cwd.join(".cosmon");
        std::fs::create_dir_all(&cosmon).unwrap();
        let cfg = cosmon.join("config.toml");
        let bin = cwd.join("cs");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        std::fs::write(&cfg, b"[adapters]\ndefault = \"local\"\n").unwrap();
        let launch = seal_config(cwd, &bin);
        // The wrong-oracle drift: swap the default adapter to openai.
        std::fs::write(&cfg, b"[adapters]\ndefault = \"openai\"\n").unwrap();
        let after = seal_config(cwd, &bin);
        assert_ne!(launch, after, "config change must flip the seal");
    }

    #[test]
    fn config_seal_unaffected_by_binary_reinstall() {
        // task-20260608-1c59 — the regression test for the self-poisoning
        // bug. `cs done`'s post-merge hook runs `just install`, which
        // overwrites the `cs` binary (new bytes, bumped mtime) on EVERY
        // successful drain. The old seal hashed that binary image, so the
        // very next tick saw `H' ≠ H` and halted fail-closed (exit 75) —
        // the propulsion died of its own success. With the binary term
        // dropped, a reinstall mid-run must NOT flip the seal.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cosmon = cwd.join(".cosmon");
        std::fs::create_dir_all(&cosmon).unwrap();
        std::fs::write(
            cosmon.join("config.toml"),
            b"[adapters]\ndefault = \"local\"\n",
        )
        .unwrap();
        let bin = cwd.join("cs");
        std::fs::write(&bin, b"OLD-BINARY-IMAGE").unwrap();

        let launch = seal_config(cwd, &bin);
        // Simulate `just install`: same path, new bytes, bumped mtime.
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(&bin, b"NEW-BINARY-IMAGE-AFTER-just-install-on-merge").unwrap();
        let after = seal_config(cwd, &bin);
        assert_eq!(
            launch, after,
            "a binary reinstall (the merge install hook) must NOT trip the seal",
        );
    }

    #[test]
    fn state_hash_changes_with_file_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mol_dir = root
            .join(".cosmon")
            .join("state")
            .join("fleets")
            .join("default")
            .join("molecules")
            .join("task-a");
        std::fs::create_dir_all(&mol_dir).unwrap();
        std::fs::write(mol_dir.join("state.json"), b"{}").unwrap();
        let h1 = state_hash(root);
        // Re-write with a small delay to bump mtime.
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(mol_dir.join("state.json"), b"{\"x\":1}").unwrap();
        let h2 = state_hash(root);
        assert_ne!(h1, h2);
        assert!(h2.starts_with("blake3:"));
    }
}
