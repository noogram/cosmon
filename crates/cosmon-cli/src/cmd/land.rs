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
//! # Where the body lives
//!
//! Since issue #54 U3, the door's body — the ordered refusal checks, the
//! trunk-lock discipline, the post-effect interpretation — is the library
//! entry [`cosmon_filestore::harvest_door::land`], shared with the §8p
//! route so the two doors cannot drift. This module keeps only what is
//! CLI-shaped: argument parsing, the sealed `cs done` transaction as the
//! injected [`SealedHarvestEffect`], and the mapping of the library's typed
//! refusals onto the stable exit codes 70–76 that `main` reads back.
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
//! [`SealedHarvestEffect`]: cosmon_filestore::harvest_door::SealedHarvestEffect

use cosmon_core::config::ProjectConfig;
use cosmon_core::harvest_door::{DoorOutcome, DoorRefusal};
use cosmon_core::id::MoleculeId;
use cosmon_filestore::harvest_door::{self, LandError, SealedHarvestEffect};

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

/// Recover the door's exit code from an error, if it is a door refusal.
#[must_use]
pub fn refusal_exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<RefusedHarvest>()
        .map(|r| r.refusal.exit_code())
}

/// The sealed `cs done` transaction as the door's effect half.
///
/// [`super::done::Args::sealed_door`] fixes every field at the type; this
/// wrapper adds nothing to that argument set, it only adapts the call to the
/// [`SealedHarvestEffect`] port. It declares `binds_trunk_lock` because the
/// `cs done` path acquires the trunk flock at its own effect boundary
/// (ADR-172 D3) — the door holding it too would deadlock, not serialize
/// (see the port's docs and the ADR-176 §-amendment).
struct SealedDoneEffect<'a> {
    ctx: &'a Context,
}

impl SealedHarvestEffect for SealedDoneEffect<'_> {
    fn binds_trunk_lock(&self) -> bool {
        true
    }

    fn harvest(&mut self, molecule: &MoleculeId) -> Result<(), String> {
        let done_args = super::done::Args::sealed_door(molecule.as_str().to_owned());
        super::done::run(self.ctx, &done_args).map_err(|e| format!("{e:#}"))
    }
}

/// Execute the `land` command.
///
/// The decision half, the trunk-lock discipline and the post-effect
/// interpretation live in [`cosmon_filestore::harvest_door::land`]; this
/// function injects the sealed `cs done` transaction as the effect and maps
/// the typed result onto the CLI's rendering and exit codes.
///
/// # Errors
///
/// [`RefusedHarvest`] for every named refusal; a plain `anyhow` error for a
/// malformed id, an unreadable store, or an effect failure that recorded
/// nothing — faults of the invocation rather than outcomes of the door.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id = MoleculeId::new(&args.molecule)?;
    let store = ctx.store();
    let cfg = cosmon_filestore::load_project_config(&super::resolve_config_from_context(ctx))
        .unwrap_or_else(|_| ProjectConfig::default());

    let mut effect = SealedDoneEffect { ctx };
    match harvest_door::land(store.as_ref(), &cfg, &mol_id, &mut effect) {
        Ok(outcome) => {
            report(ctx, &mol_id, outcome, None);
            Ok(())
        }
        Err(LandError::Refused(refused)) => Err(anyhow::Error::new(RefusedHarvest {
            refusal: refused.refusal,
            detail: refused.detail,
        })),
        Err(LandError::Fault(e)) => Err(e.into()),
        Err(LandError::EffectFailed(message)) => Err(anyhow::anyhow!("{message}")),
    }
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

    fn refused(refusal: DoorRefusal, detail: &str) -> anyhow::Error {
        anyhow::Error::new(RefusedHarvest {
            refusal,
            detail: Some(detail.to_owned()),
        })
    }

    #[test]
    fn a_refusal_carries_its_exit_code_through_anyhow() {
        // `main` recovers the code by downcast; if the error is wrapped in a
        // way that loses the type, every refusal exits 1 and the route can no
        // longer tell one from another.
        let err = refused(DoorRefusal::MergeConflict, "src/lib.rs");
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
    fn every_library_refusal_keeps_its_exit_code_through_the_cli_mapping() {
        // The library speaks `DoorRefused`; the CLI speaks exit codes. This
        // walks the closed set through the same conversion `run` performs,
        // so a refusal cannot lose its code between the two vocabularies.
        for refusal in cosmon_core::harvest_door::ALL_REFUSALS {
            let refused = cosmon_filestore::harvest_door::DoorRefused {
                refusal: *refusal,
                detail: None,
            };
            let err = anyhow::Error::new(RefusedHarvest {
                refusal: refused.refusal,
                detail: refused.detail,
            });
            assert_eq!(refusal_exit_code(&err), Some(refusal.exit_code()));
        }
    }
}
