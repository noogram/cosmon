// SPDX-License-Identifier: AGPL-3.0-only

//! The PRIMARY lease and its supervised transfer (mission co-pilotage M4).
//!
//! ADR-168 §D6 states the authority model in one sentence: **authority is a
//! lease with an epoch, and every mutation carries the epoch it believed it
//! held.** This module is that sentence, made decidable and I/O-free.
//!
//! # What a lease is, and what it is not
//!
//! A [`PilotLease`] says who may issue piloting gestures on one mission, for
//! how long, and at which [`LeaseEpoch`]. It is *not* the `hold:pilot` tag of
//! `cs claim`: that tag is a boolean on one molecule and stays a boolean. The
//! lease is the same idea raised from one molecule to one mission, and the
//! guard here — not the tag — is what refuses.
//!
//! # The four rules, and why each exists
//!
//! 1. **A transfer increments the epoch.** Two pilots cannot both be at the
//!    head of a strictly increasing sequence, so NO-SPLIT-BRAIN is a property
//!    of arithmetic rather than of a lock nobody holds.
//! 2. **A mutation presents the epoch it believed it held.** Without that, a
//!    stale primary that never noticed the transfer is indistinguishable from
//!    the current one, and the refusal would have to be a compensation after
//!    the fact. With it, [`authorize`] refuses *before* the effect.
//! 3. **Anything unknown is read-only.** No lease, no epoch presented, an
//!    expired lease or a different holder all resolve to a refusal.
//!    FAIL-CLOSED-AUTHORITY is the default, not a branch.
//! 4. **A grant is a signature, not a claim.** `granted_by` used to be a free
//!    string, which made the operator gesture forgeable by the very agent it
//!    seated (M7 dogfood, friction F1). It is now inside bytes an operator
//!    signs out of band — see [`crate::operator_attestation`] — and a ledger
//!    line whose signature does not check is a grant that did not happen.
//!
//! # What this module deliberately cannot do
//!
//! It cannot grant itself a lease from a quota reading, a heartbeat gap or a
//! timeout. A [`LeaseRequest`] is an *ask* and produces no authority at all;
//! only an operator's grant does, which is why the two are separate records in
//! separate files with separate writers (ADR-168 §D3.1, TAKEOVER-SUPERVISED).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::id::{IdError, MoleculeId, SessionId};
use crate::operator_attestation::{ChallengeError, GrantChallenge, OperatorAttestation};

/// Monotonic generation counter of a mission's lease.
///
/// A newtype rather than a bare `u64` because the whole safety argument rests
/// on this number only ever moving one way, and a bare integer invites the
/// arithmetic that would move it the other way.
///
/// # Examples
///
/// ```
/// use cosmon_core::pilot_lease::LeaseEpoch;
///
/// let first = LeaseEpoch::first();
/// assert_eq!(first.get(), 1);
/// assert_eq!(first.next().get(), 2);
/// assert!(first < first.next());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseEpoch(u64);

impl LeaseEpoch {
    /// The epoch of a mission's first grant. Epochs start at 1 so that a
    /// decoded `0` — the value a zero-filled or defaulted record produces —
    /// is never a valid authority.
    #[must_use]
    pub fn first() -> Self {
        Self(1)
    }

    /// Build an epoch from a raw counter.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Invalid`] for `0`: an epoch of zero is what an
    /// uninitialised record looks like, and accepting it would make the
    /// absence of a grant indistinguishable from the first one.
    pub fn new(raw: u64) -> Result<Self, IdError> {
        if raw == 0 {
            return Err(IdError::Invalid {
                kind: "LeaseEpoch",
                reason: "epoch 0 is the uninitialised value, not an authority".to_owned(),
            });
        }
        Ok(Self(raw))
    }

    /// The epoch a transfer out of this one lands on.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// The raw counter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for LeaseEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identity of one takeover request.
///
/// Derived from what makes it that request, exactly as [`crate::pilot_message::MessageId`]
/// is: a requester that crashes after computing the id but before appending it
/// recomputes the same id on retry, so the retry deduplicates instead of
/// queueing a second ask the operator would have to arbitrate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RequestId(String);

impl RequestId {
    /// Build a request id from `raw`.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Empty`] if `raw` is empty and [`IdError::Invalid`]
    /// if it holds whitespace — an operator has to paste this id back into
    /// `cs presence lease grant`, and an id with a space in it cannot survive
    /// that trip unquoted.
    pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(IdError::Empty { kind: "RequestId" });
        }
        if raw.chars().any(char::is_whitespace) {
            return Err(IdError::Invalid {
                kind: "RequestId",
                reason: format!("{raw:?} contains whitespace"),
            });
        }
        Ok(Self(raw))
    }

    /// Derive the id of a request from the mission, the candidate, the epoch
    /// the requester observed, and the instant it asked.
    ///
    /// The observed epoch participates on purpose: the same pilot asking again
    /// *after* a transfer is asking a different question, and collapsing the
    /// two onto one id would let a grant answer a request nobody made.
    #[must_use]
    pub fn derive(
        mission: &MoleculeId,
        candidate: &SessionId,
        observed_epoch: Option<LeaseEpoch>,
        requested_at: DateTime<Utc>,
    ) -> Self {
        let mut h = Sha256::new();
        h.update(mission.as_str().as_bytes());
        h.update(b"\x00");
        h.update(candidate.as_str().as_bytes());
        h.update(b"\x00");
        h.update(observed_epoch.map_or(0, LeaseEpoch::get).to_be_bytes());
        h.update(b"\x00");
        h.update(requested_at.to_rfc3339().as_bytes());
        let digest = h.finalize();
        let mut hex = String::with_capacity(12);
        for b in digest.iter().take(6) {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
        }
        Self(format!("req-{hex}"))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pilot asking for the controls. Carries no authority whatsoever.
///
/// Kept in its own record, and on disk in its own file, because the requester
/// and the granter are different actors: a pilot writes requests, the operator
/// writes grants. A crash anywhere between the two leaves the mission exactly
/// as authoritative as it was, which is the M4 acceptance clause on *crash
/// between request and grant*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRequest {
    /// Identity — the deduplication key. See [`RequestId::derive`].
    pub id: RequestId,
    /// Mission the controls are being asked for.
    pub mission_id: MoleculeId,
    /// Session that would become PRIMARY if the operator granted this.
    pub candidate_session_id: SessionId,
    /// Holder the requester believed was in the seat, `None` if it believed
    /// the seat was empty. Recorded so the operator can see whether the
    /// request was written against the world as it is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_holder: Option<SessionId>,
    /// Epoch the requester observed. `None` means it saw no lease at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_epoch: Option<LeaseEpoch>,
    /// Session that asked. Usually the candidate; a co-pilot may also ask on
    /// behalf of a primary that is about to run out of credit.
    pub requested_by: SessionId,
    /// When the ask was written.
    pub requested_at: DateTime<Utc>,
    /// One line the operator reads before deciding. Free-form.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

impl LeaseRequest {
    /// Compose a request, deriving its [`RequestId`] from its content.
    #[must_use]
    pub fn new(
        mission_id: MoleculeId,
        candidate_session_id: SessionId,
        requested_by: SessionId,
        observed: Option<&PilotLease>,
        requested_at: DateTime<Utc>,
        reason: impl Into<String>,
    ) -> Self {
        let observed_epoch = observed.map(|l| l.epoch);
        let id = RequestId::derive(
            &mission_id,
            &candidate_session_id,
            observed_epoch,
            requested_at,
        );
        Self {
            id,
            mission_id,
            candidate_session_id,
            observed_holder: observed.map(|l| l.holder_session_id.clone()),
            observed_epoch,
            requested_by,
            requested_at,
            reason: reason.into(),
        }
    }
}

/// The authority record: who holds one mission's controls, at which epoch.
///
/// # Examples
///
/// ```
/// use chrono::{Duration, Utc};
/// use cosmon_core::id::{MoleculeId, SessionId};
/// use cosmon_core::pilot_lease::{authorize, LeaseEpoch, LeaseDecision, PilotLease};
///
/// let now = Utc::now();
/// let mission = MoleculeId::new("task-20260731-9cf4").unwrap();
/// let claude = SessionId::new("claude-sid").unwrap();
/// let codex = SessionId::new("codex-sid").unwrap();
///
/// let lease = PilotLease::new(mission, claude.clone(), LeaseEpoch::first(), "operator", now, None);
///
/// // The holder, presenting the epoch it holds, may act.
/// assert!(authorize(Some(&lease), now, &claude, Some(LeaseEpoch::first())).is_granted());
/// // The co-pilot may not, however current its epoch reading is.
/// assert!(!authorize(Some(&lease), now, &codex, Some(LeaseEpoch::first())).is_granted());
/// // Nor may the holder without saying which epoch it believes it holds.
/// assert!(!authorize(Some(&lease), now, &claude, None).is_granted());
/// // Nor may anyone at all when there is no lease.
/// assert_eq!(authorize(None, now, &claude, Some(LeaseEpoch::first())), LeaseDecision::Refused(cosmon_core::pilot_lease::RefusalReason::NoLease));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PilotLease {
    /// Mission whose controls this lease covers.
    pub mission_id: MoleculeId,
    /// Session holding the controls.
    pub holder_session_id: SessionId,
    /// Generation of this grant. Strictly increasing per mission.
    pub epoch: LeaseEpoch,
    /// Operator identity that granted it. A *human*, by construction: no
    /// pilot and no heuristic writes this record.
    pub granted_by: String,
    /// When the grant was written.
    pub granted_at: DateTime<Utc>,
    /// After this instant the lease authorises nothing. `None` means it holds
    /// until the next grant supersedes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The request this grant answers, when it answers one. `None` for a
    /// grant the operator issued unprompted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    /// The operator's detached signature over [`PilotLease::challenge`].
    ///
    /// `Option` because the field is what a ledger line *carries*, not what a
    /// reader accepts: a line without one deserialises fine and then confers
    /// nothing, which is how a hand-appended grant is refused instead of
    /// crashing the reader. See [`crate::operator_attestation`] for why
    /// `granted_by` alone was not enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<OperatorAttestation>,
}

impl PilotLease {
    /// Compose a lease record.
    #[must_use]
    pub fn new(
        mission_id: MoleculeId,
        holder_session_id: SessionId,
        epoch: LeaseEpoch,
        granted_by: impl Into<String>,
        granted_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            mission_id,
            holder_session_id,
            epoch,
            granted_by: granted_by.into(),
            granted_at,
            expires_at,
            request_id: None,
            attestation: None,
        }
    }

    /// Attach the request this grant answers.
    #[must_use]
    pub fn answering(mut self, request: &LeaseRequest) -> Self {
        self.request_id = Some(request.id.clone());
        self
    }

    /// Attach the operator signature that authorised this transfer.
    #[must_use]
    pub fn attested_by(mut self, attestation: OperatorAttestation) -> Self {
        self.attestation = Some(attestation);
        self
    }

    /// The bytes the operator had to sign for this grant to be honoured.
    ///
    /// Rebuilt from the record rather than stored beside it, so a ledger line
    /// cannot carry a signature over a *different* transfer than the one it
    /// describes. The time-to-live is recovered as `expires_at - granted_at`,
    /// which is exact because a grant computes both from the same instant.
    ///
    /// # Errors
    ///
    /// [`ChallengeError`] when the recorded `granted_by` is not a name the
    /// canonical encoding can hold — which is itself a refusal, since such a
    /// line could never have been signed.
    pub fn challenge(&self) -> Result<GrantChallenge, ChallengeError> {
        GrantChallenge::new(
            self.mission_id.clone(),
            self.holder_session_id.clone(),
            self.epoch,
            self.granted_by.clone(),
            self.expires_at
                .map(|deadline| (deadline - self.granted_at).num_seconds()),
        )
    }

    /// Return `true` iff `now` is within the lease's validity window.
    ///
    /// Exactly at `expires_at` is already expired, the same comparator
    /// discipline [`crate::pilot_message::PilotMessage::state`] uses.
    #[must_use]
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            Some(deadline) => now < deadline,
            None => true,
        }
    }
}

/// Why a piloting gesture was refused.
///
/// Enumerated rather than collapsed into one "denied", because the operator
/// reading the refusal has to know whether to grant a lease, to re-read the
/// epoch, or to stop trying — three different next moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum RefusalReason {
    /// No lease exists for this mission. Nobody is PRIMARY, so nobody may act.
    NoLease,
    /// A lease exists but the gesture presented no epoch. The rule is not
    /// "hold the lease" but "hold it and say so".
    EpochNotPresented,
    /// The lease is held by another session.
    NotHolder {
        /// The session that actually holds it, so the caller can address it.
        holder: SessionId,
    },
    /// The presented epoch is not the current one — the classic stale primary
    /// that never noticed the transfer.
    EpochMismatch {
        /// What the caller believed.
        presented: LeaseEpoch,
        /// What is true.
        current: LeaseEpoch,
    },
    /// The lease is past its `expires_at`. Authority does not survive its own
    /// deadline, and no timeout confers a new one.
    Expired {
        /// The deadline that has passed.
        expired_at: DateTime<Utc>,
    },
}

impl RefusalReason {
    /// A one-line explanation an operator can act on.
    #[must_use]
    pub fn explain(&self) -> String {
        match self {
            Self::NoLease => {
                "no lease on this mission — nobody is PRIMARY, so nobody may pilot".to_owned()
            }
            Self::EpochNotPresented => {
                "no epoch presented — a gesture must carry the epoch it believes it holds"
                    .to_owned()
            }
            Self::NotHolder { holder } => {
                format!("the lease is held by {}", holder.as_str())
            }
            Self::EpochMismatch { presented, current } => {
                format!("stale epoch {presented} — the mission is at epoch {current}")
            }
            Self::Expired { expired_at } => {
                format!("the lease expired at {expired_at} and confers nothing after that")
            }
        }
    }
}

/// The verdict of the guard. Binary, unlike `cs diverge`'s tri-value: an
/// unknown authority is not a third state, it is a refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum LeaseDecision {
    /// The gesture may proceed, at this epoch.
    Granted {
        /// The epoch the caller was authorised at.
        epoch: LeaseEpoch,
    },
    /// The gesture is refused, before it takes effect.
    Refused(RefusalReason),
}

impl LeaseDecision {
    /// Return `true` iff the gesture may proceed.
    #[must_use]
    pub fn is_granted(&self) -> bool {
        matches!(self, Self::Granted { .. })
    }

    /// The refusal reason, if this is a refusal.
    #[must_use]
    pub fn refusal(&self) -> Option<&RefusalReason> {
        match self {
            Self::Refused(r) => Some(r),
            Self::Granted { .. } => None,
        }
    }
}

/// The epoch a pilot's seat presents *for `mission`*, given the authority that
/// seat claims.
///
/// `claim` is what a presence snapshot advertises — the pair returned by
/// `Presence::claimed_authority`: the mission the seat is about, and the epoch
/// it believes it holds there. This function is the one rule that turns a seat
/// into an argument for [`authorize`]: **a claim on one mission presents
/// nothing on another.**
///
/// It exists as its own function because that rule is easy to lose at a
/// call-site. A pilot legitimately PRIMARY on mission A, typing a lifecycle
/// verb against mission B, would otherwise hand A's epoch to B's guard; if A
/// and B happened to be at the same epoch number the gesture would be granted
/// on an authority nobody ever conferred. Filtering by mission first makes
/// that a refusal (`EpochNotPresented`) rather than a coincidence.
///
/// # Examples
///
/// ```
/// use cosmon_core::id::MoleculeId;
/// use cosmon_core::pilot_lease::{epoch_presented_for, LeaseEpoch};
///
/// let a = MoleculeId::new("task-20260731-9cf4").unwrap();
/// let b = MoleculeId::new("task-20260724-4cef").unwrap();
/// let e = LeaseEpoch::first();
///
/// assert_eq!(epoch_presented_for(&a, Some((&a, e))), Some(e));
/// assert_eq!(epoch_presented_for(&b, Some((&a, e))), None);
/// assert_eq!(epoch_presented_for(&a, None), None);
/// ```
#[must_use]
pub fn epoch_presented_for(
    mission: &MoleculeId,
    claim: Option<(&MoleculeId, LeaseEpoch)>,
) -> Option<LeaseEpoch> {
    claim.and_then(|(claimed, epoch)| (claimed == mission).then_some(epoch))
}

/// Decide whether `session`, believing it holds `presented_epoch`, may issue a
/// piloting gesture on the mission whose current lease is `lease`.
///
/// Pure, total and fail-closed: every path that is not "the current holder
/// presenting the current epoch inside the validity window" is a refusal, and
/// the refusal is produced *before* the caller does anything.
///
/// The order of the checks is itself load-bearing. Existence comes first, then
/// expiry, then identity, then the epoch — so a caller who is not the holder
/// of an expired lease is told the lease expired rather than being told to go
/// talk to a session that no longer has the seat either.
#[must_use]
pub fn authorize(
    lease: Option<&PilotLease>,
    now: DateTime<Utc>,
    session: &SessionId,
    presented_epoch: Option<LeaseEpoch>,
) -> LeaseDecision {
    let Some(lease) = lease else {
        return LeaseDecision::Refused(RefusalReason::NoLease);
    };
    if !lease.is_valid_at(now) {
        // `is_valid_at` is false only when there is a deadline, so the
        // `unwrap_or` arm is unreachable; it exists so this function has no
        // panicking path at all.
        return LeaseDecision::Refused(RefusalReason::Expired {
            expired_at: lease.expires_at.unwrap_or(now),
        });
    }
    if &lease.holder_session_id != session {
        return LeaseDecision::Refused(RefusalReason::NotHolder {
            holder: lease.holder_session_id.clone(),
        });
    }
    let Some(presented) = presented_epoch else {
        return LeaseDecision::Refused(RefusalReason::EpochNotPresented);
    };
    if presented != lease.epoch {
        return LeaseDecision::Refused(RefusalReason::EpochMismatch {
            presented,
            current: lease.epoch,
        });
    }
    LeaseDecision::Granted { epoch: lease.epoch }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn mission() -> MoleculeId {
        MoleculeId::new("task-20260731-9cf4").unwrap()
    }

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, 9, 0, 0).unwrap()
    }

    fn lease_held_by(holder: &str, epoch: LeaseEpoch) -> PilotLease {
        PilotLease::new(mission(), sid(holder), epoch, "operator", t0(), None)
    }

    #[test]
    fn an_epoch_starts_at_one_and_zero_is_not_an_authority() {
        assert_eq!(LeaseEpoch::first().get(), 1);
        assert!(LeaseEpoch::new(0).is_err());
        assert_eq!(LeaseEpoch::new(7).unwrap().get(), 7);
    }

    #[test]
    fn an_epoch_only_moves_forward() {
        let e = LeaseEpoch::first();
        assert!(e < e.next());
        assert_eq!(e.next().next().get(), 3);
    }

    // FAIL-CLOSED-AUTHORITY, in its bluntest form: the state of the world
    // before any grant authorises nothing at all.
    #[test]
    fn no_lease_authorises_nobody() {
        let d = authorize(None, t0(), &sid("claude"), Some(LeaseEpoch::first()));
        assert_eq!(d, LeaseDecision::Refused(RefusalReason::NoLease));
        assert!(!d.is_granted());
    }

    #[test]
    fn the_holder_presenting_the_current_epoch_may_act() {
        let lease = lease_held_by("claude", LeaseEpoch::first());
        let d = authorize(
            Some(&lease),
            t0(),
            &sid("claude"),
            Some(LeaseEpoch::first()),
        );
        assert_eq!(
            d,
            LeaseDecision::Granted {
                epoch: LeaseEpoch::first()
            }
        );
    }

    // PRIMARY-UNIQUE: the acceptance clause "tentative concurrente refusée".
    // Two sessions read the same current epoch; only one of them holds it.
    #[test]
    fn a_concurrent_attempt_by_a_non_holder_is_refused() {
        let lease = lease_held_by("claude", LeaseEpoch::first());
        let d = authorize(Some(&lease), t0(), &sid("codex"), Some(LeaseEpoch::first()));
        assert_eq!(
            d,
            LeaseDecision::Refused(RefusalReason::NotHolder {
                holder: sid("claude")
            })
        );
    }

    // The acceptance clause "ancien primaire refusé après transfert". The old
    // primary is still presenting the epoch it held, and it is now stale.
    #[test]
    fn the_former_primary_is_refused_after_a_transfer() {
        let before = lease_held_by("claude", LeaseEpoch::first());
        assert!(authorize(
            Some(&before),
            t0(),
            &sid("claude"),
            Some(LeaseEpoch::first())
        )
        .is_granted());

        let after = lease_held_by("codex", LeaseEpoch::first().next());
        let d = authorize(
            Some(&after),
            t0(),
            &sid("claude"),
            Some(LeaseEpoch::first()),
        );
        assert_eq!(
            d,
            LeaseDecision::Refused(RefusalReason::NotHolder {
                holder: sid("codex")
            })
        );
        // And the new primary is authorised at the new epoch, not the old one.
        assert!(authorize(
            Some(&after),
            t0(),
            &sid("codex"),
            Some(LeaseEpoch::first().next())
        )
        .is_granted());
    }

    // The holder that never noticed a re-grant to *itself* — a lease renewed
    // at a new epoch — is refused just the same. Holding the seat is not
    // enough; the gesture must name the generation it was written against.
    #[test]
    fn a_stale_epoch_is_refused_even_for_the_holder() {
        let lease = lease_held_by("claude", LeaseEpoch::new(4).unwrap());
        let d = authorize(
            Some(&lease),
            t0(),
            &sid("claude"),
            Some(LeaseEpoch::new(3).unwrap()),
        );
        assert_eq!(
            d,
            LeaseDecision::Refused(RefusalReason::EpochMismatch {
                presented: LeaseEpoch::new(3).unwrap(),
                current: LeaseEpoch::new(4).unwrap(),
            })
        );
    }

    // A *future* epoch is refused by the same equality check. Being ahead of
    // the ledger is not evidence of authority; it is evidence of a guess.
    #[test]
    fn an_epoch_ahead_of_the_ledger_is_refused_too() {
        let lease = lease_held_by("claude", LeaseEpoch::first());
        let d = authorize(
            Some(&lease),
            t0(),
            &sid("claude"),
            Some(LeaseEpoch::new(99).unwrap()),
        );
        assert!(matches!(
            d,
            LeaseDecision::Refused(RefusalReason::EpochMismatch { .. })
        ));
    }

    #[test]
    fn a_gesture_that_names_no_epoch_is_refused() {
        let lease = lease_held_by("claude", LeaseEpoch::first());
        let d = authorize(Some(&lease), t0(), &sid("claude"), None);
        assert_eq!(d, LeaseDecision::Refused(RefusalReason::EpochNotPresented));
    }

    #[test]
    fn authority_does_not_outlive_its_deadline() {
        let deadline = t0() + Duration::minutes(30);
        let lease = PilotLease::new(
            mission(),
            sid("claude"),
            LeaseEpoch::first(),
            "operator",
            t0(),
            Some(deadline),
        );
        assert!(lease.is_valid_at(t0()));
        // Exactly at the deadline is already expired — pins the comparator.
        assert!(!lease.is_valid_at(deadline));
        let d = authorize(
            Some(&lease),
            deadline,
            &sid("claude"),
            Some(LeaseEpoch::first()),
        );
        assert_eq!(
            d,
            LeaseDecision::Refused(RefusalReason::Expired {
                expired_at: deadline
            })
        );
    }

    // Expiry is checked before identity so the refusal names the fact that
    // actually blocks progress.
    #[test]
    fn an_expired_lease_reads_as_expired_even_to_a_stranger() {
        let deadline = t0() + Duration::minutes(1);
        let lease = PilotLease::new(
            mission(),
            sid("claude"),
            LeaseEpoch::first(),
            "operator",
            t0(),
            Some(deadline),
        );
        let d = authorize(
            Some(&lease),
            deadline + Duration::hours(1),
            &sid("codex"),
            Some(LeaseEpoch::first()),
        );
        assert!(matches!(
            d,
            LeaseDecision::Refused(RefusalReason::Expired { .. })
        ));
    }

    #[test]
    fn a_request_id_is_a_function_of_the_request_and_of_nothing_else() {
        let a = RequestId::derive(&mission(), &sid("codex"), Some(LeaseEpoch::first()), t0());
        let b = RequestId::derive(&mission(), &sid("codex"), Some(LeaseEpoch::first()), t0());
        assert_eq!(a, b, "a retry must recompute the same id");

        // Each component participates.
        assert_ne!(
            a,
            RequestId::derive(&mission(), &sid("claude"), Some(LeaseEpoch::first()), t0())
        );
        assert_ne!(
            a,
            RequestId::derive(
                &mission(),
                &sid("codex"),
                Some(LeaseEpoch::first().next()),
                t0()
            )
        );
        assert_ne!(
            a,
            RequestId::derive(
                &mission(),
                &sid("codex"),
                Some(LeaseEpoch::first()),
                t0() + Duration::seconds(1)
            )
        );
        // "Saw no lease" and "saw epoch 1" are different questions.
        assert_ne!(a, RequestId::derive(&mission(), &sid("codex"), None, t0()));
    }

    #[test]
    fn a_request_id_may_not_hold_whitespace() {
        assert!(RequestId::new("").is_err());
        assert!(RequestId::new("req one").is_err());
        assert!(RequestId::new("req-ok").is_ok());
    }

    // A request records what the world looked like when it was written, so
    // the operator can see a stale ask for what it is.
    #[test]
    fn a_request_records_the_lease_it_was_written_against() {
        let held = lease_held_by("claude", LeaseEpoch::first());
        let r = LeaseRequest::new(
            mission(),
            sid("codex"),
            sid("codex"),
            Some(&held),
            t0(),
            "claude is near its window limit",
        );
        assert_eq!(r.observed_holder, Some(sid("claude")));
        assert_eq!(r.observed_epoch, Some(LeaseEpoch::first()));
        assert_eq!(r.candidate_session_id, sid("codex"));
    }

    #[test]
    fn a_request_confers_no_authority_by_itself() {
        // The whole point of M4's "crash between request and grant" clause:
        // there is no code path from a LeaseRequest to a LeaseDecision.
        let r = LeaseRequest::new(mission(), sid("codex"), sid("codex"), None, t0(), "");
        assert!(!authorize(None, t0(), &r.candidate_session_id, None).is_granted());
    }

    #[test]
    fn records_round_trip_through_json() {
        let lease = lease_held_by("claude", LeaseEpoch::new(3).unwrap());
        let json = serde_json::to_string(&lease).unwrap();
        assert!(!json.contains("expires_at"), "a None deadline is omitted");
        assert!(!json.contains("request_id"), "an unprompted grant is bare");
        assert_eq!(serde_json::from_str::<PilotLease>(&json).unwrap(), lease);

        let req = LeaseRequest::new(
            mission(),
            sid("codex"),
            sid("codex"),
            Some(&lease),
            t0(),
            "",
        );
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<LeaseRequest>(&json).unwrap(), req);

        // The epoch is transparent on the wire — a bare integer, so `jq
        // '.epoch'` reads a number and not an object.
        assert!(
            serde_json::to_string(&LeaseEpoch::new(3).unwrap()).unwrap() == "3",
            "epoch must serialise as a bare integer"
        );
    }

    #[test]
    fn a_grant_can_name_the_request_it_answers() {
        let req = LeaseRequest::new(mission(), sid("codex"), sid("codex"), None, t0(), "");
        let lease = lease_held_by("codex", LeaseEpoch::first()).answering(&req);
        assert_eq!(lease.request_id.as_ref(), Some(&req.id));
    }

    #[test]
    fn every_refusal_explains_itself_in_one_line() {
        for reason in [
            RefusalReason::NoLease,
            RefusalReason::EpochNotPresented,
            RefusalReason::NotHolder {
                holder: sid("claude"),
            },
            RefusalReason::EpochMismatch {
                presented: LeaseEpoch::first(),
                current: LeaseEpoch::first().next(),
            },
            RefusalReason::Expired { expired_at: t0() },
        ] {
            let line = reason.explain();
            assert!(!line.is_empty());
            assert!(!line.contains('\n'), "one line: {line:?}");
        }
    }

    #[test]
    fn the_challenge_of_a_grant_names_that_grant_and_no_other() {
        let lease = lease_held_by("claude", LeaseEpoch::first());
        let c = lease
            .challenge()
            .expect("a lease rebuilds its own challenge");
        assert_eq!(c.mission_id, lease.mission_id);
        assert_eq!(c.holder_session_id, lease.holder_session_id);
        assert_eq!(c.epoch, lease.epoch);
        assert_eq!(c.granted_by, lease.granted_by);
        assert_eq!(c.ttl_seconds, None);
    }

    #[test]
    fn a_ttl_survives_the_round_trip_through_expires_at() {
        let lease = PilotLease::new(
            mission(),
            sid("claude"),
            LeaseEpoch::first(),
            "operator",
            t0(),
            Some(t0() + Duration::seconds(900)),
        );
        assert_eq!(lease.challenge().expect("challenge").ttl_seconds, Some(900));
    }

    #[test]
    fn a_grant_carries_no_attestation_until_one_is_attached() {
        let lease = lease_held_by("claude", LeaseEpoch::first());
        assert!(lease.attestation.is_none());
    }
}
