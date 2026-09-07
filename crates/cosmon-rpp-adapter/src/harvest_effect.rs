// SPDX-License-Identifier: AGPL-3.0-only

//! The effect half of the harvest door — issue #51, ADR-176 §11.
//!
//! # What this module is for
//!
//! The door has a decision half and an effect half. The decision half is a
//! library ([`cosmon_filestore::harvest_door::decide`]) and answers every
//! pre-effect refusal in-process. The effect half is the sealed harvest
//! transaction — merge with lineage trailers, publish/identity/
//! confidentiality gates, the `pre_done` gate, the teardown — and it has
//! exactly one implementation in this repository: `cmd/done.rs`. Rewriting
//! it here would fork the door, which is the failure the shared
//! [`DoorRefusal`](cosmon_core::harvest_door::DoorRefusal) vocabulary exists
//! to prevent.
//!
//! So the effect is a **port**, and the deployment chooses an
//! implementation:
//!
//! - [`UnavailableHarvestEffect`] — the default and the honest answer for an
//!   image that carries no `cs`. The route refuses `harvest_effect_unavailable`
//!   rather than pretending, and every pre-effect refusal still answers in
//!   full.
//! - [`CsBinaryHarvestEffect`] — the operator declared a `cs` binary in
//!   `rpp.toml`. The harvest runs as that binary, with the argv
//!   [`HarvestOptions::cs_done_argv`] builds, in the tenant's own galaxy
//!   root.
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

/// The default: no effect implementation in this deployment.
///
/// Answers [`HarvestEffectError::Unavailable`] for every harvest the
/// decision half admits. Fail-honest rather than fail-open: the alternative
/// an adapter reaches for under pressure is a `202`, and a `202` on a
/// transaction that may integrate nothing is exactly the defect issue #51
/// reported.
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
}
