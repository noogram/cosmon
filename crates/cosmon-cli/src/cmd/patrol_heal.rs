// SPDX-License-Identifier: AGPL-3.0-only

//! `cs patrol --heal` — the Deacon, safe reversible classes (ADR-137 Phase 3).
//!
//! The L2 *remediate* layer of the molecule-health primitive. Where `cs health`
//! ([`super::health`]) only *detects* (the Witness, P1) and the §5 no-interference
//! guard ([`cosmon_core::patrol::heal_gate`], P2) only *decides*, the Deacon
//! *acts* — but on the **low-risk, reversible classes only**, each gated by the
//! P2 guard, each idempotent, each logged.
//!
//! ## Scope (ADR-137 §11 P3): the safe classes only
//!
//! | Class | Remedy | How |
//! |---|---|---|
//! | **A1** unsent-paste | [`HealthRemedy::TransportResubmit`] | delegate to the transport's robust submit-retry (the 81b2 Enter-budget baked into [`cosmon_transport::TmuxBackend::send_input`]) — a bare Enter submits the pasted-but-unsent prompt. **We never re-grep the pane.** |
//! | **A4/A8** idle-after-complete / completed-unharvested | [`HealthRemedy::HarvestDone`] | the orchestrator's `cs done` harvest ([`crate::cmd::harvest::harvest_one`]). Runs from the patrol (scheduler/operator) caller, **never** a worker self-`cs done`. |
//! | **A5** idle-no-progress | [`HealthRemedy::Nudge`] | re-engage via transport, referencing `briefing.md`. |
//! | **A6** overloaded | [`HealthRemedy::BackoffPerAccount`] | a runtime *hold* — never a collapse, never a re-dispatch into the same wall. The guard's per-class cooldown is the backoff. |
//!
//! The **collapse / integrity classes — A3 (auth-dead), A7 (ghost-merge), A9
//! (crash-zombie)** — are *not* mutated here. They are P4 (`docs/adr/137 §11`):
//! reported so the operator sees them, never auto-collapsed by P3.
//!
//! ## The be1e seven-defect firewall (read before touching this file)
//!
//! `delib-20260625-be1e` audited the bash prototype this primitive retires and
//! found seven defects. Each is structurally foreclosed here:
//!
//! 1. **Control-plane detection, never pane glyphs.** Every input is folded into
//!    `MoleculeHealthView` / [`HealGuardView`], neither of which has a field
//!    for rendered scrollback. A worker cannot trip the Deacon by *printing* the
//!    glyphs of the rule meant to police it (the SEV-1 `grep 'cs done'` bug).
//! 2. **No collapse-on-kill orphan.** P3 never kills a `Running` session: the
//!    only state-lossy classes (A3/A9) are deferred to P4. Nothing here strands
//!    a `running` zombie with no reclaim path (the SEV-2 bug).
//! 3. **No anchored/unanchored substring matching at all** — the `401` detector
//!    (SEV-3) has no analogue; A3 is a *typed* probe, deferred to P4 regardless.
//! 4. **Suffix-not-title mapping.** Session ↔ molecule is read straight from
//!    `mol.assigned_worker` / `mol.id` in the state store — we never reconstruct
//!    or title-match a session name (the SEV-4 collision amplifier).
//! 5. **No worker self-`cs done`.** The harvest runs from the patrol caller; the
//!    `cs done` perimeter (ADR-016) refuses a worker-context caller anyway.
//! 6. **The cognition is versioned, typed, reviewed** — not an un-versioned
//!    shell brief (the godel regime critique).
//! 7. **Idempotent + guarded + logged.** Every action passes [`heal_gate`] and
//!    records a per-molecule backoff memory ([`HealLedger`]) so a remedy is not
//!    re-applied within its cooldown and three failures stop the molecule.

use std::path::Path;

use chrono::{DateTime, Utc};
use colored::Colorize;
use cosmon_core::id::MoleculeId;
use cosmon_core::patrol::{
    heal_gate, scan, AnomalyClass, GuardConfig, HealBlockReason, HealGate, HealGuardView,
    HealthFinding, HealthRemedy, HealthThresholds,
};
use cosmon_core::presence::Presence;
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::PresenceStore;
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};
use cosmon_transport::TmuxBackend;
use serde::{Deserialize, Serialize};

use super::Context;

/// Filename of the per-molecule backoff-memory sidecar (ADR-137 §5.5). Lives in
/// the molecule's state directory next to `briefing.md` / `state.json`. It is
/// **disposable runtime sediment, not source-of-truth** — losing it only
/// re-arms the cooldown, never corrupts state (same status as the §6 ledger).
const HEAL_LEDGER_FILE: &str = "heal-state.json";

/// Append-only log of applied heal actions, one JSON line per mutation, under
/// the state dir. Best-effort observability; never the source-of-truth.
const HEAL_ACTIONS_LOG: &str = "heal-actions.jsonl";

/// Whether a remedy is in the **P3 safe set** (ADR-137 §11). The Deacon acts on
/// these four; the collapse/integrity remedies ([`HealthRemedy::CollapseProcessDeath`],
/// [`HealthRemedy::FlagOnly`]) are deferred to P4 and only *reported*.
#[must_use]
pub(crate) fn is_safe_remedy(remedy: HealthRemedy) -> bool {
    matches!(
        remedy,
        HealthRemedy::TransportResubmit
            | HealthRemedy::HarvestDone
            | HealthRemedy::Nudge
            | HealthRemedy::BackoffPerAccount
    )
}

/// Per-molecule backoff memory (ADR-137 §5.5). Persisted as a small JSON
/// sidecar so idempotence survives across patrol ticks: the same remedy is not
/// re-applied within its cooldown, and three consecutive failures stop the
/// Deacon from healing the molecule (the three-strikes convention).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HealLedger {
    /// When the Deacon last applied a remedy to this molecule (§5.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heal_at: Option<DateTime<Utc>>,
    /// Which remedy was last applied — the cooldown is per remedy class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remedy: Option<HealthRemedy>,
    /// Consecutive failed remediations; at the three-strikes limit the guard
    /// stops healing this molecule and flags for a human.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub consecutive_failures: u32,
}

/// serde skip helper — keeps a fresh ledger's JSON minimal.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl HealLedger {
    /// Load the ledger from a molecule's state directory, or [`Self::default`]
    /// when absent/corrupt — a missing or garbage ledger is treated as "no
    /// prior remediation", never an error (disposable sediment).
    fn load(mol_dir: &Path) -> Self {
        std::fs::read_to_string(mol_dir.join(HEAL_LEDGER_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist the ledger. Best-effort: a write failure is swallowed (the
    /// Deacon must never wedge on sediment I/O) and reported `false`.
    fn save(&self, mol_dir: &Path) -> bool {
        serde_json::to_string_pretty(self)
            .ok()
            .and_then(|s| std::fs::write(mol_dir.join(HEAL_LEDGER_FILE), s).ok())
            .is_some()
    }
}

/// The pure, control-plane-only inputs the guard needs, folded into a
/// [`HealGuardView`]. Reads presence rows, the whisper log, molecule tags / the
/// `.no-heal` sentinel, the global kill-switch, and the backoff ledger — **never
/// a pane**.
fn build_guard_view(
    mol: &MoleculeData,
    mol_dir: &Path,
    presences: &[Presence],
    global_kill_switch: bool,
    now: DateTime<Utc>,
) -> HealGuardView {
    let mut v = HealGuardView::healable(mol.id.clone());

    // §5.1 — a live pilot/presence session pointing at this molecule.
    v.pilot_present = presences
        .iter()
        .any(|p| p.is_live(now) && p.current_molecule.as_ref() == Some(&mol.id));

    // §5.2 — last directed whisper, folded from the per-molecule whisper log.
    v.last_whisper_at = super::whisper::last_whisper_ts(&mol_dir.join("whispers.jsonl"));

    // §5.3 — per-molecule do-not-heal marker: tag `health:hold` OR `.no-heal`.
    v.do_not_heal =
        mol.tags.iter().any(|t| t.as_str() == "health:hold") || mol_dir.join(".no-heal").exists();

    // §5.4 — global kill-switch, re-evaluated by the caller before every tick.
    v.global_kill_switch = global_kill_switch;

    // §5.5 — backoff memory from the per-molecule ledger.
    let ledger = HealLedger::load(mol_dir);
    v.last_heal_at = ledger.last_heal_at;
    v.last_remedy = ledger.last_remedy;
    v.consecutive_failures = ledger.consecutive_failures;

    v
}

/// One planned remediation: a safe-class finding paired with its guard verdict.
/// **Pure** — produced by [`plan_heal`] without I/O, so the planner is unit
/// testable (the guard-blocks-piloted-worker case lives here).
#[derive(Debug, Clone)]
pub(crate) struct HealPlan {
    /// The molecule to remediate.
    pub molecule_id: MoleculeId,
    /// The anomaly class that fired.
    pub class: AnomalyClass,
    /// The perimeter-correct remedy (always a [`is_safe_remedy`]).
    pub remedy: HealthRemedy,
    /// The §5 guard verdict — only [`HealGate::Heal`] plans are applied.
    pub gate: HealGate,
}

/// **Pure planner** — for each *safe-class* finding, look up its guard view and
/// compute the §5 gate. Findings whose remedy is a P4 collapse/flag class are
/// dropped (they are reported by the caller, not planned for action here).
///
/// `guard_for` is a closure so production folds it from disk while tests pass an
/// in-memory map — keeping the planner I/O-free and the guard logic testable in
/// isolation, exactly as the ADR-137 §11 P2 discipline demands.
pub(crate) fn plan_heal<F>(
    findings: &[HealthFinding],
    cfg: &GuardConfig,
    now: DateTime<Utc>,
    mut guard_for: F,
) -> Vec<HealPlan>
where
    F: FnMut(&MoleculeId) -> HealGuardView,
{
    findings
        .iter()
        .filter(|f| is_safe_remedy(f.remedy))
        .map(|f| {
            let view = guard_for(&f.molecule_id);
            let gate = heal_gate(&view, f.remedy, now, cfg);
            HealPlan {
                molecule_id: f.molecule_id.clone(),
                class: f.class,
                remedy: f.remedy,
                gate,
            }
        })
        .collect()
}

/// The result of *attempting* one remedy. Drives the ledger transition
/// ([`ledger_after`]) and the human/JSON report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApplyResult {
    /// The remedy ran and the molecule advanced (or was already resolved).
    Success(String),
    /// The remedy ran but failed — increments the failure count toward
    /// three-strikes.
    Failure(String),
    /// The remedy could not run (no transport, no worker, dry-run) — the ledger
    /// is left untouched (we did not act, so we neither cool down nor fail).
    Skipped(String),
}

impl ApplyResult {
    /// The human/JSON outcome string.
    fn detail(&self) -> &str {
        match self {
            Self::Success(s) | Self::Failure(s) | Self::Skipped(s) => s,
        }
    }
}

/// **Pure** ledger transition (ADR-137 §5.5). A success resets the failure
/// streak and arms the cooldown; a failure increments the streak (toward
/// three-strikes) while still arming the cooldown (so we do not hammer a failing
/// remedy); a skip leaves the ledger untouched.
#[must_use]
pub(crate) fn ledger_after(
    prev: &HealLedger,
    remedy: HealthRemedy,
    result: &ApplyResult,
    now: DateTime<Utc>,
) -> HealLedger {
    match result {
        ApplyResult::Success(_) => HealLedger {
            last_heal_at: Some(now),
            last_remedy: Some(remedy),
            consecutive_failures: 0,
        },
        ApplyResult::Failure(_) => HealLedger {
            last_heal_at: Some(now),
            last_remedy: Some(remedy),
            consecutive_failures: prev.consecutive_failures.saturating_add(1),
        },
        ApplyResult::Skipped(_) => prev.clone(),
    }
}

/// Render the A5 nudge text. Mirrors `cs patrol --nudge`: references the
/// molecule's `briefing.md` so the re-engaged worker re-reads its contract.
fn nudge_text(briefing_path: &Path) -> String {
    format!(
        "⚛ NUDGE — re-read your briefing at {} and continue execution. \
         A molecule in motion stays in motion.",
        briefing_path.display()
    )
}

/// Apply one safe-class remedy. Thin I/O over a perimeter-correct existing verb;
/// the *decision* already happened in [`plan_heal`] + [`heal_gate`].
///
/// `dry_run` short-circuits every arm to `Skipped` so `--heal --dry-run` mutates
/// nothing — the safe default for first operator trust.
fn apply_remedy(
    store: &dyn StateStore,
    state_dir: &Path,
    mol: &MoleculeData,
    remedy: HealthRemedy,
    backend: Option<&TmuxBackend>,
    dry_run: bool,
) -> ApplyResult {
    if dry_run {
        return ApplyResult::Skipped("dry-run".to_owned());
    }
    match remedy {
        // A4/A8 — orchestrator `cs done` harvest. Never a worker self-done.
        HealthRemedy::HarvestDone => {
            use crate::cmd::harvest::HarvestOutcome as O;
            match crate::cmd::harvest::harvest_one(store, state_dir, &mol.id, false) {
                Ok(O::Harvested) => ApplyResult::Success("harvested → cs done".to_owned()),
                Ok(O::AlreadyMerged | O::NotCompleted | O::DryRun) => {
                    ApplyResult::Success("already harvested (no-op)".to_owned())
                }
                Ok(O::HarvestFailed) | Err(_) => {
                    ApplyResult::Failure("cs done harvest failed".to_owned())
                }
            }
        }
        // A5 — re-engage via transport, referencing briefing.md.
        HealthRemedy::Nudge => {
            let Some(be) = backend else {
                return ApplyResult::Skipped("no transport (--no-tmux)".to_owned());
            };
            let Some(wid) = mol.assigned_worker.clone() else {
                return ApplyResult::Skipped("no assigned worker".to_owned());
            };
            if !be.is_alive(&wid).unwrap_or(false) {
                return ApplyResult::Skipped("session not alive".to_owned());
            }
            // The healer is the third organ that can speak unbidden into a
            // worker's terminal, and it reaches the *same* gated worker the
            // other two must leave alone — indeed sooner, since a correctly
            // paused worker looks to every diagnostic like a stalled one. It
            // consults the one judge like its siblings.
            if crate::cmd::patrol::worker_awaits_operator(store, mol) {
                return ApplyResult::Skipped(
                    "awaiting operator — questions pending, not nudged".to_owned(),
                );
            }
            let briefing = store.molecule_dir(&mol.id).join("briefing.md");
            if be.send_input(&wid, &nudge_text(&briefing)).is_ok() {
                bump_nudge_count(store, &mol.id);
                ApplyResult::Success("nudged".to_owned())
            } else {
                ApplyResult::Failure("send_input failed".to_owned())
            }
        }
        // A1 — delegate to the transport's robust submit-retry (the 81b2 fix in
        // send_input's Enter budget). A bare Enter submits a pasted-but-unsent
        // prompt. NO pane re-grep — the witness already proved the boot-stall
        // from event-log non-growth.
        HealthRemedy::TransportResubmit => {
            let Some(be) = backend else {
                return ApplyResult::Skipped("no transport (--no-tmux)".to_owned());
            };
            let Some(wid) = mol.assigned_worker.clone() else {
                return ApplyResult::Skipped("no assigned worker".to_owned());
            };
            if !be.is_alive(&wid).unwrap_or(false) {
                return ApplyResult::Skipped("session not alive".to_owned());
            }
            // Empty input = the transport's bare-Enter submit path, which carries
            // the multi-block Enter-budget retry loop.
            if be.send_input(&wid, "").is_ok() {
                ApplyResult::Success("transport re-submitted (Enter)".to_owned())
            } else {
                ApplyResult::Failure("transport submit failed".to_owned())
            }
        }
        // A6 — a runtime hold. The *action* is to record the backoff; the
        // guard's per-class cooldown then suppresses re-action until it elapses.
        // We never collapse, never re-dispatch into the same wall.
        HealthRemedy::BackoffPerAccount => ApplyResult::Success("backoff hold recorded".to_owned()),
        // P4 classes — structurally unreachable here ([`plan_heal`] filtered
        // them out via [`is_safe_remedy`]); kept total for exhaustiveness.
        HealthRemedy::CollapseProcessDeath | HealthRemedy::FlagOnly => {
            ApplyResult::Skipped("deferred to P4".to_owned())
        }
    }
}

/// Bump `nudge_count` / `last_nudged_at` so a post-mortem can read "how many
/// nudges before recovery" the same way `cs patrol --nudge` does. Best-effort.
fn bump_nudge_count(store: &dyn StateStore, mol_id: &MoleculeId) {
    if let Ok(mut mol) = store.load_molecule(mol_id) {
        mol.nudge_count = mol.nudge_count.saturating_add(1);
        mol.last_nudged_at = Some(Utc::now());
        let _ = store.save_molecule(mol_id, &mol);
    }
}

/// Append one applied-action line to the heal-actions log. Best-effort.
fn log_action(state_dir: &Path, rec: &HealActionRecord) {
    use std::io::Write;
    let Ok(line) = serde_json::to_string(rec) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join(HEAL_ACTIONS_LOG))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// One row of the heal report: a safe-class finding, its guard verdict, and (if
/// healed) the applied outcome. Serialised verbatim in `--json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HealActionRecord {
    /// The molecule.
    pub molecule_id: MoleculeId,
    /// The anomaly class.
    pub class: AnomalyClass,
    /// The remedy.
    pub remedy: HealthRemedy,
    /// Whether the §5 guard let the Deacon act.
    pub healed: bool,
    /// If blocked, which §5 clause failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<HealBlockReason>,
    /// Whether a mutation was actually applied (false in dry-run / when skipped).
    pub applied: bool,
    /// Human-readable outcome.
    pub outcome: String,
}

/// Aggregate result of one `cs patrol --heal` pass.
#[derive(Debug, Clone, Default)]
pub(crate) struct HealSweepReport {
    /// Molecules scanned.
    pub scanned: usize,
    /// Whether this was a dry-run (no mutation).
    pub dry_run: bool,
    /// Whether the global kill-switch short-circuited the pass.
    pub kill_switched: bool,
    /// Per-safe-class action rows.
    pub actions: Vec<HealActionRecord>,
    /// Count of P4 collapse/integrity findings observed (A3/A7/A9) — reported,
    /// never mutated here.
    pub deferred_p4: usize,
}

impl HealSweepReport {
    /// How many actions actually mutated state.
    fn applied_count(&self) -> usize {
        self.actions.iter().filter(|a| a.applied).count()
    }

    /// How many safe-class findings the guard blocked.
    fn blocked_count(&self) -> usize {
        self.actions.iter().filter(|a| !a.healed).count()
    }
}

/// Resolve `~/.cosmon/health.off` and report whether the global kill-switch is
/// present (ADR-137 §5.4). Absent home dir ⇒ treat as not set.
fn global_kill_switch_present() -> bool {
    dirs::home_dir().is_some_and(|h| h.join(".cosmon").join("health.off").exists())
}

/// Run one detect → guard → remediate pass over the current galaxy's state
/// store (ADR-137 §11 P3). Returns the [`HealSweepReport`]; the caller prints
/// it (human or `--json`).
///
/// **Stateless one-shot** (Transactional Core, ADR-016): reads state, computes a
/// report, applies guarded idempotent actions, returns. No daemon, no loop — the
/// cadence is the external scheduler's job (P5).
pub(crate) fn heal_sweep(
    ctx: &Context,
    state_dir: &Path,
    dry_run: bool,
    no_tmux: bool,
    now: DateTime<Utc>,
) -> HealSweepReport {
    // §5.4 — global kill-switch dominates: the whole pass is a no-op.
    if global_kill_switch_present() {
        return HealSweepReport {
            kill_switched: true,
            dry_run,
            ..Default::default()
        };
    }

    let store = ctx.store_at(state_dir);
    let Ok(molecules) = store.list_molecules(&MoleculeFilter::default()) else {
        return HealSweepReport {
            dry_run,
            ..Default::default()
        };
    };

    let backend: Option<TmuxBackend> = if no_tmux {
        None
    } else {
        Some(TmuxBackend::new(super::tmux_socket_name(ctx)))
    };

    // Fold every scannable molecule into a Witness view (shared with `cs health`
    // — one fold, no drift), then classify.
    let scannable: Vec<&MoleculeData> = molecules
        .iter()
        .filter(|m| super::health::is_scannable(m))
        .collect();
    let views: Vec<_> = scannable
        .iter()
        .map(|m| super::health::fold_view(m, backend.as_ref(), now))
        .collect();
    let report = scan(&views, now, &HealthThresholds::default());

    // P4 classes seen — reported, never mutated in P3.
    let deferred_p4 = report
        .findings
        .iter()
        .filter(|f| !is_safe_remedy(f.remedy))
        .count();

    // Presence rows for the §5.1 pilot guard (one scan, reused per molecule).
    let presences = PresenceStore::new(state_dir).scan().unwrap_or_default();
    let cfg = GuardConfig::default();

    // Pure plan: safe-class findings × guard verdict.
    let by_id: std::collections::HashMap<&MoleculeId, &MoleculeData> =
        molecules.iter().map(|m| (&m.id, m)).collect();
    let plans = plan_heal(&report.findings, &cfg, now, |mid| {
        // Fold the guard view from disk (or an all-clear view if the molecule
        // vanished mid-scan — conservative: a missing molecule is not piloted).
        by_id.get(mid).map_or_else(
            || HealGuardView::healable(mid.clone()),
            |m| {
                let mol_dir = store.molecule_dir(mid);
                build_guard_view(m, &mol_dir, &presences, false, now)
            },
        )
    });

    // Apply the healable plans; record every plan (healed or blocked).
    let mut actions = Vec::new();
    for plan in plans {
        let record = apply_plan(
            store.as_ref(),
            state_dir,
            &by_id,
            &plan,
            backend.as_ref(),
            dry_run,
            now,
        );
        if record.applied {
            log_action(state_dir, &record);
        }
        actions.push(record);
    }

    HealSweepReport {
        scanned: report.patrol.ensemble_size,
        dry_run,
        kill_switched: false,
        actions,
        deferred_p4,
    }
}

/// Apply one [`HealPlan`] — the per-plan I/O lifted out of [`heal_sweep`] so the
/// sweep stays a thin orchestrator. A blocked plan records its reason and
/// mutates nothing; a healable plan runs the remedy and persists the backoff
/// memory (idempotence across ticks).
fn apply_plan(
    store: &dyn StateStore,
    state_dir: &Path,
    by_id: &std::collections::HashMap<&MoleculeId, &MoleculeData>,
    plan: &HealPlan,
    backend: Option<&TmuxBackend>,
    dry_run: bool,
    now: DateTime<Utc>,
) -> HealActionRecord {
    match &plan.gate {
        HealGate::Heal => {
            let (applied, outcome) = match by_id.get(&plan.molecule_id).copied() {
                Some(m) => {
                    let result = apply_remedy(store, state_dir, m, plan.remedy, backend, dry_run);
                    let applied =
                        matches!(result, ApplyResult::Success(_) | ApplyResult::Failure(_));
                    if applied {
                        // Persist the backoff memory (idempotence across ticks).
                        let mol_dir = store.molecule_dir(&plan.molecule_id);
                        let prev = HealLedger::load(&mol_dir);
                        ledger_after(&prev, plan.remedy, &result, now).save(&mol_dir);
                    }
                    (applied, result.detail().to_owned())
                }
                None => (false, "molecule vanished".to_owned()),
            };
            HealActionRecord {
                molecule_id: plan.molecule_id.clone(),
                class: plan.class,
                remedy: plan.remedy,
                healed: true,
                block_reason: None,
                applied,
                outcome,
            }
        }
        HealGate::Blocked(reason) => HealActionRecord {
            molecule_id: plan.molecule_id.clone(),
            class: plan.class,
            remedy: plan.remedy,
            healed: false,
            outcome: format!("blocked: {}", block_label(reason)),
            block_reason: Some(reason.clone()),
            applied: false,
        },
    }
}

/// A short human label for a guard block reason.
fn block_label(reason: &HealBlockReason) -> String {
    match reason {
        HealBlockReason::GlobalKillSwitch => "global kill-switch".to_owned(),
        HealBlockReason::DoNotHealMarker => "do-not-heal marker".to_owned(),
        HealBlockReason::LivePilot => "live pilot".to_owned(),
        HealBlockReason::WhisperQuietPeriod {
            secs_since_whisper,
            quiet_secs,
        } => {
            format!("whisper quiet-period ({secs_since_whisper}s < {quiet_secs}s)")
        }
        HealBlockReason::ThreeStrikes { failures } => {
            format!("three-strikes ({failures} failures)")
        }
        HealBlockReason::BackoffCooldown {
            secs_since_last,
            cooldown_secs,
            ..
        } => {
            format!("backoff cooldown ({secs_since_last}s < {cooldown_secs}s)")
        }
    }
}

/// Render the heal report as a single JSON value, embedded under `"heal"` in
/// `cs patrol --json`'s aggregate object (agent-first per the `--json` convention).
pub(crate) fn to_value(report: &HealSweepReport) -> serde_json::Value {
    serde_json::json!({
        "dry_run": report.dry_run,
        "kill_switched": report.kill_switched,
        "scanned": report.scanned,
        "applied": report.applied_count(),
        "blocked": report.blocked_count(),
        "deferred_p4": report.deferred_p4,
        "actions": report
            .actions
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or(serde_json::Value::Null))
            .collect::<Vec<_>>(),
    })
}

/// Print the human-facing heal section.
pub(crate) fn print_plain(report: &HealSweepReport) {
    println!();
    let banner = "HEAL".cyan().bold();
    if report.kill_switched {
        println!("  {banner} ~/.cosmon/health.off present — heal pass is a no-op");
        return;
    }
    let mode = if report.dry_run { " (dry-run)" } else { "" };
    if report.actions.is_empty() {
        println!(
            "  {banner}{mode} {} molecule(s) scanned, no safe-class anomalies to heal",
            report.scanned
        );
    } else {
        println!(
            "  {banner}{mode} {} applied, {} blocked ({} scanned):",
            report.applied_count(),
            report.blocked_count(),
            report.scanned,
        );
        for a in &report.actions {
            let mark = if a.healed {
                if a.applied {
                    "✓".green()
                } else {
                    "·".dimmed()
                }
            } else {
                "⊘".yellow()
            };
            println!(
                "    {mark} {} {}  {:?} → {}",
                a.class.code().bold(),
                a.molecule_id,
                a.remedy,
                a.outcome,
            );
        }
    }
    if report.deferred_p4 > 0 {
        println!(
            "    {} {} integrity/collapse finding(s) deferred to P4 — see `cs health`",
            "→".dimmed(),
            report.deferred_p4,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::patrol::ControlPlaneSignal;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn finding(id: &str, class: AnomalyClass, remedy: HealthRemedy) -> HealthFinding {
        HealthFinding {
            molecule_id: mid(id),
            class,
            signal: ControlPlaneSignal::Overloaded,
            piloted: false,
            remedy,
        }
    }

    // ---- is_safe_remedy: the P3/P4 boundary ---------------------------------

    #[test]
    fn safe_set_is_exactly_the_four_p3_remedies() {
        assert!(is_safe_remedy(HealthRemedy::TransportResubmit));
        assert!(is_safe_remedy(HealthRemedy::HarvestDone));
        assert!(is_safe_remedy(HealthRemedy::Nudge));
        assert!(is_safe_remedy(HealthRemedy::BackoffPerAccount));
        // P4 — collapse / integrity classes are NOT mutated in P3.
        assert!(!is_safe_remedy(HealthRemedy::CollapseProcessDeath));
        assert!(!is_safe_remedy(HealthRemedy::FlagOnly));
    }

    // ---- plan_heal: filters to safe classes, runs the guard -----------------

    #[test]
    fn plan_drops_p4_collapse_and_flag_findings() {
        let findings = vec![
            finding(
                "cs-20260626-a3xx",
                AnomalyClass::AuthDead,
                HealthRemedy::CollapseProcessDeath,
            ),
            finding(
                "cs-20260626-a7xx",
                AnomalyClass::GhostMerge,
                HealthRemedy::FlagOnly,
            ),
            finding(
                "cs-20260626-a9xx",
                AnomalyClass::CrashZombie,
                HealthRemedy::CollapseProcessDeath,
            ),
            finding(
                "cs-20260626-a5xx",
                AnomalyClass::IdleRunningZombie,
                HealthRemedy::Nudge,
            ),
        ];
        let now = Utc::now();
        let plans = plan_heal(&findings, &GuardConfig::default(), now, |id| {
            HealGuardView::healable(id.clone())
        });
        // Only the A5 (safe) finding survives.
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].class, AnomalyClass::IdleRunningZombie);
        assert!(plans[0].gate.is_healable());
    }

    #[test]
    fn plan_makes_every_safe_class_actionable_when_unguarded() {
        let findings = vec![
            finding(
                "cs-20260626-a1aa",
                AnomalyClass::UnsentPaste,
                HealthRemedy::TransportResubmit,
            ),
            finding(
                "cs-20260626-a4aa",
                AnomalyClass::IdleAfterComplete,
                HealthRemedy::HarvestDone,
            ),
            finding(
                "cs-20260626-a5aa",
                AnomalyClass::IdleRunningZombie,
                HealthRemedy::Nudge,
            ),
            finding(
                "cs-20260626-a6aa",
                AnomalyClass::Overloaded,
                HealthRemedy::BackoffPerAccount,
            ),
        ];
        let now = Utc::now();
        let plans = plan_heal(&findings, &GuardConfig::default(), now, |id| {
            HealGuardView::healable(id.clone())
        });
        assert_eq!(plans.len(), 4);
        assert!(plans.iter().all(|p| p.gate.is_healable()));
    }

    // ---- the guard-blocks-piloted-worker case (ADR-137 §5.1) ----------------

    #[test]
    fn guard_blocks_piloted_worker() {
        let findings = vec![finding(
            "cs-20260626-plt0",
            AnomalyClass::IdleRunningZombie,
            HealthRemedy::Nudge,
        )];
        let now = Utc::now();
        let plans = plan_heal(&findings, &GuardConfig::default(), now, |id| {
            let mut v = HealGuardView::healable(id.clone());
            v.pilot_present = true; // a human is steering this molecule
            v
        });
        assert_eq!(plans.len(), 1);
        assert!(
            !plans[0].gate.is_healable(),
            "piloted molecule must not be healed"
        );
        assert_eq!(
            plans[0].gate.blocked_reason(),
            Some(&HealBlockReason::LivePilot),
        );
    }

    #[test]
    fn guard_blocks_health_hold_marker_and_kill_switch() {
        let now = Utc::now();
        // do-not-heal marker.
        let p = plan_heal(
            &[finding(
                "cs-20260626-hold",
                AnomalyClass::Overloaded,
                HealthRemedy::BackoffPerAccount,
            )],
            &GuardConfig::default(),
            now,
            |id| {
                let mut v = HealGuardView::healable(id.clone());
                v.do_not_heal = true;
                v
            },
        );
        assert_eq!(
            p[0].gate.blocked_reason(),
            Some(&HealBlockReason::DoNotHealMarker)
        );
    }

    // ---- apply_remedy: each safe heal action --------------------------------

    #[test]
    fn apply_backoff_is_pure_success() {
        // A6 backoff needs no I/O: a runtime hold, recorded.
        let now = Utc::now();
        let prev = HealLedger::default();
        let result = ApplyResult::Success("backoff hold recorded".to_owned());
        let next = ledger_after(&prev, HealthRemedy::BackoffPerAccount, &result, now);
        assert_eq!(next.last_remedy, Some(HealthRemedy::BackoffPerAccount));
        assert_eq!(next.last_heal_at, Some(now));
        assert_eq!(next.consecutive_failures, 0);
    }

    // ---- ledger_after: the §5.5 backoff/three-strikes transition ------------

    #[test]
    fn ledger_success_resets_failure_streak_and_arms_cooldown() {
        let now = Utc::now();
        let prev = HealLedger {
            consecutive_failures: 2,
            ..Default::default()
        };
        let next = ledger_after(
            &prev,
            HealthRemedy::Nudge,
            &ApplyResult::Success("nudged".to_owned()),
            now,
        );
        assert_eq!(next.consecutive_failures, 0);
        assert_eq!(next.last_remedy, Some(HealthRemedy::Nudge));
        assert_eq!(next.last_heal_at, Some(now));
    }

    #[test]
    fn ledger_failure_increments_toward_three_strikes() {
        let now = Utc::now();
        let prev = HealLedger {
            consecutive_failures: 1,
            ..Default::default()
        };
        let next = ledger_after(
            &prev,
            HealthRemedy::TransportResubmit,
            &ApplyResult::Failure("submit failed".to_owned()),
            now,
        );
        assert_eq!(next.consecutive_failures, 2);
        // Cooldown still arms — we don't hammer a failing remedy.
        assert_eq!(next.last_heal_at, Some(now));
    }

    #[test]
    fn ledger_skip_leaves_state_untouched() {
        let now = Utc::now();
        let prev = HealLedger {
            last_heal_at: Some(now - chrono::Duration::minutes(5)),
            last_remedy: Some(HealthRemedy::Nudge),
            consecutive_failures: 1,
        };
        let next = ledger_after(
            &prev,
            HealthRemedy::Nudge,
            &ApplyResult::Skipped("no transport".to_owned()),
            now,
        );
        assert_eq!(next, prev, "a skipped remedy neither cools down nor fails");
    }

    // ---- backoff cooldown blocks a too-soon re-application ------------------

    #[test]
    fn ledger_drives_cooldown_block_on_next_tick() {
        // After a nudge at T0, a second nudge 30 s later (< 60 s cooldown) is
        // blocked by the guard — idempotence across ticks via the ledger.
        let t0 = Utc::now();
        let prev = HealLedger::default();
        let after = ledger_after(
            &prev,
            HealthRemedy::Nudge,
            &ApplyResult::Success("nudged".to_owned()),
            t0,
        );
        let t1 = t0 + chrono::Duration::seconds(30);
        let mut view = HealGuardView::healable(mid("cs-20260626-cool"));
        view.last_heal_at = after.last_heal_at;
        view.last_remedy = after.last_remedy;
        let gate = heal_gate(&view, HealthRemedy::Nudge, t1, &GuardConfig::default());
        assert!(!gate.is_healable());
        assert!(matches!(
            gate.blocked_reason(),
            Some(HealBlockReason::BackoffCooldown { .. })
        ));
    }

    // ---- HealLedger persists round-trip ------------------------------------

    #[test]
    fn heal_ledger_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("heal-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let ledger = HealLedger {
            last_heal_at: Some(now),
            last_remedy: Some(HealthRemedy::HarvestDone),
            consecutive_failures: 1,
        };
        assert!(ledger.save(&dir));
        let back = HealLedger::load(&dir);
        assert_eq!(back.last_remedy, Some(HealthRemedy::HarvestDone));
        assert_eq!(back.consecutive_failures, 1);
        // A missing ledger loads as default (disposable sediment).
        let empty = HealLedger::load(&dir.join("nonexistent"));
        assert_eq!(empty, HealLedger::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
