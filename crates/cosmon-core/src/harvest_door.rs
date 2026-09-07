// SPDX-License-Identifier: AGPL-3.0-only

//! The vocabulary of the harvest door — ADR-176, the answer to issue #51,
//! **as amended by its reversal of D4**.
//!
//! # Why a door and not a command
//!
//! `cs done` carries two authorities: the **closure** of a molecule's own
//! lifecycle, and the **integration** of its branch into a trunk that every
//! future molecule inherits as its initial condition. ADR-176 D2 refuses to
//! split that into two verbs for the requester to choose between. The
//! requester states an intent; the door decides which authorities that intent
//! needs, and refuses by name when it cannot exercise them. That decision
//! stands.
//!
//! # Why the door now carries the whole parameter set
//!
//! It did not, at first. ADR-176 D4 read *no option crosses the wire*, and
//! rested on one sentence — *a derogation requested by its beneficiary is not
//! a derogation*. That sentence is sound only when the requester is a
//! constrained principal, **distinct** from the party the gate protects. The
//! deployment that exists is single-tenant: one galaxy, one nucleon, one
//! user, and the requester *is* the operator. There, beneficiary and
//! protected party are the same person, the gate protects nobody, and
//! withholding `--strategy` from someone merging into their own trunk is an
//! amputation of their own verb rather than a safety property.
//!
//! So D4 is reversed and [`HarvestOptions`] carries the full argument set of
//! `cs done`. What did **not** move is the authority: the operator's
//! `[harvest_authority]` arming still authorises the *effect* (D1). The seal
//! answers *may this requester cause this effect at all*; the options answer
//! *how*. Reversing D4 does not dismantle D1, and the seven named
//! [`DoorRefusal`]s below are untouched by it.
//!
//! The condition under which D4 comes back is stated and testable: a
//! requester who is **not** the operator — the multi-tenant phase. What that
//! phase needs is a restriction on *which* molecules a requester may close,
//! not on which parameters they may pass; ADR-176 D5 still refuses an
//! `owner` field, and this module deliberately grows no ownership notion.
//!
//! # Why the refusals live here and not at either end
//!
//! A refusal has to be the same string in three places: the exit code of the
//! CLI verb, the label the §8p route returns, and the word an operator greps
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
    /// The request carried no reason for closing the molecule.
    ///
    /// The eighth refusal, added by the D4 reversal, and the only one that
    /// is a *fault of the argument set* rather than of the world. It exists
    /// because the alternative is worse than a refusal: `land` fabricated a
    /// generic reason, and a fabricated reason is indistinguishable, a year
    /// later, from one somebody meant. The seven ADR-176 refusals keep their
    /// labels and their exit codes 70–76 unchanged; this one takes 77.
    MissingReason,
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
    DoorRefusal::MissingReason,
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
            Self::MissingReason => "missing_reason",
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
            Self::MissingReason => 77,
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
            Self::MissingReason => {
                "the request named no reason for closing this molecule. The reason is \
                 traced trunk-side and is the only account a later reader has; the door \
                 will not invent one. Send `reason` with a sentence a human would write"
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

/// Merge strategy for the branch a harvest integrates.
///
/// The domain twin of `cs done --strategy`. It lives here rather than in the
/// CLI because the wire, the door and the merge must name the same two
/// shapes; a third spelling in a request body is a third strategy as far as a
/// script is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeStrategy {
    /// Non-fast-forward merge (`git merge --no-ff --no-edit`) — the default,
    /// because parallel tackling is the validated common case: when the
    /// first worker lands, the trunk moves and the second can no longer
    /// fast-forward.
    #[default]
    Merge,
    /// Fast-forward-only merge (`git merge --ff-only`). Strictly linear
    /// history; refused when the completion merge must carry trailers,
    /// because a fast-forward creates no cosmon-owned commit to stamp.
    FfOnly,
}

impl MergeStrategy {
    /// Stable wire token — the same word `cs done --strategy` accepts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::FfOnly => "ff-only",
        }
    }

    /// Parse a wire token, or `None` for anything else.
    ///
    /// Deliberately not `FromStr` with a lossy fallback: a body carrying
    /// `"fast-forward"` must be refused as an unsupported parameter, never
    /// silently defaulted to `merge` — a strategy the requester did not ask
    /// for is the same defect as a strategy they could not ask for.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "merge" => Some(Self::Merge),
            "ff-only" => Some(Self::FfOnly),
            _ => None,
        }
    }
}

impl fmt::Display for MergeStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The full parameter set of one harvest — the argument set of `cs done`,
/// expressed once, in the domain, for every caller of the door.
///
/// # Why this type exists
///
/// Before the D4 reversal the door's argument set was fixed at the type
/// (`done::Args::sealed_door`) so that *no option could cross the wire*. The
/// reversal replaces that seal with this struct: the wire, the CLI and the
/// merge now read the **same** options, so a parameter cannot mean one thing
/// in a request body and another at the merge. It is I/O-free and carries no
/// authority — the operator's `[harvest_authority]` arming still decides
/// whether the effect may happen at all (D1).
///
/// # Why `reason` is not optional
///
/// Closing someone's molecule is a lifecycle act that outlives the request,
/// and the trunk-side record of *why* is the only thing a later reader has.
/// `land` fabricated a generic one; the door refuses to. A caller that has
/// nothing to say is a caller who has not decided, and
/// [`Self::validate`] answers that with [`DoorRefusal::MissingReason`]
/// rather than inventing a sentence on their behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
// Ten independent bool fields because each mirrors one independent `cs done`
// opt-out, one-to-one. A bitflag or a nested enum would rename the mapping
// without reducing it, and the round-trip falsifier
// (`every_harvest_option_survives_the_argv_round_trip`) is what keeps the
// list honest — exactly the reasoning `done::Args` already carries.
#[allow(clippy::struct_excessive_bools)]
pub struct HarvestOptions {
    /// Why this molecule is being closed. Traced on the molecule; never
    /// fabricated. Empty or whitespace-only is a refusal, not a default.
    pub reason: String,
    /// Merge strategy for the worker's branch.
    pub strategy: MergeStrategy,
    /// Proceed even if the molecule is not in a terminal state.
    pub force: bool,
    /// Silent no-op when the molecule is not `Completed` or already merged.
    pub if_completed: bool,
    /// Skip merging the worker's branch into the base branch.
    pub no_merge: bool,
    /// Skip removing the git worktree.
    pub no_worktree_remove: bool,
    /// Skip deleting the worker's branch after the merge.
    pub no_branch_delete: bool,
    /// Skip killing the worker's session.
    pub no_kill: bool,
    /// Disable auto-propel escalation on merge conflict.
    pub no_auto_propel: bool,
    /// Custom message sent to the worker during auto-propel escalation.
    pub propel_message: Option<String>,
    /// Maximum number of auto-propel escalation retries before giving up.
    pub max_retries: u32,
    /// Skip the blocking `[hooks] pre_done` gate for this invocation.
    pub skip_pre_done_hook: bool,
    /// Run the `[hooks] post_merge` deploy hook even off the reference trunk.
    pub deploy_off_trunk: bool,
}

impl HarvestOptions {
    /// The options of a harvest whose caller supplied only a reason.
    ///
    /// Every other field takes the same value `cs done` defaults to, so the
    /// door with no options passed behaves exactly as the operator's bare
    /// `cs done <mol>` does — the documented default falsifier 1 names.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            strategy: MergeStrategy::Merge,
            force: false,
            if_completed: false,
            no_merge: false,
            no_worktree_remove: false,
            no_branch_delete: false,
            no_kill: false,
            no_auto_propel: false,
            propel_message: None,
            max_retries: DEFAULT_MAX_RETRIES,
            skip_pre_done_hook: false,
            deploy_off_trunk: false,
        }
    }

    /// The `cs done` argument vector this option set executes as.
    ///
    /// The **one** place the domain options become CLI flags. It exists so
    /// that "the parameter arrives at the merge" is a property something can
    /// assert end to end: the effect adapter spawns exactly this argv, and
    /// `cs done`'s own parser turns it back into the argument set the merge
    /// reads. A flag added to [`HarvestOptions`] and forgotten here is a
    /// parameter the requester can send and the merge never sees, which is
    /// the defect the D4 reversal exists to remove.
    ///
    /// Flags are emitted only when they differ from `cs done`'s own default,
    /// so the argv of a bare harvest is `["done", "<mol>", "--reason", …]` —
    /// the documented default, not a re-statement of it.
    #[must_use]
    pub fn cs_done_argv(&self, molecule: &str) -> Vec<String> {
        let mut argv = vec![
            "done".to_owned(),
            molecule.to_owned(),
            "--reason".to_owned(),
            self.reason.clone(),
        ];
        let mut flag = |on: bool, name: &str| {
            if on {
                argv.push(name.to_owned());
            }
        };
        flag(self.force, "--force");
        flag(self.if_completed, "--if-completed");
        flag(self.no_merge, "--no-merge");
        flag(self.no_worktree_remove, "--no-worktree-remove");
        flag(self.no_branch_delete, "--no-branch-delete");
        flag(self.no_kill, "--no-kill");
        flag(self.no_auto_propel, "--no-auto-propel");
        flag(self.skip_pre_done_hook, "--skip-pre-done-hook");
        flag(self.deploy_off_trunk, "--deploy-off-trunk");
        if self.strategy != MergeStrategy::Merge {
            argv.push("--strategy".to_owned());
            argv.push(self.strategy.as_str().to_owned());
        }
        if let Some(message) = &self.propel_message {
            argv.push("--propel-message".to_owned());
            argv.push(message.clone());
        }
        if self.max_retries != DEFAULT_MAX_RETRIES {
            argv.push("--max-retries".to_owned());
            argv.push(self.max_retries.to_string());
        }
        argv
    }

    /// Refuse an argument set the door cannot honour.
    ///
    /// # Errors
    ///
    /// [`DoorRefusal::MissingReason`] when the reason is absent in
    /// substance — empty or whitespace only.
    pub fn validate(&self) -> Result<(), DoorRefusal> {
        if self.reason.trim().is_empty() {
            return Err(DoorRefusal::MissingReason);
        }
        Ok(())
    }
}

/// Default auto-propel retry ceiling — the value `cs done --max-retries`
/// documents as its default, named once so the wire and the CLI cannot
/// drift.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

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
    /// Falsifier 4 of the D4 reversal: reversing D4 did not break the
    /// failure surface.
    ///
    /// The seven ADR-176 refusals keep their exact labels and their exact
    /// exit codes 70–76, pinned literally rather than derived, so a
    /// renumbering that a bijection test would happily accept fails here.
    /// `missing_reason` is the eighth and takes 77; it displaces nothing.
    #[test]
    fn the_seven_adr_176_refusals_keep_their_labels_and_codes() {
        let pinned: &[(DoorRefusal, &str, i32)] = &[
            (DoorRefusal::NotCompleted, "not_completed", 70),
            (DoorRefusal::NotAuthorized, "not_authorized", 71),
            (
                DoorRefusal::ReservationRequiresSeal,
                "reservation_requires_seal",
                72,
            ),
            (DoorRefusal::BacklogFull, "backlog_full", 73),
            (DoorRefusal::MergeConflict, "merge_conflict", 74),
            (DoorRefusal::BaseNotFastForward, "base_not_fast_forward", 75),
            (DoorRefusal::PreDoneRefused, "pre_done_refused", 76),
        ];
        for (refusal, label, code) in pinned {
            assert_eq!(refusal.as_str(), *label, "{label} lost its label");
            assert_eq!(refusal.exit_code(), *code, "{label} lost its exit code");
            assert_eq!(DoorRefusal::from_exit_code(*code), Some(*refusal));
        }
        assert_eq!(DoorRefusal::MissingReason.exit_code(), 77);
        assert_eq!(DoorRefusal::MissingReason.as_str(), "missing_reason");
        // And the operator-configuration classification of D7 is
        // untouched: exactly one refusal is not charged to the requester.
        let operator_faults: Vec<&str> = ALL_REFUSALS
            .iter()
            .filter(|r| r.is_operator_configuration_fault())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(operator_faults, vec!["base_not_fast_forward"]);
    }

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
