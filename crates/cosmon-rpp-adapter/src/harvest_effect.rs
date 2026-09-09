// SPDX-License-Identifier: AGPL-3.0-only

//! The effect half of the harvest door — issue #51, ADR-176 §11.
//!
//! # What this module is for
//!
//! The door has a decision half and an effect half. The decision half is a
//! library ([`cosmon_filestore::harvest_door::decide`]) and answers every
//! pre-effect refusal in-process. The effect half is the sealed harvest
//! transaction — merge with lineage trailers, publish/identity/
//! confidentiality gates, the `pre_done` gate, the teardown — and it still
//! has exactly **one** implementation. What changed is where that one
//! implementation lives: it was ~6 000 lines private to the `cs` binary
//! (`cmd/done.rs`), and it is now the [`cosmon_harvest`] crate, which this
//! adapter can call. Rewriting it here would fork the door, which is the
//! failure the shared [`DoorRefusal`] vocabulary exists to prevent; calling
//! it is not a fork.
//!
//! The effect stays a **port**, because the deployment still chooses:
//!
//! - [`LibraryHarvestEffect`] — **the default**. The transaction runs
//!   in-process, in the tenant's own galaxy, through the same
//!   [`cosmon_harvest::run`] `cs done` calls with the same [`Args`]. No `cs`
//!   process is spawned and no configuration line is needed: an armed galaxy
//!   (`[harvest_authority]`) is harvestable on a stock image.
//! - [`CsBinaryHarvestEffect`] — the operator declared a `cs` binary in
//!   `rpp.toml`. The harvest runs as that binary, with the argv
//!   [`HarvestOptions::cs_done_argv`] builds, in the tenant's own galaxy
//!   root. Kept for one release as the operator's escape hatch — a
//!   deployment that wants the harvest to run as a *specific* build of `cs`
//!   rather than as the one compiled into this adapter — and documented as
//!   such in `rpp.toml`.
//! - [`UnavailableHarvestEffect`] — the honest `501
//!   harvest_effect_unavailable`, kept because the route must still have an
//!   answer for a port with no implementation, and because the tests that
//!   pin that answer must be able to select it. No shipped configuration
//!   selects it any more.
//!
//! [`Args`]: cosmon_harvest::Args
//!
//! # Why a `cs` child is legitimate here, and was not before
//!
//! ADR-080 §5.1 classed `done` as an operator-only verb, and §3.5 gave that
//! two locks: the adapter refuses to route to one, and `cs` refuses to run
//! one under `COSMON_API_REQUEST=1`. A `cs done` child of a request was
//! therefore refused by construction, which is why the harvest could not
//! reach its effect at all.
//!
//! The §5.1 amendment of issue #51 takes `done` off that list — closing a
//! molecule is lifecycle, not administration — by the same §5.2 successor
//! path `run` used in ADR-124. With `done` off the list, a child that
//! performs it is an ordinary local gesture, and it announces itself as one:
//! [`cosmon_core::api_envelope::hand_off_to_local_child`] consumes the
//! request marker, exactly as the resident drain's own `cs done` teardown
//! already does. **No security posture is consumed**: the egress variables
//! and the exposed-host refusal are untouched by that call.
//!
//! What this deliberately does *not* restore is the general §3.5 clause (e)
//! subprocess envelope retired by issue #54 U6. This is one port, one verb,
//! one operator-declared binary, off by default.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::harvest_door::{DoorRefusal, HarvestOptions};
use cosmon_core::id::MoleculeId;

/// The message an unavailable effect crosses the door's string-typed seam
/// as.
///
/// The door's [`SealedHarvestEffect`](cosmon_filestore::harvest_door::SealedHarvestEffect)
/// reports failure as a `String`, deliberately: it re-reads the trunk-side
/// record and derives the named refusal from *that*, never from error text.
/// "No effect exists" is the one outcome that record cannot express — no
/// effect ran, so nothing was written — so the route has to recognise it,
/// and it does so against this constant rather than against a sentence
/// somebody may reword.
pub const UNAVAILABLE_MARKER: &str = "cosmon::harvest_effect::unavailable";

/// Why a harvest effect did not run, or did not complete.
#[derive(Debug)]
pub enum HarvestEffectError {
    /// No implementation is wired in this deployment. A typed refusal, not
    /// a failure: the door admitted the harvest and the server cannot
    /// perform it, which the requester must be told plainly rather than
    /// discovering through a success that integrated nothing.
    Unavailable,
    /// The effect ran and failed. The string is the implementation's own
    /// message; the door re-reads the trunk-side record and derives the
    /// named refusal from *that* rather than from this text.
    Failed(String),
    /// The effect exited with one of the door's stable refusal codes
    /// (70–77). Carried as the named refusal so the route answers with the
    /// label the requester can act on.
    Refused(DoorRefusal),
}

/// The effect half of the door, as a port.
///
/// `Send + Sync` because the implementation is held in
/// [`crate::AppState`] and shared across every request thread.
pub trait HarvestEffectPort: Send + Sync + std::fmt::Debug {
    /// Perform the sealed harvest of `molecule` in `tenant_root`, with the
    /// requester's options.
    ///
    /// # Errors
    ///
    /// [`HarvestEffectError`] — see its variants. An implementation that
    /// cannot run at all returns [`HarvestEffectError::Unavailable`], which
    /// the route maps to `501`, never to a success.
    fn harvest(
        &self,
        tenant_root: &Path,
        molecule: &MoleculeId,
        options: &HarvestOptions,
    ) -> Result<(), HarvestEffectError>;

    /// Whether this effect acquires the trunk flock at its own effect
    /// boundary.
    ///
    /// `true` for the `cs done` transaction in both of its shapes — it
    /// flocks before its first git mutation (ADR-172 D3) — and the door
    /// then must not hold the lock across the call, because `flock(2)` does
    /// not nest and holding it would deadlock rather than serialize.
    fn binds_trunk_lock(&self) -> bool;
}

/// No effect implementation in this deployment.
///
/// Answers [`HarvestEffectError::Unavailable`] for every harvest the
/// decision half admits. Fail-honest rather than fail-open: the alternative
/// an adapter reaches for under pressure is a `202`, and a `202` on a
/// transaction that may integrate nothing is exactly the defect issue #51
/// reported.
///
/// It was the default while the transaction was locked inside the `cs`
/// binary. It is no longer selected by any configuration — the library
/// implementation is always compiled in — and survives as the route's
/// answer for a port with no implementation, and as the double the tests
/// that pin that answer construct.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableHarvestEffect;

impl HarvestEffectPort for UnavailableHarvestEffect {
    fn harvest(
        &self,
        _tenant_root: &Path,
        _molecule: &MoleculeId,
        _options: &HarvestOptions,
    ) -> Result<(), HarvestEffectError> {
        Err(HarvestEffectError::Unavailable)
    }

    fn binds_trunk_lock(&self) -> bool {
        // Nothing runs, so nothing is serialized; the answer keeps the
        // door from taking a lock it would only release again.
        true
    }
}

/// The harvest as the operator's own `cs` binary, run in the tenant's
/// galaxy root.
///
/// The binary is **operator-declared** (`harvest_cs_binary` in `rpp.toml`)
/// and there is no PATH fallback. A door that discovered its own executor
/// would change behaviour when someone else's `cs` appeared on the host's
/// PATH, which is a deployment fact no operator reviewed.
#[derive(Debug, Clone)]
pub struct CsBinaryHarvestEffect {
    /// Absolute path to the `cs` binary the operator declared.
    binary: PathBuf,
}

impl CsBinaryHarvestEffect {
    /// Wire the effect to an operator-declared `cs` binary.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl HarvestEffectPort for CsBinaryHarvestEffect {
    fn harvest(
        &self,
        tenant_root: &Path,
        molecule: &MoleculeId,
        options: &HarvestOptions,
    ) -> Result<(), HarvestEffectError> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(options.cs_done_argv(molecule.as_str()))
            .current_dir(tenant_root);
        // The child is a local gesture downstream of a request, not the
        // request itself — the same hand-off the resident drain performs
        // before its own `cs done`. Security posture is not consumed here.
        cosmon_core::api_envelope::hand_off_to_local_child(&mut cmd);

        let output = cmd
            .output()
            .map_err(|e| HarvestEffectError::Failed(format!("spawning `cs done` failed: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        match output.status.code().and_then(DoorRefusal::from_exit_code) {
            Some(refusal) => Err(HarvestEffectError::Refused(refusal)),
            None => Err(HarvestEffectError::Failed(format!(
                "`cs done` exited with {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |c| c.to_string())
            ))),
        }
    }

    fn binds_trunk_lock(&self) -> bool {
        true
    }
}

/// The harvest as a library call, in-process.
///
/// # Why this is the default, and why it is not a second door
///
/// This is the same transaction `cs done` performs, reached through the same
/// entry point: [`cosmon_harvest::run`], with an [`Args`](cosmon_harvest::Args)
/// built by [`Args::from_harvest_options`](cosmon_harvest::Args::from_harvest_options)
/// — the constructor the previous unit left here for exactly this. The merge
/// and its lineage trailers, the publish / identity / confidentiality gates,
/// the `pre_done` gate, the tmux / worktree / branch teardown and the
/// `decide_branch_delete` invariant (D3: a closure that did not merge never
/// deletes the branch) are not re-implemented, re-ordered or re-decided here.
/// There is one implementation and two callers, which is what §12's follow-up
/// asked for.
///
/// # What it needs that a CLI gets for free
///
/// A `cs` invocation stands *in* the galaxy it acts on, so it can read its
/// own working directory for both the state store and the git checkout. A
/// request handler stands nowhere: it is told which tenant to act for. So the
/// context is built with [`cosmon_harvest::HarvestContext::at`], naming both
/// halves explicitly — `<tenant>/.cosmon/state` and the tenant root — and the
/// library's last-resort "the repository containing the current directory"
/// answer is never reached. Without that, a harvest would resolve the
/// *adapter's* own checkout and merge a tenant's branch into it.
#[derive(Debug, Default, Clone, Copy)]
pub struct LibraryHarvestEffect;

impl HarvestEffectPort for LibraryHarvestEffect {
    fn harvest(
        &self,
        tenant_root: &Path,
        molecule: &MoleculeId,
        options: &HarvestOptions,
    ) -> Result<(), HarvestEffectError> {
        let ctx = cosmon_harvest::HarvestContext::at(
            tenant_root.join(".cosmon").join("state"),
            tenant_root,
        );
        let args =
            cosmon_harvest::Args::from_harvest_options(molecule.as_str().to_owned(), options);
        match cosmon_harvest::run(&ctx, &args) {
            Ok(()) => Ok(()),
            Err(err) => Err(harvest_error_from(&err)),
        }
    }

    fn binds_trunk_lock(&self) -> bool {
        // The transaction flocks the trunk at its own first git mutation
        // (ADR-172 D3) — the same code, so the same answer as the `cs` child.
        // `flock(2)` does not nest, so the door must not hold it across this
        // call.
        true
    }
}

/// Classify an error out of [`cosmon_harvest::run`].
///
/// A door refusal keeps its name — that is the whole reason
/// [`cosmon_harvest::RefusedHarvest`] is a typed error and not a formatted
/// string — so the route answers `merge_conflict` or `pre_done_refused` and
/// not an anonymous `harvest_failed`. Anything else is a genuine fault and
/// travels as its own message.
fn harvest_error_from(err: &anyhow::Error) -> HarvestEffectError {
    err.downcast_ref::<cosmon_harvest::RefusedHarvest>()
        .map_or_else(
            || HarvestEffectError::Failed(format!("{err:#}")),
            |refused| HarvestEffectError::Refused(refused.refusal),
        )
}

/// Choose the effect implementation this deployment runs with.
///
/// One function rather than a `match` in `main`, because the *default* is the
/// decision worth pinning: `harvest_cs_binary` absent must mean the library,
/// not a refusal. It meant a refusal while the transaction was locked inside
/// the `cs` binary, and that was the defect issue #51 reported through
/// `POST /v1/molecules/{id}/done` — "capable end to end, with one config
/// line". A `main`-local match is a decision no test can reach.
#[must_use]
pub fn from_config(harvest_cs_binary: Option<PathBuf>) -> std::sync::Arc<dyn HarvestEffectPort> {
    match harvest_cs_binary {
        Some(binary) => std::sync::Arc::new(CsBinaryHarvestEffect::new(binary)),
        None => std::sync::Arc::new(LibraryHarvestEffect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_effect_refuses_instead_of_pretending() {
        let err = UnavailableHarvestEffect
            .harvest(
                Path::new("/nonexistent"),
                &MoleculeId::new("task-20260101-abcd").unwrap(),
                &HarvestOptions::new("close it"),
            )
            .unwrap_err();
        assert!(matches!(err, HarvestEffectError::Unavailable));
    }

    /// The falsifier for "the parameter reaches the effect": the argv the
    /// `cs` child is spawned with is the one the requester's options build,
    /// including a non-default strategy and the caller's own reason.
    #[test]
    fn the_cs_child_is_spawned_with_the_requested_options() {
        let mut options = HarvestOptions::new("the spike answered its question");
        options.strategy = cosmon_core::harvest_door::MergeStrategy::FfOnly;
        options.force = true;

        let argv = options.cs_done_argv("task-20260101-abcd");
        assert_eq!(argv[0], "done");
        assert_eq!(argv[1], "task-20260101-abcd");
        let strategy_at = argv.iter().position(|a| a == "--strategy").unwrap();
        assert_eq!(argv[strategy_at + 1], "ff-only");
        let reason_at = argv.iter().position(|a| a == "--reason").unwrap();
        assert_eq!(argv[reason_at + 1], "the spike answered its question");
        assert!(argv.iter().any(|a| a == "--force"));
    }

    #[test]
    fn a_bare_harvest_passes_no_strategy_and_gets_the_documented_default() {
        let argv = HarvestOptions::new("close it").cs_done_argv("task-20260101-abcd");
        assert!(
            !argv.iter().any(|a| a == "--strategy"),
            "a bare harvest must not pin a strategy; `cs done`'s own default is the contract"
        );
    }

    /// The default this molecule exists to change: a deployment that
    /// declares nothing harvests through the library.
    ///
    /// Goes red the moment someone restores `UnavailableHarvestEffect` as the
    /// default, which is what made `POST …/done` answer `501` on a stock
    /// image (ADR-176 §12) and what the 90-day clause on ADR-095's amendment
    /// was counting down.
    #[test]
    fn a_deployment_that_declares_nothing_harvests_through_the_library() {
        let effect = from_config(None);
        assert_eq!(
            format!("{effect:?}"),
            "LibraryHarvestEffect",
            "the default must be the in-process transaction, not a refusal \
             and not a subprocess"
        );
        assert!(
            effect.binds_trunk_lock(),
            "the transaction flocks the trunk itself, so the door must not \
             hold the lock across the call"
        );
    }

    /// The escape hatch still works, and still needs a declaration.
    #[test]
    fn a_declared_binary_still_selects_the_subprocess_effect() {
        let effect = from_config(Some(PathBuf::from("/opt/cosmon/bin/cs")));
        assert!(
            format!("{effect:?}").starts_with("CsBinaryHarvestEffect"),
            "a declared `harvest_cs_binary` must still run as that binary"
        );
    }

    /// Every one of the eight door refusals survives the library effect's
    /// error classification with its own name.
    ///
    /// The CLI recovers them from an exit code
    /// (`cosmon_harvest::refusal_exit_code`); the library caller recovers
    /// them from the typed error. If this collapsed to
    /// [`HarvestEffectError::Failed`], the route would answer an anonymous
    /// `harvest_failed` where the CLI answers exit 74 — the same transaction
    /// telling two callers different things, which is the drift the shared
    /// vocabulary exists to prevent.
    #[test]
    fn every_door_refusal_keeps_its_name_through_the_library_effect() {
        for &refusal in cosmon_core::harvest_door::ALL_REFUSALS {
            let err: anyhow::Error = cosmon_harvest::RefusedHarvest {
                refusal,
                detail: None,
            }
            .into();
            match harvest_error_from(&err) {
                HarvestEffectError::Refused(seen) => {
                    assert_eq!(seen, refusal, "{} must survive as itself", refusal.as_str())
                }
                other => panic!("{} collapsed to {other:?}", refusal.as_str()),
            }
            // And the CLI half of the same claim, on the same value: the
            // exit code a script branches on.
            assert_eq!(
                cosmon_harvest::refusal_exit_code(&err),
                Some(refusal.exit_code()),
                "{} must keep its exit code for the CLI caller",
                refusal.as_str()
            );
        }
    }

    /// A genuine fault is NOT dressed up as a refusal.
    #[test]
    fn a_fault_that_is_not_a_door_refusal_stays_a_failure() {
        let err = anyhow::anyhow!("the disk went away mid-merge");
        assert!(matches!(
            harvest_error_from(&err),
            HarvestEffectError::Failed(_)
        ));
    }
}
