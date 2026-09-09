// SPDX-License-Identifier: AGPL-3.0-only

//! One-line provenance stamps for the CLI's keystroke injections
//! (COSMON #26 residual).
//!
//! Every `cs` command that writes into a worker's pane declares itself here
//! rather than at the transport call site. Two reasons, both about the next
//! reader:
//!
//! 1. **The census is greppable.** `rg 'injection_provenance::' crates/cosmon-cli`
//!    lists every place cosmon can put text in a composer. That list is the
//!    answer to issue #26's actual question — *what could have written this?* —
//!    and it should be one command, not an audit of an 8000-line module.
//! 2. **The ledger comes attached.** Each helper takes the molecule and its
//!    state directory, so a call site cannot accidentally emit an event with no
//!    log to land in. The
//!    [`cosmon_core::injection::InjectionOrigin`] alone would be attribution
//!    without a place to read it.
//!
//! A caller with no molecule in hand uses
//! [`InjectionProvenance::new`](cosmon_core::injection::InjectionProvenance::new)
//! directly; the seam then traces the injection without appending an event.

use std::path::Path;

use cosmon_core::id::MoleculeId;
use cosmon_core::injection::{InjectionLedger, InjectionOrigin, InjectionProvenance};

/// Build a ledger-bound provenance stamp.
///
/// The shared body of every helper below. Kept private so the vocabulary of
/// origins stays a closed list of named functions — a caller inventing its own
/// origin/purpose pair inline is exactly the drift that makes the census above
/// stop being complete.
fn stamped(
    origin: InjectionOrigin,
    purpose: &str,
    mol_id: &MoleculeId,
    mol_state_dir: &Path,
) -> InjectionProvenance {
    InjectionProvenance::new(origin, purpose)
        .with_ledger(InjectionLedger::new(mol_id.clone(), mol_state_dir))
}

/// `cs tackle` pasting a freshly-spawned worker its briefing.
#[must_use]
pub fn tackle_briefing(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::TackleBriefing,
        "briefing",
        mol_id,
        mol_state_dir,
    )
}

/// `cs tackle` re-pressing Enter on a briefing the composer still holds.
///
/// A bare submit, and the single most-repeated injection in a dispatch: the
/// confirmation loop fires it once per poll until the composer clears.
#[must_use]
pub fn tackle_briefing_submit(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::TackleBriefing,
        "briefing-submit",
        mol_id,
        mol_state_dir,
    )
}

/// `cs patrol` nudging a worker judged silent but alive.
#[must_use]
pub fn patrol_nudge(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(InjectionOrigin::PatrolNudge, "nudge", mol_id, mol_state_dir)
}

/// The bare submit that follows a patrol nudge.
#[must_use]
pub fn patrol_nudge_submit(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::PatrolNudge,
        "nudge-submit",
        mol_id,
        mol_state_dir,
    )
}

/// `cs patrol --heal` re-briefing a worker it re-attached to.
#[must_use]
pub fn patrol_heal(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::PatrolHeal,
        "rebrief",
        mol_id,
        mol_state_dir,
    )
}

/// The bare submit that follows a `--heal` re-brief.
#[must_use]
pub fn patrol_heal_submit(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::PatrolHeal,
        "rebrief-submit",
        mol_id,
        mol_state_dir,
    )
}

/// Propulsion — the periodic "keep going" signal to a running worker.
#[must_use]
pub fn propulsion(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(InjectionOrigin::Propulsion, "propel", mol_id, mol_state_dir)
}

/// The bare submit that follows a propulsion nudge.
#[must_use]
pub fn propulsion_submit(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::Propulsion,
        "propel-submit",
        mol_id,
        mol_state_dir,
    )
}

/// `cs thaw` handing a resumed molecule its continuation prompt.
#[must_use]
pub fn thaw(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(InjectionOrigin::Thaw, "thaw-prompt", mol_id, mol_state_dir)
}

/// `cs resume` restoring a worker after a session restart.
#[must_use]
pub fn resume(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::Resume,
        "resume-prompt",
        mol_id,
        mol_state_dir,
    )
}

/// The bare submit that follows a `cs resume` prompt.
#[must_use]
pub fn resume_submit(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::Resume,
        "resume-submit",
        mol_id,
        mol_state_dir,
    )
}

/// The durable briefing backstop pressing Enter from a process that outlived
/// the dispatcher (COSMON #26-B).
#[must_use]
pub fn briefing_backstop(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::BriefingBackstop,
        "backstop-submit",
        mol_id,
        mol_state_dir,
    )
}

/// `cs patrol`'s opt-in dialogue auto-confirm — a bare Enter accepting a TUI
/// permission prompt's highlighted default.
///
/// The narrowest and most alarming injection cosmon makes: it answers a
/// question addressed to a human. It gets its own origin so a later audit can
/// count them without inferring intent from an empty input.
#[must_use]
pub fn dialogue_auto_confirm(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(
        InjectionOrigin::DialogueAutoConfirm,
        "dialogue-auto-confirm",
        mol_id,
        mol_state_dir,
    )
}

/// `cs whisper` — operator-authored text sent to a live worker.
#[must_use]
pub fn whisper(mol_id: &MoleculeId, mol_state_dir: &Path) -> InjectionProvenance {
    stamped(InjectionOrigin::Whisper, "whisper", mol_id, mol_state_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260731-f0ab").unwrap()
    }

    #[test]
    fn every_helper_carries_a_ledger() {
        let dir = Path::new("/tmp/does-not-need-to-exist");
        let all = [
            tackle_briefing(&mol(), dir),
            tackle_briefing_submit(&mol(), dir),
            patrol_nudge(&mol(), dir),
            patrol_nudge_submit(&mol(), dir),
            patrol_heal(&mol(), dir),
            patrol_heal_submit(&mol(), dir),
            propulsion(&mol(), dir),
            propulsion_submit(&mol(), dir),
            thaw(&mol(), dir),
            resume(&mol(), dir),
            resume_submit(&mol(), dir),
            briefing_backstop(&mol(), dir),
            dialogue_auto_confirm(&mol(), dir),
            whisper(&mol(), dir),
        ];
        for p in &all {
            let ledger = p.ledger.as_ref().expect("helper binds a ledger");
            assert_eq!(ledger.mol_id, mol());
            assert_eq!(ledger.state_dir(), dir);
            assert!(!p.purpose.is_empty(), "purpose must say something");
            assert_ne!(
                p.origin,
                InjectionOrigin::Unattributed,
                "a named helper is by definition attributed",
            );
        }
    }
}
