// SPDX-License-Identifier: AGPL-3.0-only

//! `cs collapse` — terminate a molecule with final state recording.
//!
//! Transitions a molecule to `Collapsed` (terminal). Records the reason
//! and the step at which the collapse occurred.

use std::fs;
use std::str::FromStr;

use chrono::Utc;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::event_v2::{CollapseReason, EventV2};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
use cosmon_filestore::FileStore;
use cosmon_state::event_log;

use super::Context;

/// Arguments for the `collapse` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to collapse.
    molecule: String,

    /// Reason for the collapse. With `--with-seats` it may be omitted when
    /// the named molecule is already collapsed: its recorded reason is then
    /// inherited, suffixed with the seat and work.
    #[arg(long, required_unless_present = "with_seats")]
    reason: Option<String>,

    /// Also collapse every other live member of the declared work the
    /// molecule belongs to (owner or seat), with the same reason. Never
    /// merges; a completed seat is skipped and `cs done <seat>` is printed.
    #[arg(long)]
    with_seats: bool,

    /// Structured cause attribution (ADR-062): `rate_limit`,
    /// `inference_stall`, `manual`, `process_death`, `unknown`. With
    /// `rate_limit`, pair `--account` and `--kind` for the K3 fixture
    /// shape.
    #[arg(long, value_name = "CAUSE")]
    cause: Option<String>,

    /// Account alias for `--cause rate_limit` (e.g. `default`).
    #[arg(long, value_name = "ALIAS")]
    account: Option<String>,

    /// Quota currency name for `--cause rate_limit` (e.g.
    /// `max_rolling_5h`, `max_weekly`, `api_key_org_monthly`,
    /// `financial_usd`, `custody_scoped`). Free-form to remain
    /// extensible across providers.
    #[arg(long, value_name = "KIND")]
    kind: Option<String>,

    /// Operator-facing collapse classification for `cs errors`
    /// aggregation: one of `worker_crashed`, `gate_failed`,
    /// `blocker_stuck`, `manual_abort`, `resource_exhausted`. Any other
    /// value lands in [`CollapseReason::Other`] verbatim; by convention
    /// `verification_blocked` marks work done whose gates were blocked by
    /// infrastructure — the worker's claim, never merge-safety evidence.
    #[arg(long, value_name = "REASON_KIND")]
    reason_kind: Option<String>,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<std::path::PathBuf>,

    /// Conditional, idempotent collapse (issue #207).
    #[command(flatten)]
    condition: ConditionArgs,
}

/// Parse the `--cause` / `--account` / `--kind` triple into a
/// [`CollapseCause`]. Returns `None` if `--cause` was not supplied —
/// preserving the legacy free-form-`reason`-only behaviour.
fn parse_cause(args: &Args) -> anyhow::Result<Option<CollapseCause>> {
    let Some(raw) = args.cause.as_deref() else {
        return Ok(None);
    };
    let mut cause = CollapseCause::from_str(raw).map_err(|_| {
        anyhow::anyhow!(
            "unknown --cause `{raw}` (expected one of: rate_limit, \
             inference_stall, manual, process_death, unknown)"
        )
    })?;
    if let CollapseCause::RateLimit {
        account,
        kind_quota,
    } = &mut cause
    {
        account.clone_from(&args.account);
        kind_quota.clone_from(&args.kind);
    } else if args.account.is_some() || args.kind.is_some() {
        anyhow::bail!(
            "--account / --kind are only meaningful with --cause rate_limit \
             (received --cause {raw})"
        );
    }
    Ok(Some(cause))
}

/// Collapse the named molecule and, with `--with-seats`, its whole roster.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if args.with_seats {
        return run_with_seats(ctx, args);
    }
    let reason = args
        .reason
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--reason is required"))?;
    collapse_molecule(ctx, args, &MoleculeId::new(&args.molecule)?, reason)
}

/// Fan `cs collapse` out over the declared work of the named molecule.
///
/// The roster is read once as a target list; each seat then goes through
/// [`collapse_molecule`], the same path as a single collapse (lease guard,
/// archive, events). Nothing is merged. A rerun converges from each
/// molecule's own state, so no progress record is kept.
fn run_with_seats(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    use super::work::resolve_roster;

    let named = MoleculeId::new(&args.molecule)?;
    let roster = resolve_roster(ctx, &named)?;
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = ctx.store_at(&ops_dir);
    let owner_state = store.load_molecule(&roster.owner)?;
    let inherited = match (
        &args.reason,
        owner_state.status,
        &owner_state.collapse_reason,
    ) {
        (Some(_), _, _) => None,
        (None, MoleculeStatus::Collapsed, Some(reason)) => Some(reason.clone()),
        (None, ..) => anyhow::bail!(
            "--reason is required unless the owner {} is already collapsed with a recorded reason",
            roster.owner
        ),
    };
    if !ctx.json {
        println!(
            "work {} revision {}; {} seats",
            roster.owner,
            roster.revision,
            roster.seats.len()
        );
    }
    // The named molecule first, then the other members in roster order.
    let mut targets: Vec<_> = roster.seats.iter().collect();
    targets.sort_by_key(|(_, molecule)| *molecule != named);
    let mut failed = Vec::new();
    for (seat, molecule) in targets {
        let seat_status = store.load_molecule(molecule).map(|data| data.status);
        if matches!(seat_status, Ok(MoleculeStatus::Completed)) {
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({"molecule": molecule.as_str(), "seat": seat.as_str(), "skipped": "completed", "harvest_command": format!("cs done {molecule}")})
                );
            } else {
                println!("skipped {molecule} (seat {seat}): completed — run `cs done {molecule}`");
            }
            continue;
        }
        let reason = match (&args.reason, &inherited) {
            (Some(reason), _) => reason.clone(),
            (None, Some(owner_reason)) => format!(
                "{owner_reason} (seat {seat} of work {}@{})",
                roster.owner, roster.revision
            ),
            (None, None) => unreachable!("checked above"),
        };
        if let Err(error) = collapse_molecule(ctx, args, molecule, &reason) {
            eprintln!("failed {molecule} (seat {seat}): {error:#}");
            failed.push(molecule.to_string());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} seat(s) not collapsed: {} — rerun to retry",
            failed.len(),
            failed.join(", ")
        )
    }
}

/// Execute the `collapse` command for one molecule.
///
/// Every collapse goes through [`cosmon_state::ops::collapse::execute`], the
/// operation MCP calls too. This function keeps only the CLI's projections:
/// the legacy event mirror, the archive, and the rendering.
fn collapse_molecule(
    ctx: &Context,
    args: &Args,
    mol_id: &MoleculeId,
    reason: &str,
) -> anyhow::Result<()> {
    use cosmon_state::ops::collapse::{execute, CollapseOutcome, CollapseRequest, ExecuteError};

    let mol_id = mol_id.clone();
    // Validated here for its message; the operation validates it again.
    let cause = parse_cause(args)?;
    let reason_kind = args
        .reason_kind
        .as_deref()
        .map(|s| CollapseReason::from(s.to_owned()));
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = ctx.store_at(&ops_dir);
    let condition = args.condition.parse()?;

    // ADR-168 §D6 — authority guard, now inside the shared operation. The
    // lease is read from `ops_dir`, the directory this call mutates: with
    // `--ops-dir` pointing elsewhere than the ambient state dir, checking the
    // ambient ledger would clear a gesture against a mission it does not
    // hold. The caller is this process's environment, resolved here and
    // passed explicitly.
    let authority =
        cosmon_harvest::pilot_gesture::LeaseCollapseAuthority::new(&ops_dir, "cs collapse");
    let caller = cosmon_core::collapse_contract::CallerContext::Local {
        session: cosmon_harvest::pilot_gesture::resolve_pilot_session(&super::guard::process_env),
    };
    let request = CollapseRequest {
        reason: reason.to_owned(),
        cause: args.cause.clone(),
        account: args.account.clone(),
        kind: args.kind.clone(),
        reason_kind: reason_kind.clone(),
    };

    let store: &dyn cosmon_state::StateStore = &*store;
    let outcome = execute(
        store,
        &ops_dir,
        &authority,
        &caller,
        &mol_id,
        request,
        condition,
        Utc::now(),
    )
    .map_err(|e| match e {
        ExecuteError::UnleasedPilotGesture(refusal) => super::guard::rehome_unleased_pilot_gesture(
            cosmon_harvest::pilot_gesture::UnleasedPilotGesture {
                gesture: "cs collapse",
                mol_id: mol_id.as_str().to_owned(),
                why: refusal.why,
            }
            .into(),
        ),
        ExecuteError::Collapse(other) => anyhow::Error::new(other),
    })?;

    let keyed = match outcome {
        CollapseOutcome::Keyed(keyed) => keyed,
        CollapseOutcome::Unconditional { view, molecule } => {
            let Some(updated) = molecule else {
                return render_already_collapsed(ctx, store, &mol_id);
            };
            let prev_status_label = if view.previous_was_stuck {
                "stuck".to_owned()
            } else {
                view.previous_status.to_string()
            };
            let updated = project_transition(ctx, store, &ops_dir, updated, view.previous_status);
            return render_transition(
                ctx,
                store,
                &updated,
                &prev_status_label,
                reason,
                cause.as_ref(),
                reason_kind.as_ref(),
            );
        }
    };

    if keyed.announced {
        if let (Some(updated), Some(receipt)) = (keyed.molecule.clone(), keyed.receipt.as_ref()) {
            project_transition(ctx, store, &ops_dir, updated, receipt.previous_status);
        } else if let Some(receipt) = keyed.receipt.as_ref() {
            // A replay that finished an interrupted commit: the state is
            // already collapsed; only the legacy mirror is owed.
            emit_legacy_events(&ops_dir, &mol_id, receipt.previous_status, reason);
        }
    }
    render_keyed(ctx, &mol_id, &keyed)
}

/// Exit codes of keyed collapse refusals. The `--json` `outcome` field is the
/// contract; these let a shell script branch without parsing it.
pub(crate) mod exit_code {
    /// `idempotency_key_reused`.
    pub const IDEMPOTENCY_KEY_REUSED: i32 = 20;
    /// `already_completed`.
    pub const ALREADY_COMPLETED: i32 = 21;
    /// `already_collapsed`.
    pub const ALREADY_COLLAPSED: i32 = 22;
    /// `approval_expired`.
    pub const APPROVAL_EXPIRED: i32 = 23;
    /// `unexpected_status`.
    pub const UNEXPECTED_STATUS: i32 = 24;
    /// `stale_revision`.
    pub const STALE_REVISION: i32 = 25;
    /// `cs collapse-receipt`: the key record and the state disagree.
    pub const RECEIPT_INCONSISTENT: i32 = 26;
}

/// A keyed collapse that was refused, carried to `main` for its exit code.
#[derive(Debug, thiserror::Error)]
#[error("cs collapse {molecule}: refused — {outcome}{detail}")]
pub struct CollapseRefused {
    /// Molecule targeted.
    pub molecule: String,
    /// Decision-table label.
    pub outcome: &'static str,
    /// Human-readable detail, prefixed with ": " when present.
    pub detail: String,
    /// Process exit code.
    pub code: i32,
}

/// Exit code of a [`CollapseRefused`] anywhere in the error chain.
#[must_use]
pub fn refusal_exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<CollapseRefused>().map(|r| r.code)
}

/// The keyed flags of `cs collapse`. All-or-nothing: `--key` requires an
/// expected status and revision, and every other flag requires `--key`.
#[derive(clap::Args, Debug, Default, Clone)]
pub struct ConditionArgs {
    /// Make the collapse conditional and idempotent under this caller key
    /// (`[A-Za-z0-9_-]{16,128}`, generated once by the caller and resent
    /// unchanged on every retry). Requires --expect-status and
    /// --expect-revision. Without --key, `cs collapse` is unconditional, as
    /// it always was.
    #[arg(
        long,
        value_name = "KEY",
        requires_all = ["expect_status", "expect_revision"],
        conflicts_with = "with_seats"
    )]
    pub key: Option<String>,

    /// Status the caller confirmed (pending, queued, running, frozen,
    /// starved). A different status is refused `unexpected_status`.
    #[arg(long, value_name = "STATUS", requires = "key")]
    pub expect_status: Option<String>,

    /// Revision the caller confirmed, as `cs observe <ID> --json` prints it
    /// (`b3:…`). A different revision is refused `stale_revision`.
    #[arg(long, value_name = "REVISION", requires = "key")]
    pub expect_revision: Option<String>,

    /// Opaque digest of the operator approval; recorded in the request
    /// digest so a different approval is a different request.
    #[arg(long, value_name = "DIGEST", requires = "key")]
    pub approval_digest: Option<String>,

    /// RFC 3339 deadline of the approval. After it the collapse is refused
    /// `approval_expired` (a replay of a committed key still succeeds).
    #[arg(long, value_name = "RFC3339", requires = "key")]
    pub not_after: Option<String>,
}

impl ConditionArgs {
    /// The validated condition, `None` without `--key`.
    fn parse(&self) -> anyhow::Result<Option<cosmon_core::collapse_contract::CollapseCondition>> {
        use cosmon_core::collapse_contract::{
            CollapseCondition, CollapsibleStatus, IdempotencyKey, MoleculeRevision,
        };
        let Some(key) = self.key.as_deref() else {
            return Ok(None);
        };
        let (Some(status), Some(revision)) = (
            self.expect_status.as_deref(),
            self.expect_revision.as_deref(),
        ) else {
            anyhow::bail!("--key requires --expect-status and --expect-revision");
        };
        let not_after = self
            .not_after
            .as_deref()
            .map(|raw| {
                chrono::DateTime::parse_from_rfc3339(raw)
                    .map(|t| t.with_timezone(&Utc))
                    .map_err(|e| anyhow::anyhow!("--not-after `{raw}` is not RFC 3339: {e}"))
            })
            .transpose()?;
        if let Some(digest) = self.approval_digest.as_deref() {
            anyhow::ensure!(
                !digest.is_empty() && digest.len() <= 256,
                "--approval-digest must be 1 to 256 bytes"
            );
        }
        Ok(Some(CollapseCondition {
            key: IdempotencyKey::parse(key)?,
            expect_status: CollapsibleStatus::from_str(status)?,
            expect_revision: MoleculeRevision::parse(revision)?,
            approval_digest: self.approval_digest.clone(),
            not_after,
        }))
    }
}

/// Render a keyed outcome; a refusal becomes a [`CollapseRefused`] error.
fn render_keyed(
    ctx: &Context,
    mol_id: &MoleculeId,
    keyed: &cosmon_state::ops::KeyedCollapse,
) -> anyhow::Result<()> {
    use cosmon_core::collapse_contract::{CollapseOutcomeKind as K, CollapseRefusal as R};

    let json = cosmon_state::ops::KeyedCollapseJson::from_outcome(mol_id, keyed);
    if ctx.json {
        println!("{}", serde_json::to_string(&json)?);
    }
    let Some(refusal) = keyed.refusal.as_ref() else {
        if !ctx.json {
            let receipt_digest = keyed.receipt.as_ref().map_or("", |r| r.digest.as_str());
            println!(
                "{} {} collapsed ({}) — digest {}",
                MoleculeStatus::Collapsed.emoji(),
                mol_id,
                keyed.outcome.as_str(),
                receipt_digest
            );
            println!("  read it back: cs collapse-receipt {mol_id} <KEY>");
        }
        return Ok(());
    };
    if let R::UnleasedPilotGesture { why } = refusal {
        return Err(super::guard::rehome_unleased_pilot_gesture(
            cosmon_harvest::pilot_gesture::UnleasedPilotGesture {
                gesture: "cs collapse",
                mol_id: mol_id.as_str().to_owned(),
                why: why.clone(),
            }
            .into(),
        ));
    }
    let (code, detail) = match refusal {
        R::IdempotencyKeyReused { .. } => (
            exit_code::IDEMPOTENCY_KEY_REUSED,
            "the key is bound to a different request; generate a new key".to_owned(),
        ),
        R::AlreadyCompleted => (exit_code::ALREADY_COMPLETED, String::new()),
        R::AlreadyCollapsed { by_key } => (
            exit_code::ALREADY_COLLAPSED,
            by_key.as_ref().map_or_else(
                || "collapsed without a key".to_owned(),
                |k| format!("collapsed under another key ({k})"),
            ),
        ),
        R::ApprovalExpired { not_after } => (
            exit_code::APPROVAL_EXPIRED,
            format!("--not-after {not_after} has passed"),
        ),
        R::UnexpectedStatus { actual } => {
            (exit_code::UNEXPECTED_STATUS, format!("status is {actual}"))
        }
        R::StaleRevision { actual } => (
            exit_code::STALE_REVISION,
            format!("revision is now {actual}; re-read it and confirm again"),
        ),
        R::UnleasedPilotGesture { .. } => unreachable!("handled above"),
    };
    debug_assert!(!matches!(keyed.outcome, K::Replayed | K::Transitioned));
    Err(CollapseRefused {
        molecule: mol_id.as_str().to_owned(),
        outcome: keyed.outcome.as_str(),
        detail: if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        },
        code,
    }
    .into())
}

/// The CLI's post-commit projections of a transition: the legacy event
/// mirror and the ADR-030 archive. Returns the record as last saved.
fn project_transition(
    ctx: &Context,
    store: &dyn cosmon_state::StateStore,
    ops_dir: &std::path::Path,
    mut updated: cosmon_state::MoleculeData,
    prev_status: MoleculeStatus,
) -> cosmon_state::MoleculeData {
    let reason = updated.collapse_reason.clone().unwrap_or_default();
    emit_legacy_events(ops_dir, &updated.id, prev_status, &reason);

    // ADR-030 M3 — archive the collapsed molecule when the subsystem is
    // enabled. Non-fatal: any failure is logged and the collapse still
    // succeeds (the caller owns the terminal transition). The
    // `updated.archived` idempotence gate makes re-running `cs collapse`
    // a no-op on the archive — a prior successful write is never
    // clobbered, and collapsing an already-archived molecule never
    // double-counts.
    let config_path = super::resolve_config_from_context(ctx);
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
    if project_config.archive.enabled && !updated.archived {
        let mol_dir = store.molecule_dir(&updated.id);
        let archive_mol_dir = cosmon_state::archive::resolve_molecule_dir(ops_dir, &updated.id)
            .unwrap_or_else(|| mol_dir.clone());
        if cosmon_state::archive::write_non_fatal(
            ops_dir,
            &archive_mol_dir,
            &updated,
            cosmon_state::archive::Trigger::Collapse,
            Utc::now(),
        )
        .is_some()
        {
            updated.archived = true;
            let _ = store.save_molecule(&updated.id.clone(), &updated);
        }
    }
    updated
}

/// The legacy `events.jsonl` mirror of a collapse (pre-`EventV2` readers).
fn emit_legacy_events(
    ops_dir: &std::path::Path,
    mol_id: &MoleculeId,
    prev_status: MoleculeStatus,
    reason: &str,
) {
    let events_path = ops_dir.join("events.jsonl");
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeTransitioned {
            molecule_id: mol_id.clone(),
            from: prev_status,
            to: MoleculeStatus::Collapsed,
        }),
    );
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason: reason.to_owned(),
        }),
    );
}

/// Render the unconditional no-op on an already-collapsed molecule.
fn render_already_collapsed(
    ctx: &Context,
    store: &dyn cosmon_state::StateStore,
    mol_id: &MoleculeId,
) -> anyhow::Result<()> {
    let mol_data = store.load_molecule(mol_id)?;
    let fleet = store.load_fleet().unwrap_or_default();
    let location = super::work_location::WorkLocation::from_state(
        &mol_data,
        &fleet,
        &super::work_location::repo_root(ctx),
    );
    if ctx.json {
        let out = serde_json::json!({
            "molecule": mol_id.as_str(),
            "status": "collapsed",
            "already_collapsed": true,
            "branch": location.branch,
            "worktree": location.worktree,
            "harvest_command": location.harvest_command,
            "branch_deletion_requires_audit": true,
        });
        println!("{}", serde_json::to_string(&out)?);
    } else {
        println!(
            "{} {} already collapsed (no-op)",
            MoleculeStatus::Collapsed.emoji(),
            mol_id
        );
        println!("  {}", location.render());
        println!("  collapse records abandonment only; audit the branch before deletion.");
    }
    Ok(())
}

/// Render a fresh unconditional transition.
fn render_transition(
    ctx: &Context,
    store: &dyn cosmon_state::StateStore,
    updated: &cosmon_state::MoleculeData,
    prev_status_label: &str,
    reason: &str,
    cause: Option<&CollapseCause>,
    reason_kind: Option<&CollapseReason>,
) -> anyhow::Result<()> {
    let mol_id = &updated.id;
    let fleet = store.load_fleet().unwrap_or_default();
    let location = super::work_location::WorkLocation::from_state(
        updated,
        &fleet,
        &super::work_location::repo_root(ctx),
    );
    if ctx.json {
        let out = serde_json::json!({
            "molecule": mol_id.as_str(),
            "previous_status": prev_status_label,
            "status": "collapsed",
            "reason": reason,
            "cause": cause,
            "reason_kind": reason_kind.map(CollapseReason::as_str),
            "archived": updated.archived,
            "nudge_count": updated.nudge_count,
            "branch": location.branch,
            "worktree": location.worktree,
            "harvest_command": location.harvest_command,
            "branch_deletion_requires_audit": true,
        });
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }
    let cause_label = cause.map(|c| format!(" [{c}]")).unwrap_or_default();
    println!(
        "{} {} collapsed (was {}){}: {}",
        MoleculeStatus::Collapsed.emoji(),
        mol_id,
        prev_status_label,
        cause_label,
        reason
    );
    println!("  {}", location.render());
    println!("  collapse records abandonment only; audit the branch before deletion.");
    // Post-mortem nudge accounting (delib-20260420-1b02 P2):
    // surface how many times `cs patrol --nudge` had to poke this
    // molecule before it collapsed. > 2 nudges suggests an
    // ambiguous briefing rather than a runtime stall — a data
    // point a future audit formula will read.
    if updated.nudge_count > 0 {
        let suffix = if updated.nudge_count == 1 { "" } else { "s" };
        println!(
            "  • nudge_count: {} patrol nudge{suffix}",
            updated.nudge_count
        );
        if updated.nudge_count > 2 {
            println!("  ⚠ nudge_count > 2 — likely a briefing-clarity issue, not a runtime bug");
        }
    }
    Ok(())
}

/// Drive the canonical `→ Collapsed` terminal transition programmatically,
/// mirroring [`super::complete::complete_one`] on the failure side.
///
/// Exposed as `pub(crate)` so the in-process Direct-API branch of `cs tackle`
/// can collapse a molecule **loudly** when its synchronous agent loop returned
/// `Ok` but performed **no observable work** (zero tool dispatches → untouched
/// worktree, no artefact). Sealing such a molecule `Completed` would violate
/// the invariant *a completed molecule must have executed its steps or
/// collapsed loudly* (ADR-100 R2; founding witness `cmbverify-20260724-54cd`,
/// sealed 0/4 with an empty worktree). This helper is the loud alternative.
///
/// Byte-compatible with `cs collapse`'s terminal sequence: status flip to
/// `Collapsed`, `collapse_reason` / `collapsed_step` recorded, live-process
/// record released, `log.md` appended, and the legacy + `EventV2`
/// `MoleculeCollapsed` events emitted. The archive step (`cs collapse`'s
/// ADR-030 M3 tail) is intentionally omitted — this path has no `Context` and
/// the caller does not need archival for a zero-work refusal.
///
/// # Errors
///
/// Returns an error if the molecule cannot be loaded or saved, or if it is
/// already in a terminal `Completed` state (collapsing a completed molecule
/// would erase its completion). An already-`Collapsed` molecule is idempotent.
#[cfg(test)]
pub(crate) fn collapse_one(
    store: &FileStore,
    ops_dir: &std::path::Path,
    mol_id: &MoleculeId,
    reason: &str,
) -> anyhow::Result<MoleculeStatus> {
    collapse_one_with_kind(store, ops_dir, mol_id, reason, None)
}

/// Collapse a synchronous worker with a recorded machine-readable cause.
///
/// # Errors
/// Returns persistence and transition errors from the canonical collapse path.
pub(crate) fn collapse_one_with_kind(
    store: &FileStore,
    ops_dir: &std::path::Path,
    mol_id: &MoleculeId,
    reason: &str,
    kind: Option<CollapseReason>,
) -> anyhow::Result<MoleculeStatus> {
    use cosmon_state::StateStore;

    let prev_status = {
        let _g = store.lock_fleet()?;
        let mol_data = store.load_molecule(mol_id)?;
        let prev_status = mol_data.status;

        if prev_status == MoleculeStatus::Collapsed {
            return Ok(prev_status);
        }
        if prev_status == MoleculeStatus::Completed {
            anyhow::bail!("molecule {mol_id} is completed — cannot collapse a completed molecule");
        }

        let mut updated = mol_data;
        updated.status = MoleculeStatus::Collapsed;
        updated.collapse_reason = Some(reason.to_owned());
        updated.collapse_reason_kind.clone_from(&kind);
        updated.collapsed_step = Some(updated.current_step);
        if updated.process.is_some() {
            updated.release_process();
        }
        updated.updated_at = Utc::now();
        store.save_molecule(&updated.id.clone(), &updated)?;
        prev_status
    };

    // Append to log.md (best-effort — the transition already persisted).
    let mol_dir = store.molecule_dir(mol_id);
    let log_path = mol_dir.join("log.md");
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let log_entry = format!("\n## {timestamp} — Collapsed\n\n{reason}\n");
    let existing_log = fs::read_to_string(&log_path).unwrap_or_default();
    let new_log = if existing_log.is_empty() {
        format!("# Evolution Log\n{log_entry}")
    } else {
        format!("{existing_log}{log_entry}")
    };
    let _ = fs::write(&log_path, new_log);

    // Emit legacy + EventV2 collapse records.
    let events_path = ops_dir.join("events.jsonl");
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeTransitioned {
            molecule_id: mol_id.clone(),
            from: prev_status,
            to: MoleculeStatus::Collapsed,
        }),
    );
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason: reason.to_owned(),
        }),
    );
    let status_seq = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol_id.clone(),
            from: prev_status.to_string(),
            to: "collapsed".to_owned(),
        },
        None,
    )
    .ok();
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason: reason.to_owned(),
            kind,
        },
        status_seq,
    );

    Ok(prev_status)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};

    use super::Context;
    use super::{run, Args, ConditionArgs};

    /// Write a project `config.toml` with `[archive] enabled = true`
    /// at the parent of the state dir — i.e. in the `.cosmon/` layout
    /// the CLI walks up to find.
    fn enable_archive_config(state_dir: &std::path::Path) {
        let cosmon_dir = state_dir.parent().unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n\n[archive]\nenabled = true\n",
        )
        .unwrap();
    }

    fn mol(id: &str) -> MoleculeData {
        let now = chrono::Utc::now();
        MoleculeData {
            harvest_reason: None,
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 1,
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
            merged_at: None,
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

    #[test]
    fn collapse_one_flips_running_to_collapsed_and_records_reason() {
        // The programmatic collapse helper the in-process Direct-API dispatch
        // uses when a loop performed zero work. A Running molecule must become
        // Collapsed, carry the reason, and record the step it collapsed at.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260415-col9"); // Running, current_step = 1
        store.save_molecule(&m.id, &m).unwrap();

        let prev = super::collapse_one(&store, &state_dir, &m.id, "zero-tool loop").unwrap();
        assert_eq!(prev, MoleculeStatus::Running);

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        assert_eq!(reloaded.collapse_reason.as_deref(), Some("zero-tool loop"));
        assert_eq!(reloaded.collapsed_step, Some(1));

        // Idempotent on an already-collapsed molecule.
        let prev2 = super::collapse_one(&store, &state_dir, &m.id, "again").unwrap();
        assert_eq!(prev2, MoleculeStatus::Collapsed);
    }

    #[test]
    fn collapse_one_refuses_a_completed_molecule() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        let mut m = mol("task-20260415-col10");
        m.status = MoleculeStatus::Completed;
        store.save_molecule(&m.id, &m).unwrap();

        let err = super::collapse_one(&store, &state_dir, &m.id, "nope").unwrap_err();
        assert!(err.to_string().contains("completed"));
    }

    #[test]
    fn collapse_archives_and_sets_archived_flag() {
        // Fresh state dir, archive enabled, molecule running. A first
        // `cs collapse` must write the archive entry, flip `archived` to
        // true in state.json, and leave the archive root on disk.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        enable_archive_config(&state_dir);

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260415-col1");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-col1".to_owned(),
            reason: Some("test blocker".to_owned()),
            with_seats: false,
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
            condition: ConditionArgs::default(),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert!(
            reloaded.archived,
            "archived flag should be true after first collapse"
        );
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);

        // Archive dir laid out YYYY/MM/<id>/molecule.json.
        let archive_root = state_dir.join("archive");
        assert!(
            archive_root.is_dir(),
            "archive root should exist: {archive_root:?}"
        );
    }

    #[test]
    fn collapse_is_idempotent_when_archived_flag_true() {
        // When the archived flag is already true on disk (from a prior
        // collapse), re-invoking collapse on the same molecule — after
        // resetting status for the test — must NOT re-write the archive.
        // We check this by pre-populating `archived = true` and a
        // sentinel file inside the archive entry; the sentinel must
        // survive a second `cs collapse`.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        enable_archive_config(&state_dir);

        let store = FileStore::new(&state_dir);
        let mut m = mol("task-20260415-col2");
        m.archived = true;
        store.save_molecule(&m.id, &m).unwrap();

        // Ensure the archive dir is absent — the write is skipped by the
        // idempotence gate, so after `cs collapse` runs the archive must
        // still be absent (cs collapse never creates state_dir/archive/
        // when archived is already true).
        let archive_root = state_dir.join("archive");
        assert!(!archive_root.exists());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-col2".to_owned(),
            reason: Some("replay".to_owned()),
            with_seats: false,
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
            condition: ConditionArgs::default(),
        };
        run(&ctx, &args).unwrap();

        assert!(
            !archive_root.exists(),
            "archive root must not be created when archived flag is already true"
        );
    }

    #[test]
    fn collapse_skips_archive_when_disabled() {
        // [archive] enabled = false, written explicitly since the
        // default flipped to true (issue #60). collapse must not touch
        // the archive directory even for a brand-new molecule.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n\n[archive]\nenabled = false\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260415-col3");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-col3".to_owned(),
            reason: Some("noop".to_owned()),
            with_seats: false,
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
            condition: ConditionArgs::default(),
        };
        run(&ctx, &args).unwrap();

        let archive_root = state_dir.join("archive");
        assert!(
            !archive_root.exists(),
            "archive disabled — archive root must not exist"
        );
        let reloaded = store.load_molecule(&m.id).unwrap();
        assert!(!reloaded.archived);
    }

    #[test]
    fn collapse_records_rate_limit_cause_with_account_and_kind() {
        // ADR-062 K3 fixture replay: cs collapse --cause rate_limit
        // --account you --kind max_rolling_5h must persist the
        // structured CollapseCause so cs peek can surface
        // GhostKind::QuotaExhausted.
        use cosmon_core::molecule::CollapseCause;

        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260421-k3xx");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260421-k3xx".to_owned(),
            reason: Some("Claude usage limit reached".to_owned()),
            with_seats: false,
            cause: Some("rate_limit".to_owned()),
            account: Some("you".to_owned()),
            kind: Some("max_rolling_5h".to_owned()),
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
            condition: ConditionArgs::default(),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        match reloaded.collapse_cause {
            Some(CollapseCause::RateLimit {
                account,
                kind_quota,
            }) => {
                assert_eq!(account.as_deref(), Some("you"));
                assert_eq!(kind_quota.as_deref(), Some("max_rolling_5h"));
            }
            other => panic!("expected RateLimit cause, got {other:?}"),
        }
    }

    #[test]
    fn collapse_preserves_nudge_count_for_post_mortem() {
        // delib-20260420-1b02 P2: a molecule that was nudged before
        // collapsing must carry that count through to `cs collapse`'s
        // output so a future audit formula can read it. The state-side
        // expectation is simply that `nudge_count` round-trips
        // unchanged across the collapse transition (the human/JSON
        // formatting is verified by the snapshot harness).
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let mut m = mol("task-20260420-nudg");
        m.nudge_count = 3;
        m.last_nudged_at = Some(chrono::Utc::now());
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260420-nudg".to_owned(),
            reason: Some("ambiguous briefing".to_owned()),
            with_seats: false,
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
            condition: ConditionArgs::default(),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        assert_eq!(
            reloaded.nudge_count, 3,
            "collapse must preserve nudge_count for post-mortem audit"
        );
    }

    #[test]
    fn collapse_rejects_account_without_rate_limit_cause() {
        // --account / --kind only make sense with --cause rate_limit.
        // Anything else is a user error caught at parse time.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260421-k3yy");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260421-k3yy".to_owned(),
            reason: Some("stalled".to_owned()),
            with_seats: false,
            cause: Some("manual".to_owned()),
            account: Some("you".to_owned()),
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
            condition: ConditionArgs::default(),
        };
        let err = run(&ctx, &args).unwrap_err();
        assert!(
            err.to_string().contains("--account / --kind"),
            "expected mismatch error, got: {err}"
        );
    }
}
