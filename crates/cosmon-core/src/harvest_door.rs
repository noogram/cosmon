// SPDX-License-Identifier: AGPL-3.0-only

//! The vocabulary of the harvest door — ADR-176, the answer to issue #51.
//!
//! # Why a door and not a command
//!
//! `cs done` carries two authorities: the **closure** of a molecule's own
//! lifecycle, and the **integration** of its branch into a trunk that every
//! future molecule inherits as its initial condition. ADR-176 D2 refuses to
//! split that into two verbs for the requester to choose between. The
//! requester states an intent; the door decides which authorities that intent
//! needs, and refuses by name when it cannot exercise them.
//!
//! So the door takes **one** argument — the molecule — and nothing else. Not
//! a strategy, not a `--force`, not a hook skip. ADR-176 D4 states the reason
//! in one sentence: *a derogation requested by its beneficiary is not a
//! derogation*. Every flag `cs done` owns exists to let the **operator**
//! overrule a gate built to protect a third party; a remote requester holding
//! one holds the gate's own off-switch.
//!
//! # Why the refusals live here and not at either end
//!
//! A refusal has to be the same string in three places: the exit code of the
//! CLI door, the label the §8p route returns, and the word an operator greps
//! for in a log. Three spellings of one refusal is three refusals as far as a
//! script is concerned. [`DoorRefusal`] is the single spelling, and
//! `harvest_door_labels_and_codes_are_a_bijection` pins the mirror so a new
//! variant cannot quietly reuse another's code.
//!
//! A refusal with no name is a question asked of an operator who is not there
//! (ADR-110 I4). That is why there is no `Other` variant and no free-text
//! fallback: an outcome this enum cannot express is an outcome the door must
//! not produce.

use std::fmt;

/// Why the door refused. One variant per **decidable** refusal; there is
/// deliberately no catch-all.
///
/// The ordering of the variants is the order of the checks: admissibility
/// first (nothing observed yet), then the sealed authority, then the bounded
/// backlog, then the three ADR-176 D7 execution outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DoorRefusal {
    /// The molecule is not `Completed`. Closure is not owed on work that is
    /// still in flight, and integration is not owed on work that was
    /// abandoned — the `run`-step-9 defect of ADR-176 §1 in its general form.
    NotCompleted,
    /// The galaxy has not armed harvest authority, or no operator-sealed
    /// grant resolves for this molecule on this base.
    ///
    /// This is the **second key**. The JWT authenticates the requester; the
    /// seal authorises the effect. A bearer brings a request, never an
    /// authority (ADR-176 D1).
    NotAuthorized,
    /// The molecule carries a reservation that only a human can lift —
    /// `hold:human`, `needs-review`, `security`, `security:*`,
    /// `no-auto-harvest`, `harvest_to:*`.
    ///
    /// Named separately from [`Self::NotAuthorized`] because the operator's
    /// recovery differs: one needs a seal, the other needs a verdict.
    ReservationRequiresSeal,
    /// The kernel already holds its sealed threshold of molecules in the
    /// *closed, not integrated* condition.
    ///
    /// ADR-176 D7: a bounded, refusing queue is a delayed refusal, not a
    /// stall. An unbounded one would be the silent block ADR-110 I4 forbids.
    BacklogFull,
    /// A textual merge conflict. An **execution event**: the repository holds
    /// the information and says the two sides disagree. Nothing was torn
    /// down, nothing merged, the branch and worktree stand.
    MergeConflict,
    /// The resolved base cannot accept the configured merge policy.
    ///
    /// An **operator configuration error**, decidable before the first
    /// request ever arrives (ADR-176 D7). It is reported as a *server* fault
    /// on the §8p boundary precisely so it is not charged to the tenant: a
    /// door that lets a requester discover it at the bottom of a detached
    /// loop has converted an operator's misconfiguration into a tenant
    /// runtime failure class.
    BaseNotFastForward,
    /// The galaxy's blocking `[hooks] pre_done` gate refused. A **verdict**:
    /// the missing information is not in the repository but in a human's
    /// judgement, so the molecule joins the bounded backlog of
    /// [`Self::BacklogFull`].
    PreDoneRefused,
}

/// Every refusal, in check order. Iterated by the mirror tests and by any
/// caller that must enumerate the closed set (a doc table, a client SDK).
pub const ALL_REFUSALS: &[DoorRefusal] = &[
    DoorRefusal::NotCompleted,
    DoorRefusal::NotAuthorized,
    DoorRefusal::ReservationRequiresSeal,
    DoorRefusal::BacklogFull,
    DoorRefusal::MergeConflict,
    DoorRefusal::BaseNotFastForward,
    DoorRefusal::PreDoneRefused,
];

impl DoorRefusal {
    /// The stable wire label — the same string on the exit line, in the JSON
    /// body of the §8p response, and in the operator's log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotCompleted => "not_completed",
            Self::NotAuthorized => "not_authorized",
            Self::ReservationRequiresSeal => "reservation_requires_seal",
            Self::BacklogFull => "backlog_full",
            Self::MergeConflict => "merge_conflict",
            Self::BaseNotFastForward => "base_not_fast_forward",
            Self::PreDoneRefused => "pre_done_refused",
        }
    }

    /// The stable process exit code of the CLI door.
    ///
    /// The block 70–76 is unclaimed by every other cosmon refusal
    /// (`cmd::guard::exit_code` occupies 10–17, `cs run`'s named drain exits
    /// 90–93, and 124 is the timeout convention). A route reads this code to
    /// pick the label rather than parsing stderr, which is why the two must
    /// stay a bijection.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::NotCompleted => 70,
            Self::NotAuthorized => 71,
            Self::ReservationRequiresSeal => 72,
            Self::BacklogFull => 73,
            Self::MergeConflict => 74,
            Self::BaseNotFastForward => 75,
            Self::PreDoneRefused => 76,
        }
    }

    /// Recover the refusal from a door exit code. `None` for a code the door
    /// does not own — including `0`, which is not a refusal at all.
    #[must_use]
    pub fn from_exit_code(code: i32) -> Option<Self> {
        ALL_REFUSALS.iter().copied().find(|r| r.exit_code() == code)
    }

    /// Whether the refusal names a fault in the **operator's configuration**
    /// rather than anything the requester did or could change.
    ///
    /// ADR-176 D7 singles out `base_not_fast_forward`: it is fully decidable
    /// at capability arming, so a request-time occurrence means the grant was
    /// issued against a base that cannot accept the configured policy. The
    /// §8p boundary maps this to a 5xx for that reason, and to no other.
    #[must_use]
    pub const fn is_operator_configuration_fault(self) -> bool {
        matches!(self, Self::BaseNotFastForward)
    }

    /// One sentence naming what happened and what would lift it.
    ///
    /// A refusal nobody can act on is a refusal nobody investigates
    /// (ADR-171 D6), so every line names the *gesture* that unblocks it and
    /// never merely restates the label.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotCompleted => {
                "the molecule is not Completed: closure is not owed on work in flight, and \
                 integration is not owed on work that was abandoned (ADR-176 §1)"
            }
            Self::NotAuthorized => {
                "no operator-sealed harvest grant covers this molecule on this base. The \
                 bearer token authenticates the requester; the seal authorises the effect \
                 (ADR-176 D1). An operator arms `[harvest_authority] required` and seals a \
                 grant out of band"
            }
            Self::ReservationRequiresSeal => {
                "the molecule carries a reservation only a human can lift (hold:human, \
                 needs-review, security, security:*, no-auto-harvest, harvest_to:*). The \
                 door cannot supply the verdict the reservation asks for"
            }
            Self::BacklogFull => {
                "this kernel already holds its sealed threshold of closed-but-unintegrated \
                 molecules. The queue is bounded on purpose: a delayed refusal beats a \
                 silent block (ADR-176 D7, ADR-110 I4). An operator drains the backlog"
            }
            Self::MergeConflict => {
                "the branch and the base disagree textually. Nothing was merged, nothing \
                 torn down: the branch and the worktree stand, and a rebase makes the \
                 request retryable unchanged"
            }
            Self::BaseNotFastForward => {
                "the resolved base cannot accept the configured merge policy. This is an \
                 operator configuration fault, decidable at arming time (ADR-176 D7) — it \
                 is not charged to the requester"
            }
            Self::PreDoneRefused => {
                "the galaxy's blocking `[hooks] pre_done` gate refused. The missing input \
                 is a human judgement, not a repository fact; the molecule is recorded as \
                 closed-but-unintegrated and counts against the bounded backlog"
            }
        }
    }
}

impl fmt::Display for DoorRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.as_str(), self.message())
    }
}

/// What the door did when it did not refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorOutcome {
    /// The molecule was closed and, where the second authority arose, its
    /// branch landed on the resolved base.
    Landed,
    /// The harvest had already landed. The door mutates nothing and reports
    /// the same success as the first call — idempotence is what makes a
    /// retried request safe over a network that loses responses.
    AlreadyLanded,
}

impl DoorOutcome {
    /// Stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::AlreadyLanded => "already_landed",
        }
    }
}

/// Tags that reserve a molecule for a human decision.
///
/// Exact matches. The prefix family lives in [`RESERVATION_TAG_PREFIXES`]
/// because `security:` and `harvest_to:` carry a payload the door never
/// interprets — it only observes that someone attached a condition.
pub const RESERVATION_TAGS: &[&str] =
    &["hold:human", "needs-review", "security", "no-auto-harvest"];

/// Tag prefixes that reserve a molecule for a human decision.
pub const RESERVATION_TAG_PREFIXES: &[&str] = &["security:", "harvest_to:"];

/// The reservation that stops this harvest, if any.
///
/// Returns the *tag*, not a bool, so the refusal can name which condition
/// fired: "reserved" tells an operator nothing they can act on, whereas
/// `harvest_to:release-2` tells them where the work was meant to go.
#[must_use]
pub fn reservation_requiring_seal(tags: &[String]) -> Option<&str> {
    tags.iter().map(String::as_str).find(|tag| {
        RESERVATION_TAGS.contains(tag)
            || RESERVATION_TAG_PREFIXES.iter().any(|p| tag.starts_with(p))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn harvest_door_labels_and_codes_are_a_bijection() {
        // The §8p route picks its label from the CLI's exit code. Two
        // variants sharing either side would make one refusal readable as
        // the other — the failure mode this mirror exists to prevent.
        let labels: BTreeSet<&str> = ALL_REFUSALS.iter().map(|r| r.as_str()).collect();
        let codes: BTreeSet<i32> = ALL_REFUSALS.iter().map(|r| r.exit_code()).collect();
        assert_eq!(labels.len(), ALL_REFUSALS.len(), "duplicate refusal label");
        assert_eq!(
            codes.len(),
            ALL_REFUSALS.len(),
            "duplicate refusal exit code"
        );
        for refusal in ALL_REFUSALS {
            assert_eq!(
                DoorRefusal::from_exit_code(refusal.exit_code()),
                Some(*refusal)
            );
        }
    }

    #[test]
    fn no_refusal_code_collides_with_a_success_or_another_cosmon_refusal() {
        // 0 and 1 are success and the generic error; 10–17 belong to
        // `cmd::guard::exit_code`; 90–93 and 124 are `cs run`'s named drain
        // exits. A collision would make a script branch on the wrong rule.
        for refusal in ALL_REFUSALS {
            let code = refusal.exit_code();
            assert!(
                (70..=79).contains(&code),
                "{refusal:?} left the door's reserved 70–79 block with {code}",
            );
        }
    }

    #[test]
    fn only_the_configuration_fault_is_flagged_as_one() {
        // Pinned rather than obvious: mis-flagging a refusal here turns a
        // tenant's own conflict into a 5xx, which reads as "the server is
        // broken" on a page the tenant cannot fix either way.
        let faults: Vec<_> = ALL_REFUSALS
            .iter()
            .filter(|r| r.is_operator_configuration_fault())
            .collect();
        assert_eq!(faults, vec![&DoorRefusal::BaseNotFastForward]);
    }

    #[test]
    fn every_reservation_family_is_detected() {
        let cases = [
            ("hold:human", true),
            ("needs-review", true),
            ("security", true),
            ("security:audit", true),
            ("no-auto-harvest", true),
            ("harvest_to:release-2", true),
            ("temp:warm", false),
            ("securityish", false),
            ("harvest", false),
        ];
        for (tag, reserved) in cases {
            let tags = vec![tag.to_owned()];
            assert_eq!(
                reservation_requiring_seal(&tags).is_some(),
                reserved,
                "tag {tag:?} classified wrong",
            );
        }
    }

    #[test]
    fn the_reservation_is_named_not_merely_counted() {
        let tags = vec!["temp:warm".to_owned(), "harvest_to:release-2".to_owned()];
        assert_eq!(
            reservation_requiring_seal(&tags),
            Some("harvest_to:release-2")
        );
    }

    #[test]
    fn every_refusal_message_names_more_than_its_label() {
        for refusal in ALL_REFUSALS {
            assert!(
                refusal.message().len() > refusal.as_str().len() + 40,
                "{refusal:?} has a message that only restates its label",
            );
        }
    }
}
