// SPDX-License-Identifier: AGPL-3.0-only

//! `cs land` — the harvest door of ADR-176, and the answer to issue #51.
//!
//! # What this verb is
//!
//! A tenant closes a molecule and its branch never reaches the trunk. The
//! closed list of ADR-080 §5.1 forbids `cs done` across the §8p boundary, so
//! the work piles up and the tenant has no gesture at all. That is issue #51.
//!
//! `cs land` is the gesture. It takes **one argument** — the molecule — and
//! nothing else. It is not `cs done` with fewer flags reachable by
//! convention: [`super::done::Args::sealed_door`] fixes every field at the
//! type, so there is no code path through this verb that varies a merge
//! strategy, waives a gate, or forces a deletion. ADR-176 D4 gives the reason
//! in one sentence — *a derogation requested by its beneficiary is not a
//! derogation*.
//!
//! # Two proofs, two keys
//!
//! Over the §8p route the JWT **authenticates the requester**; it carries no
//! authority of its own. The operator-sealed [`HarvestGrant`] **authorises the
//! effect**, and it is verified where the effect happens — inside the trunk
//! lock, against facts re-derived there (ADR-172 D3,
//! [`super::done_authority::authorize_harvest`]). This verb therefore refuses
//! outright in a galaxy that has not armed `[harvest_authority] required`:
//! without the second key the door would be a bearer token spending an
//! authority nobody granted.
//!
//! A local operator loses nothing by this. They still have `cs done`, which
//! this verb deliberately does not replace and cannot exceed.
//!
//! # Why the refusals are named, and why they are closed
//!
//! Nobody is reading the pane. A refusal with no name is a question asked of
//! an operator who is not there, which ADR-110 I4 forbids as a silent stall.
//! Every outcome of this verb is one of the seven
//! [`DoorRefusal`] variants or one of the two [`DoorOutcome`] ones, each with
//! a stable label and a stable exit code that the §8p route reads back rather
//! than parsing stderr.
//!
//! [`HarvestGrant`]: cosmon_core::harvest_authorization::HarvestGrant

use cosmon_core::config::ProjectConfig;
use cosmon_core::harvest_door::{reservation_requiring_seal, DoorOutcome, DoorRefusal};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{MoleculeFilter, NonIntegrationReason, StateStore};

use super::Context;

/// Arguments for the `land` subcommand.
///
/// One field. That is the decision, not an oversight: see the module header
/// and ADR-176 D4. A flag added here is a degree of freedom handed to the
/// principal the gates exist to bound, and
/// `the_sealed_door_arms_no_degree_of_freedom` in [`super::done`] fails the
/// moment one leaks into the argument set actually executed.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule to close and, where the second authority arises, integrate.
    pub molecule: String,
}

/// A refusal from the harvest door, carrying its stable exit code.
///
/// Returned as the outermost `anyhow::Error` so `main` can downcast it and
/// exit with the code the route reads. Mirrors the pattern already used by
/// [`super::guard::GuardError`] and `cmd::journal::SessionExit`.
#[derive(Debug, thiserror::Error)]
#[error("cs land refused ({}): {}{}", .refusal.as_str(), .refusal.message(), .detail.as_ref().map_or(String::new(), |d| format!(" — {d}")))]
pub struct RefusedHarvest {
    /// Which refusal fired.
    pub refusal: DoorRefusal,
    /// Operator-facing specifics: the conflicted files, the reservation tag,
    /// the backlog census. Never a raw stderr dump.
    pub detail: Option<String>,
}

impl RefusedHarvest {
    fn with(refusal: DoorRefusal, detail: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            refusal,
            detail: Some(detail.into()),
        })
    }
}

/// Recover the door's exit code from an error, if it is a door refusal.
#[must_use]
pub fn refusal_exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<RefusedHarvest>()
        .map(|r| r.refusal.exit_code())
}

/// Execute the `land` command.
///
/// The order of the checks is the order of the cost they avoid: the two that
/// read only the molecule come first, the backlog census next, and only then
/// does anything touch git.
///
/// # Errors
///
/// [`RefusedHarvest`] for every named refusal; a plain `anyhow` error for a
/// malformed id or an unreadable store, which are faults of the invocation
/// rather than outcomes of the door.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id = MoleculeId::new(&args.molecule)?;
    let store = ctx.store();
    let cfg = cosmon_filestore::load_project_config(&super::resolve_config_from_context(ctx))
        .unwrap_or_else(|_| ProjectConfig::default());

    // 1. The second key. A galaxy that has not armed harvest authority has
    //    granted nobody anything, and a door that proceeded anyway would be
    //    spending an authority that was never issued. Fail-closed, and first:
    //    it is the cheapest check and the one whose absence is a doctrine
    //    violation rather than a state fact.
    if !cfg.harvest_authority.is_required() {
        return Err(RefusedHarvest::with(
            DoorRefusal::NotAuthorized,
            "this galaxy has not armed `[harvest_authority] required`",
        ));
    }

    let mol = store.load_molecule(&mol_id)?;

    // 2. Idempotence, before admissibility: a harvest that already landed
    //    must report the same success on every retry, including a retry sent
    //    because the network ate the first response. `archived` is the
    //    disjunct that closes the no-branch molecule's loop — it never stamps
    //    `merged_at` because it has nothing to merge.
    if mol.merged_at.is_some() || mol.archived {
        report(ctx, &mol_id, DoorOutcome::AlreadyLanded, None);
        return Ok(());
    }

    // 3. Admissibility. `is_terminal()` is `Completed | Collapsed`, and only
    //    one of the two is work anyone asked to land — the ADR-176 §1 defect
    //    stated generally. The molecule's own `--if-completed` gate would
    //    also catch this, silently; naming it here is what makes the refusal
    //    readable to a requester who is not watching a terminal.
    if mol.status != MoleculeStatus::Completed {
        return Err(RefusedHarvest::with(
            DoorRefusal::NotCompleted,
            format!("status is {}", mol.status.as_str()),
        ));
    }

    // 4. Reservations. These name a condition somebody attached on purpose,
    //    and the door has no verdict to offer: the missing input is a human
    //    judgement. Checked here as well as at the effect boundary because a
    //    refusal the requester can read beats one buried in a merge log —
    //    the boundary check under the lock remains the load-bearing one,
    //    since a tag added after this line is still caught there.
    let tags: Vec<String> = mol.tags.iter().map(ToString::to_string).collect();
    if let Some(tag) = reservation_requiring_seal(&tags) {
        return Err(RefusedHarvest::with(
            DoorRefusal::ReservationRequiresSeal,
            format!("reserved by `{tag}`"),
        ));
    }

    // 5. The bounded queue (ADR-176 D7). Past the operator's sealed ceiling
    //    of closed-but-unintegrated molecules, further requests are refused.
    //    The ceiling is a field of the configuration, never a parameter: a
    //    queue the requester can lengthen is not a bound.
    let ceiling = cfg.harvest_authority.backlog_ceiling();
    let backlog = unintegrated_census(store.as_ref())?;
    if backlog >= ceiling as usize {
        return Err(RefusedHarvest::with(
            DoorRefusal::BacklogFull,
            format!("{backlog} closed-but-unintegrated molecules, ceiling {ceiling}"),
        ));
    }

    // 6. The effect. One argument set, fixed at the type. Everything the
    //    door may not vary is unreachable from here.
    let done_args = super::done::Args::sealed_door(args.molecule.clone());
    match super::done::run(ctx, &done_args) {
        Ok(()) => {
            // `cs done` succeeding is not by itself proof that the work
            // landed: `--if-completed` exits success on a no-op, and a
            // molecule with no branch archives without a merge. Re-read the
            // state and let the trunk-side record answer.
            let after = store.load_molecule(&mol_id)?;
            match after.non_integration.as_ref() {
                None if after.merged_at.is_some() || after.archived => {
                    report(ctx, &mol_id, DoorOutcome::Landed, None);
                    Ok(())
                }
                None => Err(RefusedHarvest::with(
                    DoorRefusal::NotCompleted,
                    "the harvest was a no-op and nothing was recorded",
                )),
                Some(record) => Err(refusal_from_record(record)),
            }
        }
        Err(err) => {
            // The refusal reason is read from the trunk-side record rather
            // than from the error text: `cs done` writes `non_integration`
            // under the lock on every failure path, and a string match on a
            // message is a mirror that drifts the first time someone edits
            // the message.
            let recorded = store
                .load_molecule(&mol_id)
                .ok()
                .and_then(|m| m.non_integration);
            match recorded {
                Some(record) => Err(refusal_from_record(&record)),
                None => Err(err),
            }
        }
    }
}

/// Map the trunk-side non-integration record to a door refusal.
///
/// The two mechanical reasons are the ones ADR-176 D7 calls an *execution
/// event* and a *configuration error*; `PreDoneRefused` is the *verdict*.
/// `NoBranch` and `MergeSkipped` cannot arise on this path — the door never
/// passes `--no-merge`, and a molecule with no branch archives successfully —
/// so they are mapped to the refusal whose recovery is the same (a human
/// looks) rather than given a variant that no request can produce.
fn refusal_from_record(record: &cosmon_state::NonIntegration) -> anyhow::Error {
    let refusal = match record.reason {
        NonIntegrationReason::Conflict => DoorRefusal::MergeConflict,
        NonIntegrationReason::MergeFailed => DoorRefusal::BaseNotFastForward,
        NonIntegrationReason::PreDoneRefused
        | NonIntegrationReason::NoBranch
        | NonIntegrationReason::MergeSkipped => DoorRefusal::PreDoneRefused,
    };
    let base = record
        .base_branch
        .as_ref()
        .map_or_else(String::new, |b| format!(" against {b}"));
    let extra = record
        .detail
        .as_ref()
        .map_or_else(String::new, |d| format!(": {d}"));
    RefusedHarvest::with(refusal, format!("{}{base}{extra}", record.reason.as_str()))
}

/// How many molecules in this kernel are closed but not integrated.
///
/// The census is the complement `merged_at.is_none()` restricted to
/// `Completed`, which is exactly the condition ADR-176 D7 bounds. Molecules
/// that never reached completion are not a debt: nobody is waiting on a
/// verdict for them.
fn unintegrated_census(store: &dyn StateStore) -> anyhow::Result<usize> {
    let filter = MoleculeFilter {
        status: Some(MoleculeStatus::Completed),
        ..MoleculeFilter::default()
    };
    Ok(store
        .list_molecules(&filter)?
        .into_iter()
        .filter(|m| m.merged_at.is_none() && m.non_integration.is_some())
        .count())
}

/// Print the door's success line, in JSON or for a human.
fn report(ctx: &Context, mol_id: &MoleculeId, outcome: DoorOutcome, detail: Option<&str>) {
    if ctx.json {
        let body = serde_json::json!({
            "molecule": mol_id.as_str(),
            "outcome": outcome.as_str(),
            "detail": detail,
        });
        println!("{body}");
    } else {
        println!("✓ {} — {}", mol_id.as_str(), outcome.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_carries_its_exit_code_through_anyhow() {
        // `main` recovers the code by downcast; if the error is wrapped in a
        // way that loses the type, every refusal exits 1 and the route can no
        // longer tell one from another.
        let err = RefusedHarvest::with(DoorRefusal::MergeConflict, "src/lib.rs");
        assert_eq!(
            refusal_exit_code(&err),
            Some(DoorRefusal::MergeConflict.exit_code())
        );
        assert!(err.to_string().contains("merge_conflict"));
        assert!(err.to_string().contains("src/lib.rs"));
    }

    #[test]
    fn a_non_door_error_has_no_door_exit_code() {
        let err = anyhow::anyhow!("some unrelated failure");
        assert_eq!(refusal_exit_code(&err), None);
    }

    #[test]
    fn a_refusal_with_no_detail_still_names_itself() {
        // The `detail` half is optional; the label never is. A refusal that
        // degraded to a bare message when it had nothing extra to say would
        // be unreadable exactly when the operator has least context.
        let err = anyhow::Error::new(RefusedHarvest {
            refusal: DoorRefusal::BacklogFull,
            detail: None,
        });
        assert_eq!(
            refusal_exit_code(&err),
            Some(DoorRefusal::BacklogFull.exit_code())
        );
        assert!(err.to_string().contains("backlog_full"));
    }

    #[test]
    fn every_non_integration_reason_maps_to_a_named_refusal() {
        // Exhaustive over the persisted partition, so a sixth reason added
        // to `cosmon-state` cannot reach the door as an unnamed failure.
        use NonIntegrationReason as R;
        let cases = [
            (R::Conflict, DoorRefusal::MergeConflict),
            (R::MergeFailed, DoorRefusal::BaseNotFastForward),
            (R::PreDoneRefused, DoorRefusal::PreDoneRefused),
            (R::NoBranch, DoorRefusal::PreDoneRefused),
            (R::MergeSkipped, DoorRefusal::PreDoneRefused),
        ];
        for (reason, expected) in cases {
            let record = cosmon_state::NonIntegration {
                reason,
                at: chrono::Utc::now(),
                base_branch: Some("main".to_owned()),
                detail: Some("two files".to_owned()),
            };
            let err = refusal_from_record(&record);
            assert_eq!(
                refusal_exit_code(&err),
                Some(expected.exit_code()),
                "{reason:?} mapped to the wrong refusal",
            );
            assert!(err.to_string().contains("against main"));
        }
    }
}
