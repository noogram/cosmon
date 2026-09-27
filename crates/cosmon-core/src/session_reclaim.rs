// SPDX-License-Identifier: AGPL-3.0-only

//! Session reclaim — the ADR-178 doctrine, carried onto tmux sessions.
//!
//! A tmux session outlives its molecule. `cs purge`'s sweep already noticed
//! the case — "tmux alive but `current_molecule` is Completed/Collapsed" — and
//! deliberately removed only the fleet entry, leaving the session up for an
//! operator to judge. That choice is what makes the leak permanent: the roster
//! entry was the *only* link from a session back to its molecule, so the moment
//! it is removed the session becomes unattributable and nothing ever looks at
//! it again. Measured on the development machine on 2026-09-22, fifteen such
//! panes held 4.0 GB resident between them, none of them an idle shell: an
//! agent that has finished its molecule does not exit, it sits at its prompt
//! holding its whole heap.
//!
//! So a session whose molecule is terminal is derived state, and this module
//! is the predicate that says when it may go. It is the I/O-free half
//! (ADR-082); the adapter that fills these values by asking tmux lives outside
//! the core.
//!
//! # What this borrows from [`crate::worktree_reclaim`], and what it must not
//!
//! It borrows the shape: three-valued observations where `Unknown` is never
//! `false`, a consideration gate in which molecule status is a **veto and
//! never proof**, and a predicate whose inputs are narrow enough that the
//! compiler — not a doc comment — enforces what it cannot read.
//!
//! It departs on two points, both load-bearing:
//!
//! 1. **Absence inverts.** For a worktree, a proven-absent molecule *permits*
//!    consideration: `.worktrees/<id>/` is cosmon's own directory, so a
//!    directory nobody claims is still unambiguously cosmon's to reclaim.
//!    A tmux socket is a shared namespace — an operator may run their own
//!    session on it — and a name is not ownership evidence the way a location
//!    is. So [`OwnershipObservation::Unowned`] **withholds**. Ownership is
//!    established by computing each known molecule's session name *forward*
//!    (`slugify::session_name_for`) and matching; it is never parsed backward
//!    out of a session name, which carries only four characters of molecule id
//!    and would be a guess. This is the same refusal as the worktree
//!    contract's "never synthesize `feat/<id>` for a directory without a
//!    molecule".
//!
//! 2. **The whole thing may go, where a worktree's may not.** ADR-178 D1
//!    forbids any automatic path from removing a worktree, because a worktree
//!    holds durable bytes that may exist nowhere else and cosmon cannot
//!    exclude a concurrent writer over them. Neither half of that argument
//!    survives the move to a session:
//!
//!    * A session's durable content is its scrollback, and unlike a worktree's
//!      durable bytes it is **bounded and capturable**. So it is not traded
//!      against the reclamation — it is moved out of the way first, and a
//!      capture that failed withholds the session
//!      ([`ScrollbackObservation::Unavailable`]). Everything else about a
//!      session — the pane, the process, the heap — is rebuilt by `cs tackle`.
//!    * ADR-178's race argument is that every conjunct is "an observation, and
//!      an observation is a claim about the past": `git status` was clean *when
//!      it ran*, and an agent can write a file before the `remove_dir_all`.
//!      Terminality does not decay that way. `MoleculeStatus::can_transition_to`
//!      admits no transition *out of* `Completed` or `Collapsed` — they appear
//!      on no left-hand side — so a molecule observed terminal is terminal for
//!      good. The gate here rests on a **monotone** observation, which is
//!      precisely what the worktree conjuncts were not.
//!
//!    ADR-178 D4 reserves whole-worktree removal for a successor ADR that says
//!    how it excludes a concurrent writer. This module removes no worktree and
//!    widens no worktree path; a session is a different resource, and the
//!    paragraph above is the exclusion argument for it.
//!
//! What stays untouched is the asymmetry of the two mistakes, which is the
//! actual content of ADR-178. A session wrongly reclaimed costs a respawn and
//! a scrollback that was captured to disk a moment earlier. That is the cheap
//! mistake, and every axis below is arranged so the expensive one needs
//! positive evidence.

use crate::molecule::MoleculeStatus;
use crate::worktree_reclaim::ObservationError;

// ---------------------------------------------------------------------------
// The four observation axes
// ---------------------------------------------------------------------------

/// Whether cosmon owns this session at all (axis `O`).
///
/// The replacement for the worktree contract's `M` axis, and the place where
/// absence inverts. `Owned` carries the molecule id that claimed the name, so
/// a report can name it and a status can be looked up for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipObservation {
    /// A known molecule's forward-computed session name equals this session's,
    /// or the fleet roster names it. The string is that molecule's id.
    Owned(String),
    /// Enumeration succeeded and matched nothing: the session is on cosmon's
    /// socket but is not cosmon's. Withholds — see the module docs.
    Unowned,
    /// The enumeration itself failed. Withholds.
    Unknown(ObservationError),
}

/// The owning molecule's lifecycle status (axis `S`).
///
/// Distinct from [`OwnershipObservation`] for the same reason the worktree
/// contract keeps `M` and `S` apart: "no molecule claims this" and "I could
/// not read the status of the molecule that does" are opposite answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusObservation {
    /// A status was read.
    Known(MoleculeStatus),
    /// The status could not be read. Withholds.
    Unknown(ObservationError),
}

/// Whether a tmux client is attached to the session (axis `C`).
///
/// The operator-presence guard, and the one axis that answers a question about
/// *now* rather than about a record. An attached client is a human looking at
/// the pane; reclaiming it out from under them is the expensive mistake in its
/// most direct form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentObservation {
    /// `#{session_attached}` reported zero clients.
    Detached,
    /// At least one client is attached.
    Attached,
    /// The probe failed. Withholds — never read as `Detached`.
    Unknown(ObservationError),
}

/// What became of the pane's scrollback (axis `B`).
///
/// Not a question about whether reclaiming is *allowed* but about whether the
/// durable half has already been carried to safety. It is an input to the
/// predicate rather than a step after it so that a failed capture cannot be
/// stepped over: there is no value of this axis that means "capture failed,
/// proceed anyway".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollbackObservation {
    /// The scrollback was captured and written durably. Carries the number of
    /// lines, for the operator's arithmetic only.
    Captured {
        /// Lines written.
        lines: usize,
    },
    /// The capture or the write failed. Withholds the session.
    Unavailable(ObservationError),
}

// ---------------------------------------------------------------------------
// The consideration gate (`G`)
// ---------------------------------------------------------------------------

/// Permission to consider reclaiming at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consideration {
    /// Reclaim nothing.
    No,
    /// The predicate may apply its remaining rules.
    Yes,
}

/// Normalize ownership and status into [`Consideration`].
///
/// | `O` | `S` | `G` |
/// |---|---|---|
/// | `Owned` | `Completed`, `Collapsed` | Yes |
/// | `Owned` | any other, or `Unknown` | No |
/// | `Unowned` | * | No |
/// | `Unknown` | * | No |
///
/// Status is a veto and never proof: it is consulted only once ownership is
/// positive, so a status file left behind by something cosmon does not own
/// cannot become authority over a session.
#[must_use]
pub fn consideration_gate(
    ownership: &OwnershipObservation,
    status: &StatusObservation,
) -> Consideration {
    match ownership {
        OwnershipObservation::Unowned | OwnershipObservation::Unknown(_) => Consideration::No,
        OwnershipObservation::Owned(_) => match status {
            StatusObservation::Known(s) if s.is_terminal() => Consideration::Yes,
            _ => Consideration::No,
        },
    }
}

// ---------------------------------------------------------------------------
// The predicate
// ---------------------------------------------------------------------------

/// The inputs of [`session_selection`], and deliberately nothing more.
///
/// Pane liveness is absent by construction, and that absence is the decision
/// this module exists to make. A live agent process in the pane is not a
/// reason to keep the session — it *is* the leak, four gigabytes of it, and a
/// predicate able to read it would sooner or later be taught to spare it.
/// Whether the agent is still running says nothing about whether its molecule
/// is finished, and the molecule is what cosmon tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionObservation {
    /// The tmux session name.
    pub name: String,
    /// Axis `O`.
    pub ownership: OwnershipObservation,
    /// Axis `S`.
    pub status: StatusObservation,
    /// Axis `C`.
    pub attachment: AttachmentObservation,
    /// Axis `B`.
    pub scrollback: ScrollbackObservation,
}

/// What [`session_selection`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSelection {
    /// Leave the session alone.
    Keep,
    /// The session may be killed: its molecule is terminal, no client is
    /// attached, and its scrollback is already on disk.
    ReclaimSession,
}

/// Decide whether a session may be reclaimed.
///
/// Three conjuncts, each of which withholds on `Unknown`: a permitting
/// [`Consideration`], a proven-`Detached` session, and a scrollback already
/// captured. There is no flag, anywhere, that makes this return
/// `ReclaimSession` on an input it did not prove.
#[must_use]
pub fn session_selection(obs: &SessionObservation) -> SessionSelection {
    if consideration_gate(&obs.ownership, &obs.status) == Consideration::No {
        return SessionSelection::Keep;
    }
    if obs.attachment != AttachmentObservation::Detached {
        return SessionSelection::Keep;
    }
    if !matches!(obs.scrollback, ScrollbackObservation::Captured { .. }) {
        return SessionSelection::Keep;
    }
    SessionSelection::ReclaimSession
}

/// The exact session-name set [`session_selection`] selects — zero or one.
///
/// A set rather than a bool so a batch falsifier can be stated the way the
/// worktree contract states its own: `reclaimed(F) = {name for rows selected}`.
#[must_use]
pub fn selected_session(obs: &SessionObservation) -> Option<&str> {
    match session_selection(obs) {
        SessionSelection::ReclaimSession => Some(obs.name.as_str()),
        SessionSelection::Keep => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err() -> ObservationError {
        ObservationError::new("tmux list-panes", "/nonexistent", "exit 1")
    }

    /// A session that every axis permits: the baseline each test below
    /// perturbs on exactly one axis, so a changed verdict is attributable.
    fn reclaimable() -> SessionObservation {
        SessionObservation {
            name: "close-two-security-residuals-7251".into(),
            ownership: OwnershipObservation::Owned("task-20260920-7251".into()),
            status: StatusObservation::Known(MoleculeStatus::Completed),
            attachment: AttachmentObservation::Detached,
            scrollback: ScrollbackObservation::Captured { lines: 1200 },
        }
    }

    #[test]
    fn a_completed_molecules_detached_session_is_reclaimed() {
        let obs = reclaimable();
        assert_eq!(session_selection(&obs), SessionSelection::ReclaimSession);
        assert_eq!(
            selected_session(&obs),
            Some("close-two-security-residuals-7251")
        );
    }

    #[test]
    fn a_collapsed_molecule_is_terminal_too() {
        let obs = SessionObservation {
            status: StatusObservation::Known(MoleculeStatus::Collapsed),
            ..reclaimable()
        };
        assert_eq!(session_selection(&obs), SessionSelection::ReclaimSession);
    }

    /// The case this module exists to get right, and the one the running
    /// molecule that wrote it would have been killed by if it were wrong.
    #[test]
    fn a_running_molecules_session_is_never_reclaimed() {
        for status in [
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Starved,
        ] {
            let obs = SessionObservation {
                status: StatusObservation::Known(status),
                ..reclaimable()
            };
            assert_eq!(
                session_selection(&obs),
                SessionSelection::Keep,
                "{status:?} is not terminal and must withhold"
            );
        }
    }

    /// The gate's monotonicity argument, asserted against the type that
    /// supplies it rather than restated in prose. If a transition out of a
    /// terminal status is ever added, the doctrine in this module's header
    /// stops being true and this test is where that is noticed.
    #[test]
    fn terminal_statuses_have_no_exit_transition() {
        let every = [
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Starved,
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
        ];
        for from in [MoleculeStatus::Completed, MoleculeStatus::Collapsed] {
            for to in every {
                assert!(
                    !from.can_transition_to(to),
                    "{from:?} -> {to:?} would make terminality non-monotone, \
                     and the reclaim gate rests on it being monotone"
                );
            }
        }
    }

    #[test]
    fn an_unowned_session_is_never_reclaimed() {
        let obs = SessionObservation {
            ownership: OwnershipObservation::Unowned,
            ..reclaimable()
        };
        assert_eq!(session_selection(&obs), SessionSelection::Keep);
        assert_eq!(selected_session(&obs), None);
    }

    /// Absence inverts relative to the worktree contract. Stated as its own
    /// test because it is the one place a reader who knows ADR-178 would
    /// expect the opposite verdict.
    #[test]
    fn unowned_withholds_where_an_absent_worktree_molecule_would_permit() {
        let session_gate = consideration_gate(
            &OwnershipObservation::Unowned,
            &StatusObservation::Known(MoleculeStatus::Completed),
        );
        assert_eq!(session_gate, Consideration::No);

        let worktree_gate = crate::worktree_reclaim::consideration_gate(
            &crate::worktree_reclaim::MoleculeRecordObservation::Absent,
            &crate::worktree_reclaim::StatusObservation::Known(MoleculeStatus::Running),
        );
        assert_eq!(worktree_gate, crate::worktree_reclaim::Consideration::Yes);
    }

    #[test]
    fn an_attached_session_is_never_reclaimed() {
        let obs = SessionObservation {
            attachment: AttachmentObservation::Attached,
            ..reclaimable()
        };
        assert_eq!(session_selection(&obs), SessionSelection::Keep);
    }

    /// Every failed observation withholds. `Unknown` is not `false`, on any
    /// axis, which is the whole of the 2026-08-02 lesson.
    #[test]
    fn every_unknown_withholds() {
        let cases = [
            SessionObservation {
                ownership: OwnershipObservation::Unknown(err()),
                ..reclaimable()
            },
            SessionObservation {
                status: StatusObservation::Unknown(err()),
                ..reclaimable()
            },
            SessionObservation {
                attachment: AttachmentObservation::Unknown(err()),
                ..reclaimable()
            },
            SessionObservation {
                scrollback: ScrollbackObservation::Unavailable(err()),
                ..reclaimable()
            },
        ];
        for obs in cases {
            assert_eq!(
                session_selection(&obs),
                SessionSelection::Keep,
                "an Unknown axis must withhold: {obs:?}"
            );
        }
    }

    /// The durable half is not traded against the reclamation — it is moved
    /// first. A session whose scrollback could not be written stays up even
    /// though every other axis permits.
    #[test]
    fn an_uncaptured_scrollback_withholds_an_otherwise_permitted_session() {
        let obs = SessionObservation {
            scrollback: ScrollbackObservation::Unavailable(err()),
            ..reclaimable()
        };
        assert_eq!(session_selection(&obs), SessionSelection::Keep);
    }

    /// Differential controls: each axis moved alone flips the verdict alone.
    #[test]
    fn each_axis_is_independently_load_bearing() {
        assert_eq!(
            session_selection(&reclaimable()),
            SessionSelection::ReclaimSession
        );
        let perturbations: Vec<SessionObservation> = vec![
            SessionObservation {
                ownership: OwnershipObservation::Unowned,
                ..reclaimable()
            },
            SessionObservation {
                status: StatusObservation::Known(MoleculeStatus::Running),
                ..reclaimable()
            },
            SessionObservation {
                attachment: AttachmentObservation::Attached,
                ..reclaimable()
            },
            SessionObservation {
                scrollback: ScrollbackObservation::Unavailable(err()),
                ..reclaimable()
            },
        ];
        for obs in perturbations {
            assert_eq!(session_selection(&obs), SessionSelection::Keep);
        }
    }
}
