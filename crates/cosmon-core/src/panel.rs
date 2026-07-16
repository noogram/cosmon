// SPDX-License-Identifier: AGPL-3.0-only

//! Constitutional panel — hash-pinned supermajority deliberation.
//!
//! A **panel** is a deliberative body convened to approve or refuse a
//! change whose cost-of-amendment we deliberately want to raise. It is the
//! cosmon-side primitive behind the *constitutional ratchet*: some bullets
//! of a galaxy's DNA — anything `operator-uncapturable`,
//! a `forbid_operator_*` lint, or the addition of an `operator_*` field —
//! must not be amendable by a single pull request. By routing such an
//! amendment through a panel, its cost rises from `O(1 PR)` to
//! `O(panel convocation)`: what was *legislative* (one author, one merge)
//! becomes *constitutional* (a supermajority of named perspectives, on the
//! record).
//!
//! # The two anti-capture mechanisms
//!
//! The whole point is that the convener must not be able to *stack* the
//! panel in their own favour — the "audience-after-the-test" pathology:
//! choosing your judges *after* you have seen (and written) the question
//! they will judge.
//!
//! 1. **Hash-pinned composition.** A panel has a fixed constitutional
//!    *core* plus one or more *rotating* seats. The rotating seat is filled
//!    deterministically from a pool by hashing the artifact under review
//!    (e.g. the PR diff). Because the seat is a pure function of the diff,
//!    the convener cannot pick a friendly fifth voter after seeing what
//!    they want to pass — the diff itself names the judge.
//! 2. **Exact-quorum tally.** [`tally`] refuses to count a ballot from
//!    anyone who is not on the convened panel, and refuses to render a
//!    verdict until every seated persona has voted. You cannot pad the
//!    panel with extra approvers, nor drop a dissenter by omission.
//!
//! The verdict requires a **supermajority** ([`SupermajorityRule`],
//! default 4/5): for the canonical five-seat panel, at least four of five
//! must approve. The rule scales with `ceil(num·N / den)`, so a larger
//! panel needs proportionally more approvals — never a bare majority.
//!
//! # Zero I/O
//!
//! Like the rest of `cosmon-core`, this module is pure. The artifact hash
//! is supplied by the caller (the CLI hashes the diff bytes); the
//! [`RoleLog`] is returned as a value for the caller to inscribe to disk.
//! Nothing here reads files, spawns personas, or invokes an LLM — those
//! belong to the `cs panel` command and to future dispatch.

use std::fmt;
use std::str::FromStr;

use cosmon_hash::Hash;
use serde::{Deserialize, Serialize};

/// Minimum number of seats on a constitutional panel.
///
/// The spec is "≥4-of-N≥5": a panel smaller than five cannot express a
/// 4/5 supermajority and is therefore not constitutional. [`PanelRoster::validate`]
/// rejects any roster whose `core + rotating_seats` falls below this.
pub const MIN_PANEL_SIZE: usize = 5;

/// A panel persona, identified by its short-name (e.g. `"wheeler"`).
///
/// Personas are the named perspectives the panel mobilises. The short-name
/// matches the canonical persona directory under
/// `/srv/cosmon/workshop/personas/<name>/`, mirroring the `deep-think`
/// formula's `panel=` vocabulary, but this type carries no opinion about
/// where the persona's prose lives — it is just a stable identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Persona(String);

impl Persona {
    /// Construct a persona from a short-name.
    ///
    /// The name is trimmed; an empty name is rejected because an anonymous
    /// seat would defeat the on-the-record property of the role-log.
    ///
    /// # Errors
    ///
    /// Returns [`PanelError::EmptyPersona`] if the trimmed name is empty.
    pub fn new(name: impl Into<String>) -> Result<Self, PanelError> {
        let name = name.into();
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(PanelError::EmptyPersona);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The persona's short-name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Persona {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Persona {
    type Err = PanelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// A single persona's verdict on the artifact under review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vote {
    /// The persona approves the amendment.
    Approve,
    /// The persona refuses the amendment.
    Refuse,
}

impl fmt::Display for Vote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approve => f.write_str("approve"),
            Self::Refuse => f.write_str("refuse"),
        }
    }
}

impl FromStr for Vote {
    type Err = PanelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "approve" | "approved" | "yes" | "aye" => Ok(Self::Approve),
            "refuse" | "refused" | "reject" | "no" | "nay" => Ok(Self::Refuse),
            other => Err(PanelError::UnknownVote(other.to_owned())),
        }
    }
}

/// One persona's recorded ballot — vote plus optional written rationale.
///
/// The rationale is what turns a tally into a *role-log*: a future auditor
/// can read why each seated perspective decided as it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ballot {
    /// The persona casting this ballot.
    pub persona: Persona,
    /// The persona's verdict.
    pub vote: Vote,
    /// Optional one-line rationale inscribed alongside the vote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Ballot {
    /// Construct a ballot.
    #[must_use]
    pub fn new(persona: Persona, vote: Vote, reason: Option<String>) -> Self {
        Self {
            persona,
            vote,
            reason,
        }
    }
}

/// A supermajority threshold expressed as a fraction of the panel size.
///
/// The default is 4/5 — the canonical "≥4-of-N≥5" rule. The required
/// approval count for a panel of `n` seats is `ceil(numerator·n / denominator)`,
/// so the threshold scales with the panel and is always strictly above a
/// bare majority for any fraction greater than 1/2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupermajorityRule {
    /// Numerator of the supermajority fraction.
    pub numerator: u32,
    /// Denominator of the supermajority fraction.
    pub denominator: u32,
}

impl Default for SupermajorityRule {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 5,
        }
    }
}

impl SupermajorityRule {
    /// The number of approvals required to carry a panel of `panel_size`.
    ///
    /// Computed as `ceil(numerator · panel_size / denominator)` and clamped
    /// to at most `panel_size` (a fraction `>1` cannot demand more votes
    /// than there are seats).
    #[must_use]
    pub fn required(self, panel_size: usize) -> usize {
        if self.denominator == 0 {
            return panel_size;
        }
        let size = u128::try_from(panel_size).unwrap_or(u128::MAX);
        let num = u128::from(self.numerator) * size;
        let den = u128::from(self.denominator);
        let ceil = num.div_ceil(den);
        usize::try_from(ceil).unwrap_or(panel_size).min(panel_size)
    }
}

/// Errors arising from panel construction, convocation, or tallying.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PanelError {
    /// A persona short-name was empty after trimming.
    #[error("persona short-name must not be empty")]
    EmptyPersona,

    /// A vote string was not recognised.
    #[error("unknown vote '{0}' — expected 'approve' or 'refuse'")]
    UnknownVote(String),

    /// The roster would seat fewer than [`MIN_PANEL_SIZE`] personas.
    #[error(
        "panel too small: {got} seats (core {core} + {rotating} rotating), \
         constitutional minimum is {min}"
    )]
    PanelTooSmall {
        /// Total seats the roster would fill.
        got: usize,
        /// Core seat count.
        core: usize,
        /// Rotating seat count.
        rotating: usize,
        /// The constitutional minimum.
        min: usize,
    },

    /// The rotation pool is too small to fill the rotating seats.
    #[error("rotation pool has {pool} personas but {rotating} rotating seats must be filled")]
    PoolTooSmall {
        /// Pool size.
        pool: usize,
        /// Rotating seats requested.
        rotating: usize,
    },

    /// A persona appears more than once across core and pool.
    #[error("persona '{0}' appears more than once in the roster")]
    DuplicatePersona(String),

    /// A ballot was cast by someone who is not on the convened panel.
    #[error("ballot from '{0}' who is not a seated panelist — cannot pad the panel")]
    UnseatedBallot(String),

    /// A seated persona voted more than once.
    #[error("persona '{0}' cast more than one ballot")]
    DuplicateBallot(String),

    /// Not every seated persona has voted yet.
    #[error("incomplete panel: {voted}/{seats} seated personas have voted")]
    IncompleteQuorum {
        /// Ballots received from seated personas.
        voted: usize,
        /// Total seats that must vote.
        seats: usize,
    },
}

/// The set of personas available to a constitutional panel.
///
/// A roster declares a fixed `core` (always seated) and a `pool` from which
/// `rotating_seats` are filled by hashing the artifact under review. The
/// [`SupermajorityRule`] governs how many approvals carry a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelRoster {
    /// Personas that are always seated (the constitutional core).
    pub core: Vec<Persona>,
    /// Personas eligible for the rotating seat(s), selected by diff hash.
    pub pool: Vec<Persona>,
    /// How many seats are filled from the pool by hash.
    pub rotating_seats: usize,
    /// The supermajority rule for the verdict.
    #[serde(default)]
    pub rule: SupermajorityRule,
}

impl PanelRoster {
    /// Construct and validate a roster.
    ///
    /// # Errors
    ///
    /// Returns a [`PanelError`] if the roster is structurally invalid (too
    /// small, duplicate personas, or an under-sized pool). See
    /// [`PanelRoster::validate`].
    pub fn new(
        core: Vec<Persona>,
        pool: Vec<Persona>,
        rotating_seats: usize,
        rule: SupermajorityRule,
    ) -> Result<Self, PanelError> {
        let roster = Self {
            core,
            pool,
            rotating_seats,
            rule,
        };
        roster.validate()?;
        Ok(roster)
    }

    /// Total number of seats this roster fills (`core + rotating`).
    #[must_use]
    pub fn panel_size(&self) -> usize {
        self.core.len() + self.rotating_seats
    }

    /// Check the roster's structural invariants.
    ///
    /// A valid roster (a) seats at least [`MIN_PANEL_SIZE`] personas,
    /// (b) has a pool large enough to fill its rotating seats, and
    /// (c) contains no persona twice across core and pool.
    ///
    /// # Errors
    ///
    /// Returns the first violated invariant as a [`PanelError`].
    pub fn validate(&self) -> Result<(), PanelError> {
        let size = self.panel_size();
        if size < MIN_PANEL_SIZE {
            return Err(PanelError::PanelTooSmall {
                got: size,
                core: self.core.len(),
                rotating: self.rotating_seats,
                min: MIN_PANEL_SIZE,
            });
        }
        if self.pool.len() < self.rotating_seats {
            return Err(PanelError::PoolTooSmall {
                pool: self.pool.len(),
                rotating: self.rotating_seats,
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in self.core.iter().chain(self.pool.iter()) {
            if !seen.insert(p.as_str()) {
                return Err(PanelError::DuplicatePersona(p.as_str().to_owned()));
            }
        }
        Ok(())
    }

    /// Convene the panel for a specific artifact, identified by its hash.
    ///
    /// The core is always seated. The rotating seat(s) are filled
    /// deterministically: for seat `i`, the index into the (shrinking) pool
    /// is derived by re-hashing `artifact_hash ‖ i` and reducing modulo the
    /// remaining pool size. Selection is without replacement, so two
    /// rotating seats never draw the same persona.
    ///
    /// The result is a pure function of `(roster, artifact_hash)` — the
    /// hash-pinned composition that makes audience-after-the-test
    /// impossible.
    ///
    /// # Errors
    ///
    /// Returns a [`PanelError`] if the roster fails [`PanelRoster::validate`].
    pub fn convene(&self, artifact_hash: &Hash) -> Result<PanelComposition, PanelError> {
        self.validate()?;

        let mut remaining: Vec<Persona> = self.pool.clone();
        let mut pinned: Vec<Persona> = Vec::with_capacity(self.rotating_seats);
        for seat in 0..self.rotating_seats {
            let idx = derive_seat_index(artifact_hash, seat, remaining.len());
            pinned.push(remaining.remove(idx));
        }

        let mut seated = self.core.clone();
        seated.extend(pinned.iter().cloned());

        Ok(PanelComposition {
            artifact_hash: *artifact_hash,
            core: self.core.clone(),
            pinned,
            seated,
            rule: self.rule,
        })
    }
}

/// Derive a pool index for rotating seat `seat` from the artifact hash.
///
/// We re-hash `artifact_hash_bytes ‖ seat_le_bytes` so that each seat draws
/// from an independent, uniform position, then reduce modulo `pool_len`.
/// `pool_len` is guaranteed non-zero by the caller (selection without
/// replacement stops once the rotating seats are filled, and
/// [`PanelRoster::validate`] guarantees the pool is large enough).
fn derive_seat_index(artifact_hash: &Hash, seat: usize, pool_len: usize) -> usize {
    debug_assert!(pool_len > 0, "pool must be non-empty for seat selection");
    let mut buf = Vec::with_capacity(32 + 8);
    buf.extend_from_slice(artifact_hash.as_bytes());
    buf.extend_from_slice(&u64::try_from(seat).unwrap_or(u64::MAX).to_le_bytes());
    let sub = Hash::of_bytes(&buf);
    let bytes = sub.as_bytes();
    let mut acc = [0u8; 8];
    acc.copy_from_slice(&bytes[..8]);
    let n = u64::from_le_bytes(acc);
    let modulus = u64::try_from(pool_len).unwrap_or(u64::MAX);
    // `n % modulus` < modulus ≤ pool_len, so the result always fits in usize.
    usize::try_from(n % modulus).unwrap_or(0)
}

/// The seated panel for one artifact — output of [`PanelRoster::convene`].
///
/// Carries the artifact hash that pinned the composition, the core, the
/// hash-selected rotating personas, and the full seated set. The `seated`
/// field is the authoritative quorum that [`tally`] checks ballots against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelComposition {
    /// Hash of the artifact (e.g. PR diff) that pinned this composition.
    pub artifact_hash: Hash,
    /// The constitutional core (always seated).
    pub core: Vec<Persona>,
    /// The rotating personas selected by the artifact hash.
    pub pinned: Vec<Persona>,
    /// The full seated panel (`core` followed by `pinned`).
    pub seated: Vec<Persona>,
    /// The supermajority rule that will govern the verdict.
    pub rule: SupermajorityRule,
}

impl PanelComposition {
    /// Number of seated personas.
    #[must_use]
    pub fn size(&self) -> usize {
        self.seated.len()
    }

    /// Approvals required to carry this panel under its rule.
    #[must_use]
    pub fn required(&self) -> usize {
        self.rule.required(self.size())
    }

    /// Whether `persona` holds a seat on this panel.
    #[must_use]
    pub fn is_seated(&self, persona: &Persona) -> bool {
        self.seated.contains(persona)
    }
}

/// The final, binary outcome of a panel deliberation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelVerdict {
    /// The supermajority approved the amendment.
    Approve,
    /// The supermajority was not reached; the amendment is refused.
    Refuse,
}

impl fmt::Display for PanelVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approve => f.write_str("approve"),
            Self::Refuse => f.write_str("refuse"),
        }
    }
}

/// The inscribed record of a completed panel deliberation.
///
/// This is the *role-log*: the artifact hash that pinned the panel, the
/// full ballot of every seated persona, the supermajority arithmetic, and
/// the resulting verdict. It is the durable proof that an amendment paid the
/// constitutional cost — a future auditor reads it to confirm the panel was
/// convened from the diff (not stacked) and that the supermajority held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleLog {
    /// Hash of the artifact under review.
    pub artifact_hash: Hash,
    /// The convened composition (core + hash-pinned rotating seats).
    pub composition: PanelComposition,
    /// Every seated persona's ballot, in seated order.
    pub ballots: Vec<Ballot>,
    /// Approvals counted.
    pub approvals: usize,
    /// Refusals counted.
    pub refusals: usize,
    /// Approvals required by the supermajority rule.
    pub required: usize,
    /// The verdict.
    pub verdict: PanelVerdict,
}

/// Tally a panel's ballots into a [`RoleLog`].
///
/// Enforces the exact-quorum invariant: every ballot must come from a
/// seated persona, no persona may vote twice, and every seat must have
/// voted. These three checks are what stop a convener from padding the
/// panel with friendly extra approvers or silently dropping a dissenter.
///
/// The verdict is `Approve` iff approvals reach the supermajority threshold
/// ([`SupermajorityRule::required`]); otherwise `Refuse`.
///
/// # Errors
///
/// - [`PanelError::UnseatedBallot`] — a ballot from a non-panelist.
/// - [`PanelError::DuplicateBallot`] — a seated persona voted twice.
/// - [`PanelError::IncompleteQuorum`] — not every seat has voted.
pub fn tally(composition: &PanelComposition, ballots: &[Ballot]) -> Result<RoleLog, PanelError> {
    // Every ballot must be from a seated persona, and no double-voting.
    let mut voted: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for b in ballots {
        if !composition.is_seated(&b.persona) {
            return Err(PanelError::UnseatedBallot(b.persona.as_str().to_owned()));
        }
        if !voted.insert(b.persona.as_str()) {
            return Err(PanelError::DuplicateBallot(b.persona.as_str().to_owned()));
        }
    }

    // Every seat must have voted.
    if voted.len() != composition.size() {
        return Err(PanelError::IncompleteQuorum {
            voted: voted.len(),
            seats: composition.size(),
        });
    }

    // Order ballots by seated order for a stable, readable role-log.
    let mut ordered: Vec<Ballot> = Vec::with_capacity(ballots.len());
    for seat in &composition.seated {
        if let Some(b) = ballots.iter().find(|b| &b.persona == seat) {
            ordered.push(b.clone());
        }
    }

    let approvals = ordered.iter().filter(|b| b.vote == Vote::Approve).count();
    let refusals = ordered.len() - approvals;
    let required = composition.required();
    let verdict = if approvals >= required {
        PanelVerdict::Approve
    } else {
        PanelVerdict::Refuse
    };

    Ok(RoleLog {
        artifact_hash: composition.artifact_hash,
        composition: composition.clone(),
        ballots: ordered,
        approvals,
        refusals,
        required,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(s: &str) -> Persona {
        Persona::new(s).unwrap()
    }

    fn canonical_roster() -> PanelRoster {
        PanelRoster::new(
            vec![
                persona("wheeler"),
                persona("torvalds"),
                persona("feynman"),
                persona("shannon"),
            ],
            vec![
                persona("jobs"),
                persona("jr"),
                persona("godel"),
                persona("einstein"),
            ],
            1,
            SupermajorityRule::default(),
        )
        .unwrap()
    }

    #[test]
    fn supermajority_4_of_5_is_four() {
        let rule = SupermajorityRule::default();
        assert_eq!(rule.required(5), 4);
    }

    #[test]
    fn supermajority_scales_above_majority() {
        let rule = SupermajorityRule::default();
        // 6 seats → ceil(24/5)=5; 7 → ceil(28/5)=6; 10 → 8.
        assert_eq!(rule.required(6), 5);
        assert_eq!(rule.required(7), 6);
        assert_eq!(rule.required(10), 8);
        // Always strictly above a bare majority.
        for n in MIN_PANEL_SIZE..=20 {
            assert!(rule.required(n) > n / 2, "n={n}");
        }
    }

    #[test]
    fn roster_rejects_sub_minimum_panel() {
        let err = PanelRoster::new(
            vec![persona("a"), persona("b")],
            vec![persona("c")],
            1,
            SupermajorityRule::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PanelError::PanelTooSmall { .. }));
    }

    #[test]
    fn roster_rejects_undersized_pool() {
        let err = PanelRoster::new(
            vec![persona("a"), persona("b"), persona("c"), persona("d")],
            vec![persona("e")],
            2,
            SupermajorityRule::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PanelError::PoolTooSmall { .. }));
    }

    #[test]
    fn roster_rejects_duplicate_persona() {
        let err = PanelRoster::new(
            vec![persona("a"), persona("b"), persona("c"), persona("d")],
            vec![persona("a"), persona("e")],
            1,
            SupermajorityRule::default(),
        )
        .unwrap_err();
        assert!(matches!(err, PanelError::DuplicatePersona(_)));
    }

    #[test]
    fn convocation_is_deterministic_for_a_diff() {
        let roster = canonical_roster();
        let h = Hash::of_bytes(b"diff --git a/dna.md b/dna.md\n+forbid_operator_capture");
        let a = roster.convene(&h).unwrap();
        let b = roster.convene(&h).unwrap();
        assert_eq!(a, b, "same diff must convene the identical panel");
        assert_eq!(a.size(), 5);
        assert_eq!(a.pinned.len(), 1);
    }

    #[test]
    fn different_diffs_can_pin_different_rotating_seats() {
        let roster = canonical_roster();
        // Find two diffs whose pinned seat differs — proves the seat is a
        // function of the diff, not a constant.
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..64u32 {
            let h = Hash::of_bytes(&i.to_le_bytes());
            let comp = roster.convene(&h).unwrap();
            seen.insert(comp.pinned[0].as_str().to_owned());
        }
        assert!(
            seen.len() > 1,
            "hash-pinning must reach more than one pool persona across diffs"
        );
    }

    #[test]
    fn core_is_always_seated() {
        let roster = canonical_roster();
        for i in 0..32u32 {
            let h = Hash::of_bytes(&i.to_le_bytes());
            let comp = roster.convene(&h).unwrap();
            for c in &roster.core {
                assert!(comp.is_seated(c), "core persona {c} must always be seated");
            }
        }
    }

    #[test]
    fn two_rotating_seats_are_distinct() {
        let roster = PanelRoster::new(
            vec![persona("wheeler"), persona("torvalds"), persona("feynman")],
            vec![
                persona("jobs"),
                persona("jr"),
                persona("godel"),
                persona("einstein"),
            ],
            2,
            SupermajorityRule::default(),
        )
        .unwrap();
        for i in 0..32u32 {
            let h = Hash::of_bytes(&i.to_le_bytes());
            let comp = roster.convene(&h).unwrap();
            assert_eq!(comp.size(), 5);
            assert_ne!(comp.pinned[0], comp.pinned[1], "no persona seated twice");
        }
    }

    #[test]
    fn tally_approves_on_supermajority() {
        let roster = canonical_roster();
        let h = Hash::of_bytes(b"amend");
        let comp = roster.convene(&h).unwrap();
        let ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // 4 approve, 1 refuse → carries.
                let v = if i == 0 { Vote::Refuse } else { Vote::Approve };
                Ballot::new(p.clone(), v, None)
            })
            .collect();
        let log = tally(&comp, &ballots).unwrap();
        assert_eq!(log.approvals, 4);
        assert_eq!(log.refusals, 1);
        assert_eq!(log.required, 4);
        assert_eq!(log.verdict, PanelVerdict::Approve);
    }

    #[test]
    fn tally_refuses_below_supermajority() {
        let roster = canonical_roster();
        let h = Hash::of_bytes(b"amend2");
        let comp = roster.convene(&h).unwrap();
        let ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // Only 3 approve → refused.
                let v = if i < 3 { Vote::Approve } else { Vote::Refuse };
                Ballot::new(p.clone(), v, None)
            })
            .collect();
        let log = tally(&comp, &ballots).unwrap();
        assert_eq!(log.approvals, 3);
        assert_eq!(log.verdict, PanelVerdict::Refuse);
    }

    #[test]
    fn tally_rejects_unseated_ballot() {
        let roster = canonical_roster();
        let comp = roster.convene(&Hash::of_bytes(b"x")).unwrap();
        let mut ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .map(|p| Ballot::new(p.clone(), Vote::Approve, None))
            .collect();
        // Pad with a friendly outsider — must be rejected.
        ballots.push(Ballot::new(persona("stranger"), Vote::Approve, None));
        let err = tally(&comp, &ballots).unwrap_err();
        assert!(matches!(err, PanelError::UnseatedBallot(_)));
    }

    #[test]
    fn tally_rejects_incomplete_quorum() {
        let roster = canonical_roster();
        let comp = roster.convene(&Hash::of_bytes(b"y")).unwrap();
        // Drop one seat's ballot.
        let ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .skip(1)
            .map(|p| Ballot::new(p.clone(), Vote::Approve, None))
            .collect();
        let err = tally(&comp, &ballots).unwrap_err();
        assert!(matches!(err, PanelError::IncompleteQuorum { .. }));
    }

    #[test]
    fn tally_rejects_double_voting() {
        let roster = canonical_roster();
        let comp = roster.convene(&Hash::of_bytes(b"z")).unwrap();
        let mut ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .map(|p| Ballot::new(p.clone(), Vote::Approve, None))
            .collect();
        // First seat votes a second time.
        ballots.push(Ballot::new(comp.seated[0].clone(), Vote::Refuse, None));
        let err = tally(&comp, &ballots).unwrap_err();
        assert!(matches!(err, PanelError::DuplicateBallot(_)));
    }

    #[test]
    fn vote_parses_common_spellings() {
        assert_eq!("approve".parse::<Vote>().unwrap(), Vote::Approve);
        assert_eq!("YES".parse::<Vote>().unwrap(), Vote::Approve);
        assert_eq!("refuse".parse::<Vote>().unwrap(), Vote::Refuse);
        assert_eq!("no".parse::<Vote>().unwrap(), Vote::Refuse);
        assert!("maybe".parse::<Vote>().is_err());
    }

    #[test]
    fn role_log_roundtrips_through_json() {
        let roster = canonical_roster();
        let comp = roster.convene(&Hash::of_bytes(b"json")).unwrap();
        let ballots: Vec<Ballot> = comp
            .seated
            .iter()
            .map(|p| Ballot::new(p.clone(), Vote::Approve, Some("ok".into())))
            .collect();
        let log = tally(&comp, &ballots).unwrap();
        let json = serde_json::to_string(&log).unwrap();
        let back: RoleLog = serde_json::from_str(&json).unwrap();
        assert_eq!(log, back);
    }
}
