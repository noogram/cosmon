// SPDX-License-Identifier: AGPL-3.0-only

//! Harvest belongs to the pilot: a worker may not `cs done` its own molecule
//! (issue #109).
//!
//! # Why
//!
//! `cs done` merges a worker's branch into the trunk. The worker is the
//! author of that branch, so a worker harvesting itself is an author merging
//! its own change with nobody between the change and the trunk. That is what
//! happened in issue #109: a worker finished, ran `cs done` on its own
//! molecule from its own pane, and landed a `.cosmon/config.toml` change.
//! ADR-080 §5.1 already calls `done` operator-only for remote tenants; this
//! is the same rule for the local worker.
//!
//! # How a worker is recognised
//!
//! By the identity `cs tackle` injects into every worker it spawns
//! ([`cosmon_core::pilot_env::PilotVar`]): `COSMON_PARENT_MOL_ID` names the
//! molecule the worker executes, and `COSMON_MOL_DIR` is that molecule's state
//! directory, whose last component is its id. Either one naming the target
//! refuses. The refusal is keyed on *self*: a worker session that drives a
//! DAG — the resident runtime's `cs done <child>` — harvests another molecule
//! and is not refused.
//!
//! This is a guard against a worker following the wrong habit, not a sandbox:
//! a process that scrubs its own environment is outside it, as it is outside
//! every other env-keyed guard.

use std::path::Path;

use cosmon_core::id::MoleculeId;
use cosmon_core::pilot_env::PilotVar;

/// A `cs done` refused because it runs inside the worker session of the
/// molecule it would harvest.
#[derive(Debug, thiserror::Error)]
#[error(
    "cs done {mol_id}: refusing — this is the worker session of {mol_id} \
     ({var} names it), and harvest belongs to the pilot.\n\n\
     A worker's branch reaches the trunk only after the pilot reviews it. \
     Finish with `cs complete {mol_id} --reason \"<summary>\"`; the pilot \
     then runs `cs done {mol_id}` from its own session."
)]
pub struct WorkerSelfHarvest {
    /// The molecule the worker tried to harvest — its own.
    pub mol_id: String,
    /// The injected variable that identified the worker, so the operator can
    /// see why the session was read as a worker.
    pub var: &'static str,
}

/// Refuse a harvest of `target` issued from the worker session of `target`.
///
/// `env_lookup` is the process environment in production
/// ([`crate::pilot_gesture::process_env`]) and a fixture in tests.
///
/// # Errors
///
/// [`WorkerSelfHarvest`] when the environment identifies this process as the
/// worker of `target`.
pub fn refuse_worker_self_harvest<F>(target: &MoleculeId, env_lookup: &F) -> anyhow::Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    let names_target = |var: PilotVar| -> bool {
        env_lookup(var.name()).is_some_and(|raw| {
            let raw = raw.trim();
            let own = match var {
                PilotVar::MolDir => Path::new(raw)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default(),
                _ => raw,
            };
            own == target.as_str()
        })
    };
    match [PilotVar::ParentMolId, PilotVar::MolDir]
        .into_iter()
        .find(|v| names_target(*v))
    {
        None => Ok(()),
        Some(var) => Err(WorkerSelfHarvest {
            mol_id: target.as_str().to_owned(),
            var: var.name(),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> MoleculeId {
        MoleculeId::new("task-20260928-dca1").unwrap()
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn the_pilot_s_environment_harvests() {
        assert!(refuse_worker_self_harvest(&target(), &env(&[])).is_ok());
    }

    #[test]
    fn the_worker_of_the_target_is_refused_by_either_variable() {
        let by_id = env(&[("COSMON_PARENT_MOL_ID", "task-20260928-dca1")]);
        let by_dir = env(&[(
            "COSMON_MOL_DIR",
            "/state/fleets/default/molecules/task-20260928-dca1",
        )]);
        for lookup in [&by_id as &dyn Fn(&str) -> Option<String>, &by_dir] {
            let err = refuse_worker_self_harvest(&target(), &lookup).unwrap_err();
            let text = err.to_string();
            assert!(text.contains("harvest belongs to the pilot"), "{text}");
            assert!(text.contains("cs complete task-20260928-dca1"), "{text}");
        }
    }

    #[test]
    fn a_worker_harvesting_another_molecule_is_not_self_harvest() {
        // The resident runtime's `cs done <child>` from a worker session.
        let lookup = env(&[
            ("COSMON_PARENT_MOL_ID", "task-20260928-0001"),
            (
                "COSMON_MOL_DIR",
                "/state/fleets/default/molecules/task-20260928-0001",
            ),
        ]);
        assert!(refuse_worker_self_harvest(&target(), &lookup).is_ok());
    }
}
