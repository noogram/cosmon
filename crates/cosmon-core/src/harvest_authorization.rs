// SPDX-License-Identifier: AGPL-3.0-only

//! Authority to perform one bounded harvest transaction (ADR-172).
//!
//! # The hole this closes
//!
//! `cs done` is one transaction with two effects: integrate a completed
//! branch, then tear its worktree down. [ADR-165] made the pilot and its
//! workers share one non-root uid, which removed the POSIX ownership
//! difference that had incidentally stood between a worker and its own
//! harvest — while showing that the difference had never been a *designed*
//! authority boundary in the first place.
//!
//! What was left behind was a set of signals that look like authority and are
//! not: uid, cwd, an absent `COSMON_MOL_DIR`, a preceding
//! `RuntimeMergeDispatched` event, a phantom `DoneToken<A>`, a `--by operator`
//! string. Every one of them is reproducible by the caller under a shared uid,
//! and [ADR-171] had already falsified the last of them. They remain useful
//! perimeter and audit signals. None of them may be described as *the*
//! authorisation.
//!
//! # What authority is here
//!
//! A closed sum, [`DoneAuthorization`], over one canonical [`HarvestGrant`]:
//! a delegation the operator signed once for an autonomous scope, or one
//! explicitly ratified human-reserved harvest. Both variants seal every field
//! that changes the meaning of the authority — galaxy, scope, base branch,
//! action, the reservations crossed, the epoch and the expiry — so changing
//! any of them after the fact stops the signature verifying.
//!
//! # Custody: cosmon checks the seal and owns no stamp
//!
//! Exactly as [`crate::operator_attestation`] does for the takeover key, and
//! for exactly the same reason: the adversary is a process running as the
//! operator, on the operator's host, holding the operator's files. No `if` in
//! this codebase is a barrier to it. What it cannot do is produce a signature
//! under a key it does not hold. So there is **no signing path for the
//! harvest key anywhere in the shipped tree**, and a test asserts that rather
//! than trusting it
//! (`done_authorization_unforgeable::the_shipped_tree_owns_no_signing_path_for_the_harvest_seal`).
//!
//! # Revocation is arithmetic, never communicational
//!
//! There is nobody to notify — not a grant in flight, not a detached
//! container, not a cached permit. [`GrantEpoch`] is monotone per galaxy and
//! [`authorize`] demands an exact match against the epoch re-derived at the
//! effect boundary, so bumping the epoch invalidates every outstanding grant
//! by arithmetic. A policy digest change or a base-branch change invalidates a
//! delegation the same way. Any design of revocation as "a message sent" is
//! rejected.
//!
//! # The grant does not live in the molecule's state
//!
//! Merge authority is not an attribute of a molecule. It is the [`HarvestScope`]
//! of a signed grant, which is data held outside molecule state — because the
//! one property that authorises cannot live in a file its own subject can
//! rewrite.
//!
//! # What this claims, and what it does not
//!
//! It makes an **authorised cosmon harvest** the only harvest `cs done` will
//! perform. It does **not** make a same-uid worker unable to mutate the trunk:
//! that worker can still drive git plumbing against the shared repository and
//! bypass `cs` altogether. Until repository custody is actually separated,
//! out-of-band mutations stay *detectable* through the provenance ledger and
//! the git/CI gates — never *impossible*. ADR-172 §D5 is explicit about this
//! bound and this module keeps its vocabulary.
//!
//! [ADR-165]: https://noogram.org/adr/165
//! [ADR-171]: https://noogram.org/adr/171

use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::id::MoleculeId;
use crate::operator_attestation::{AttestationError, OperatorAttestation, OperatorKeyId};

/// Version tag of the canonical harvest-grant encoding.
///
/// First line of the signed bytes, so this signature domain shares no preimage
/// with the takeover grant of [`crate::operator_attestation`], with a notary
/// seal, or with a git commit signature. A v1 grant can therefore never be
/// replayed as any of them, nor as a future v2 harvest grant.
pub const HARVEST_GRANT_V1_TAG: &str = "cosmon-harvest-grant-v1";

// ---------------------------------------------------------------------------
// Epoch
// ---------------------------------------------------------------------------

/// The monotone counter that *is* the revocation mechanism for a galaxy.
///
/// A grant names the epoch it was signed against, and [`authorize`] refuses it
/// once the galaxy has moved on. Nothing is sent to anybody: the operator
/// bumps the number and every grant signed under the old one stops verifying
/// as current, wherever it happens to be sitting.
///
/// # Examples
///
/// ```
/// use cosmon_core::harvest_authorization::GrantEpoch;
///
/// let e = GrantEpoch::first();
/// assert_eq!(e.to_string(), "1");
/// assert!(e.next() > e);
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct GrantEpoch(u64);

impl GrantEpoch {
    /// The epoch a galaxy that has never revoked anything is at.
    #[must_use]
    pub fn first() -> Self {
        Self(1)
    }

    /// Wrap a raw counter, for reading one back out of storage.
    #[must_use]
    pub fn from_u64(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw counter, for writing one to storage.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// The next epoch — the operator's revocation gesture.
    ///
    /// Saturating rather than wrapping: an epoch that wrapped to zero would
    /// silently re-validate the oldest grants in the ledger.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for GrantEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Action
// ---------------------------------------------------------------------------

/// The effect a grant authorises.
///
/// A one-variant enum today and deliberately not a constant: the `action=`
/// line exists so that a second effect (a publish, a takeover of a base
/// branch) can never be authorised by a signature an operator gave for a
/// harvest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarvestAction {
    /// Integrate a completed molecule's branch into the resolved base.
    Done,
}

impl fmt::Display for HarvestAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Done => f.write_str("done"),
        }
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// What a grant covers: one molecule, or a mission under a pinned policy.
///
/// The two shapes are the two halves of ADR-172 §D2. A [`Self::Molecule`]
/// scope is one ratified harvest and cannot approve its sibling. A
/// [`Self::Mission`] scope is a delegation whose reach is bounded by the
/// policy digest it names — change the policy and the digest changes, so the
/// delegation lapses without anyone being told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HarvestScope {
    /// Exactly this molecule, and no other.
    Molecule {
        /// The molecule whose harvest is authorised.
        molecule: MoleculeId,
    },
    /// Any member of this mission, while the policy digest still matches.
    Mission {
        /// Root of the DAG the delegation covers.
        mission: MoleculeId,
        /// Digest of the autonomous policy the operator approved. Sealed, so a
        /// policy edit invalidates the delegation arithmetically.
        policy_digest: String,
    },
}

impl fmt::Display for HarvestScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Molecule { molecule } => write!(f, "molecule:{}", molecule.as_str()),
            Self::Mission {
                mission,
                policy_digest,
            } => write!(f, "mission:{}+policy:{policy_digest}", mission.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// Construction errors
// ---------------------------------------------------------------------------

/// Why a grant could not be composed.
///
/// Enumerated rather than collapsed into one "invalid" for the reason
/// [`AttestationError`] is: the operator reading the message has to know
/// whether to fix a field, re-sign, or stop.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    /// A textual field was empty or whitespace only.
    #[error("{field} is empty — a grant must name it")]
    FieldEmpty {
        /// Which line of the challenge was blank.
        field: &'static str,
    },
    /// A textual field held a character that would break the line-oriented
    /// encoding. A newline anywhere in a grant would let a caller append lines
    /// of its own to the signed text.
    #[error("{field} {found:?} holds a control character — it would forge a line of the grant")]
    FieldNotOneLine {
        /// Which line of the challenge was malformed.
        field: &'static str,
        /// The rejected text, quoted so the offending byte is visible.
        found: String,
    },
    /// A reservation name held the separator the `reservations=` line uses.
    #[error("reservation {found:?} holds a comma — it would forge a second reservation")]
    ReservationHoldsSeparator {
        /// The rejected reservation.
        found: String,
    },
    /// A ratified seal was offered a mission scope. Per-molecule ratification
    /// is the whole point of the variant: approval for one reviewed molecule
    /// must not become approval for its siblings.
    #[error(
        "an operator harvest seal is molecule-scoped — {found} is a delegation, not a ratification"
    )]
    RatificationIsNotMissionScoped {
        /// The scope that was offered.
        found: HarvestScope,
    },
}

// ---------------------------------------------------------------------------
// The grant
// ---------------------------------------------------------------------------

/// The exact harvest an operator is asked to sign.
///
/// Every field that changes the meaning of the authority is in here and
/// nothing else is, so a signature over these bytes authorises one bounded
/// transaction rather than a class of them.
///
/// # Examples
///
/// ```
/// use cosmon_core::harvest_authorization::{GrantEpoch, HarvestAction, HarvestGrant, HarvestScope};
/// use cosmon_core::id::MoleculeId;
///
/// let grant = HarvestGrant::new(
///     "cosmon",
///     HarvestScope::Molecule { molecule: MoleculeId::new("task-20260901-6da6").unwrap() },
///     "main",
///     HarvestAction::Done,
///     ["needs-review"],
///     GrantEpoch::first(),
///     None,
/// )
/// .unwrap();
///
/// assert_eq!(
///     grant.to_string(),
///     "cosmon-harvest-grant-v1\n\
///      galaxy=cosmon\n\
///      scope=molecule:task-20260901-6da6\n\
///      base=main\n\
///      action=done\n\
///      reservations=needs-review\n\
///      epoch=1\n\
///      expires=none\n"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarvestGrant {
    /// Galaxy the harvest lands in. A grant does not travel between galaxies.
    pub galaxy: String,
    /// What the authority reaches.
    pub scope: HarvestScope,
    /// The *resolved* integration branch, not a symbolic default. A base
    /// change is a different transaction and refuses the grant.
    pub base: String,
    /// The effect authorised.
    pub action: HarvestAction,
    /// The exact reservations this authority may cross, canonically sorted and
    /// deduplicated. Empty means "an ordinary, unreserved harvest".
    pub reservations: Vec<String>,
    /// Galaxy epoch the grant was signed against.
    pub epoch: GrantEpoch,
    /// When the authority lapses on its own, or `None` for one that holds
    /// until the epoch moves.
    pub expires: Option<DateTime<Utc>>,
}

impl HarvestGrant {
    /// Compose a grant, refusing any field the line-oriented encoding could
    /// not hold unambiguously.
    ///
    /// Reservations are sorted and deduplicated here so that two callers
    /// naming the same set produce the same preimage, and an operator who
    /// signed a set cannot be surprised by a re-ordering of it.
    ///
    /// # Errors
    ///
    /// [`GrantError::FieldEmpty`] for a blank galaxy, base or reservation,
    /// [`GrantError::FieldNotOneLine`] for one holding a control character,
    /// and [`GrantError::ReservationHoldsSeparator`] for a reservation
    /// carrying the `,` the encoding uses to separate them.
    pub fn new(
        galaxy: impl Into<String>,
        scope: HarvestScope,
        base: impl Into<String>,
        action: HarvestAction,
        reservations: impl IntoIterator<Item = impl Into<String>>,
        epoch: GrantEpoch,
        expires: Option<DateTime<Utc>>,
    ) -> Result<Self, GrantError> {
        let galaxy = one_line("galaxy", galaxy.into())?;
        let base = one_line("base", base.into())?;
        if let HarvestScope::Mission { policy_digest, .. } = &scope {
            one_line("policy_digest", policy_digest.clone())?;
        }
        let mut canonical = BTreeSet::new();
        for raw in reservations {
            let value = one_line("reservation", raw.into())?;
            if value.contains(',') {
                return Err(GrantError::ReservationHoldsSeparator { found: value });
            }
            canonical.insert(value);
        }
        Ok(Self {
            galaxy,
            scope,
            base,
            action,
            reservations: canonical.into_iter().collect(),
            epoch,
            expires,
        })
    }

    /// The bytes an operator signs, and the bytes a verifier checks.
    ///
    /// Identical to [`Display`](fmt::Display) so a challenge printed to a file
    /// for stock `minisign` and the grant rebuilt from a ledger line are the
    /// same preimage.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }

    /// A stable digest of the signed bytes, used to key consumption.
    #[must_use]
    pub fn fingerprint(&self) -> GrantFingerprint {
        GrantFingerprint(hex_digest(&self.canonical_bytes()))
    }

    /// Whether this grant's scope reaches `molecule`, given the mission and
    /// policy digest re-derived at the effect boundary.
    #[must_use]
    pub fn covers(
        &self,
        molecule: &MoleculeId,
        mission: Option<&MoleculeId>,
        policy_digest: Option<&str>,
    ) -> bool {
        match &self.scope {
            HarvestScope::Molecule { molecule: sealed } => sealed == molecule,
            HarvestScope::Mission {
                mission: sealed,
                policy_digest: sealed_digest,
            } => mission == Some(sealed) && policy_digest == Some(sealed_digest.as_str()),
        }
    }
}

impl fmt::Display for HarvestGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{HARVEST_GRANT_V1_TAG}")?;
        writeln!(f, "galaxy={}", self.galaxy)?;
        writeln!(f, "scope={}", self.scope)?;
        writeln!(f, "base={}", self.base)?;
        writeln!(f, "action={}", self.action)?;
        if self.reservations.is_empty() {
            writeln!(f, "reservations=none")?;
        } else {
            writeln!(f, "reservations={}", self.reservations.join(","))?;
        }
        writeln!(f, "epoch={}", self.epoch)?;
        match self.expires {
            Some(at) => writeln!(f, "expires={}", at.to_rfc3339()),
            None => writeln!(f, "expires=none"),
        }
    }
}

/// Reject a field the line-oriented encoding could not hold unambiguously.
fn one_line(field: &'static str, value: String) -> Result<String, GrantError> {
    if value.trim().is_empty() {
        return Err(GrantError::FieldEmpty { field });
    }
    if value.chars().any(char::is_control) {
        return Err(GrantError::FieldNotOneLine {
            field,
            found: value,
        });
    }
    Ok(value)
}

/// Lowercase hex of the SHA-256 of `bytes`.
fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// The digest an autonomous policy is identified by inside a delegation.
///
/// Exposed so an adapter that reads the policy file and the domain that seals
/// its digest agree on one function rather than on a shared convention.
///
/// # Examples
///
/// ```
/// use cosmon_core::harvest_authorization::policy_digest;
///
/// // A galaxy that has approved no policy still has one, and it is empty.
/// assert_eq!(policy_digest(b"").len(), 64);
/// assert_ne!(policy_digest(b"a"), policy_digest(b"b"));
/// ```
#[must_use]
pub fn policy_digest(bytes: &[u8]) -> String {
    hex_digest(bytes)
}

/// Digest of a grant's signed bytes.
///
/// Recorded in the consumption ledger so a replay is recognised without
/// keeping the whole grant beside every receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GrantFingerprint(String);

impl GrantFingerprint {
    /// The digest as recorded.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GrantFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// The closed sum
// ---------------------------------------------------------------------------

/// A delegation to policy for an auto-harvestable scope.
///
/// The operator approves an autonomous policy once; permits for individual
/// members of the scope are derived from it, never signed one by one. A policy
/// or base-branch change invalidates the whole delegation, because both are
/// inside the signed bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedHarvestCapability {
    /// The signed grant.
    pub grant: HarvestGrant,
    /// The operator's detached signature over [`HarvestGrant::canonical_bytes`].
    pub attestation: OperatorAttestation,
}

impl DelegatedHarvestCapability {
    /// Wrap a signed grant as a delegation.
    #[must_use]
    pub fn new(grant: HarvestGrant, attestation: OperatorAttestation) -> Self {
        Self { grant, attestation }
    }
}

/// One explicitly ratified human-reserved harvest.
///
/// Molecule-scoped by construction: approval for one reviewed molecule cannot
/// become approval for its sibling, and the reservations it names are the
/// exact ones it crosses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorHarvestSeal {
    /// The signed grant.
    pub grant: HarvestGrant,
    /// The operator's detached signature over [`HarvestGrant::canonical_bytes`].
    pub attestation: OperatorAttestation,
}

impl OperatorHarvestSeal {
    /// Wrap a signed grant as a ratification.
    ///
    /// # Errors
    ///
    /// [`GrantError::RatificationIsNotMissionScoped`] when the grant carries a
    /// mission scope: that is a delegation, and calling it a ratification
    /// would let one signature ratify a whole DAG.
    pub fn new(grant: HarvestGrant, attestation: OperatorAttestation) -> Result<Self, GrantError> {
        if matches!(grant.scope, HarvestScope::Mission { .. }) {
            return Err(GrantError::RatificationIsNotMissionScoped { found: grant.scope });
        }
        Ok(Self { grant, attestation })
    }
}

/// Authority to perform one bounded harvest transaction.
///
/// A closed sum, not a boolean and not a caller label: the two variants cover
/// the same canonical [`HarvestGrant`] and differ only in scope and in how a
/// permit is derived from them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "authority")]
pub enum DoneAuthorization {
    /// Delegation to policy for an auto-harvestable scope.
    Delegated(DelegatedHarvestCapability),
    /// One explicitly ratified human-reserved harvest.
    Ratified(OperatorHarvestSeal),
}

impl DoneAuthorization {
    /// The grant both variants cover.
    #[must_use]
    pub fn grant(&self) -> &HarvestGrant {
        match self {
            Self::Delegated(d) => &d.grant,
            Self::Ratified(r) => &r.grant,
        }
    }

    /// The operator signature over that grant.
    #[must_use]
    pub fn attestation(&self) -> &OperatorAttestation {
        match self {
            Self::Delegated(d) => &d.attestation,
            Self::Ratified(r) => &r.attestation,
        }
    }

    /// The unit of consumption for this authority against `molecule`.
    ///
    /// This is where the two variants genuinely differ. A ratification is
    /// consumed *whole* — its permit is the grant itself — so a second,
    /// different effect under the same seal has nothing left to spend. A
    /// delegation yields one molecule-specific permit per member of its scope,
    /// which is what lets an approved policy drain a DAG without the operator
    /// signing every edge.
    #[must_use]
    pub fn permit_id(&self, molecule: &MoleculeId) -> PermitId {
        match self {
            Self::Ratified(r) => PermitId(r.grant.fingerprint().0),
            Self::Delegated(d) => PermitId(hex_digest(
                format!("{}\n{}\n", d.grant.fingerprint(), molecule.as_str()).as_bytes(),
            )),
        }
    }
}

/// Identity of one spendable permit derived from a [`DoneAuthorization`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PermitId(String);

impl PermitId {
    /// The digest as recorded.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PermitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The reservations `tags` crosses, in the vocabulary ADR-172 §D1 fixes.
///
/// This is the list of thresholds the operator reserved: `hold:human`,
/// `needs-review` and its cross-provider variant, `security` and every
/// `security:*`, `no-auto-harvest`, and every `harvest_to:*` routing intent.
/// A harvest that crosses any of them needs a grant that names it.
///
/// Kept here, in the domain, rather than beside the `cs done` call site, so
/// that the set a grant is checked against and the set an operator reads in
/// the ADR are the same list in one place.
///
/// # Examples
///
/// ```
/// use cosmon_core::harvest_authorization::reservations_crossed;
///
/// let crossed = reservations_crossed(["needs-review", "kind:task", "security:leak"]);
/// assert_eq!(crossed, vec!["needs-review".to_owned(), "security:leak".to_owned()]);
/// ```
#[must_use]
pub fn reservations_crossed(tags: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut out = BTreeSet::new();
    for tag in tags {
        let tag = tag.as_ref();
        let reserved = matches!(
            tag,
            "hold:human" | "needs-review" | "needs-review-cross-provider" | "security"
        ) || tag.starts_with("security:")
            || tag == "no-auto-harvest"
            || tag.starts_with("harvest_to:");
        if reserved {
            out.insert(tag.to_owned());
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Facts and effect
// ---------------------------------------------------------------------------

/// The harvest facts as re-derived under the trunk lock, immediately before
/// the first git mutation.
///
/// Nothing in here is read from the grant. That is the whole point of ADR-172
/// §D3: checking a grant against the facts that were true when it was written
/// would be advice, because a same-uid process can rewrite those files. These
/// are the facts that are true *now*, inside the mutex that makes "now" mean
/// something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestFacts {
    /// Galaxy the harvest is about to land in.
    pub galaxy: String,
    /// Molecule whose branch is about to be integrated.
    pub molecule: MoleculeId,
    /// DAG root the molecule belongs to, when it has one.
    pub mission: Option<MoleculeId>,
    /// Digest of the autonomous policy currently in force, when there is one.
    pub policy_digest: Option<String>,
    /// The base branch as *resolved* here, not as configured or defaulted.
    pub base: String,
    /// The effect about to happen.
    pub action: HarvestAction,
    /// The reservations this harvest actually crosses, from the molecule's
    /// tags at this instant. Adding one after a grant was signed refuses the
    /// grant; so does removing one it named.
    pub reservations_crossed: Vec<String>,
    /// The galaxy's current grant epoch.
    pub epoch: GrantEpoch,
    /// Now, for the expiry comparison. Injected rather than read, so the
    /// decision stays I/O-free and a clock is a port like any other.
    pub now: DateTime<Utc>,
}

/// The effect a permit is spent on.
///
/// Two harvests are the same effect when they land the same molecule on the
/// same base in the same galaxy. Replaying a consumed permit against the same
/// effect returns what was already recorded; against a different one it is
/// refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarvestEffect {
    /// Galaxy the harvest landed in.
    pub galaxy: String,
    /// Molecule that was integrated.
    pub molecule: MoleculeId,
    /// Base branch it was integrated into.
    pub base: String,
}

impl HarvestEffect {
    /// The effect the facts describe.
    #[must_use]
    pub fn of(facts: &HarvestFacts) -> Self {
        Self {
            galaxy: facts.galaxy.clone(),
            molecule: facts.molecule.clone(),
            base: facts.base.clone(),
        }
    }
}

impl fmt::Display for HarvestEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}→{}",
            self.galaxy,
            self.molecule.as_str(),
            self.base
        )
    }
}

/// An append-only receipt that one permit was spent on one effect.
///
/// The ledger this lives in is append-only and the lookup is by
/// [`PermitId`], which is what makes consumption idempotent rather than
/// merely once-only: a retried `cs done` after a crash finds its own receipt
/// and reports the harvest that already landed instead of refusing a caller
/// who did nothing wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumptionRecord {
    /// The permit that was spent.
    pub permit: PermitId,
    /// The grant it was derived from, for audit without the permit algebra.
    pub grant: GrantFingerprint,
    /// The effect it was spent on.
    pub effect: HarvestEffect,
    /// Which operator key authorised it.
    pub key_id: OperatorKeyId,
    /// The invocation the `cs done` transaction and this receipt share, so the
    /// ledger can join the two.
    pub invocation_id: String,
}

/// A permit that [`authorize`] has just cleared for spending.
///
/// Carrying the effect and the key id rather than a bare `true` is what lets
/// the caller write the receipt in the same breath as the mutation, with
/// nothing re-derived in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestPermit {
    /// The permit to record as consumed.
    pub permit: PermitId,
    /// The grant it came from.
    pub grant: GrantFingerprint,
    /// The effect it authorises, and only that effect.
    pub effect: HarvestEffect,
    /// The operator key that signed the grant.
    pub key_id: OperatorKeyId,
}

/// The outcome of an authorisation at the effect boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizedHarvest {
    /// The permit is unspent and covers this effect. Consume it, then mutate.
    Fresh(Box<HarvestPermit>),
    /// This exact harvest already landed under this permit. The caller reports
    /// the recorded outcome and mutates nothing.
    AlreadyLanded(Box<ConsumptionRecord>),
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Why an authorisation did not permit a harvest.
///
/// Each variant names what an operator should do next — re-sign, bump nothing,
/// pin a key, or stop — for the reason [`AttestationError`] is enumerated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HarvestRefusal {
    /// The seal did not check out. Includes the case where no trust root is
    /// pinned, which refuses rather than permits: deleting the key stops
    /// harvests instead of unlocking them.
    #[error("not an authorised harvest gesture: {0}")]
    NotSealed(#[from] AttestationError),
    /// The grant was signed for another galaxy.
    #[error("the grant authorises galaxy {sealed}, this harvest lands in {actual}")]
    WrongGalaxy {
        /// Galaxy inside the signed bytes.
        sealed: String,
        /// Galaxy re-derived at the effect boundary.
        actual: String,
    },
    /// The molecule is outside the grant's scope — a different molecule for a
    /// ratification, or a different mission or policy digest for a delegation.
    #[error("{molecule} is outside the grant's scope {scope}")]
    OutOfScope {
        /// Molecule about to be harvested.
        molecule: MoleculeId,
        /// Scope inside the signed bytes. Boxed because a refusal travels in
        /// the `Err` arm of every authorisation and a fat error makes every
        /// success pay for it.
        scope: Box<HarvestScope>,
    },
    /// The resolved base branch is not the one signed for.
    #[error("the grant authorises base {sealed}, this harvest resolves to {actual}")]
    WrongBase {
        /// Base inside the signed bytes.
        sealed: String,
        /// Base resolved at the effect boundary.
        actual: String,
    },
    /// The grant authorises a different effect entirely.
    #[error("the grant authorises action {sealed}, this transaction is {actual}")]
    WrongAction {
        /// Action inside the signed bytes.
        sealed: HarvestAction,
        /// Action about to be performed.
        actual: HarvestAction,
    },
    /// The harvest crosses a reservation the grant never named. This is the
    /// falsifier ADR-172 lists second, kept as a refusal rather than a warning.
    #[error("the harvest crosses reservation {reservation:?}, which the grant does not name")]
    ReservationNotNamed {
        /// The reservation crossed but unnamed.
        reservation: String,
    },
    /// The galaxy epoch moved: the grant has been revoked, arithmetically.
    #[error("the grant was signed at epoch {sealed}, the galaxy is at {actual} — it is revoked")]
    EpochSuperseded {
        /// Epoch inside the signed bytes.
        sealed: GrantEpoch,
        /// Epoch re-derived at the effect boundary.
        actual: GrantEpoch,
    },
    /// The grant lapsed on its own.
    #[error("the grant expired at {expired_at}")]
    Expired {
        /// The expiry inside the signed bytes.
        expired_at: DateTime<Utc>,
    },
    /// The permit was already spent, on something else. A consumed grant
    /// never authorises a second, different effect.
    #[error("this authority was already spent on {spent_on} and cannot authorise {requested}")]
    AlreadySpentElsewhere {
        /// Effect the permit was consumed on.
        spent_on: Box<HarvestEffect>,
        /// Effect it is now being offered for.
        requested: Box<HarvestEffect>,
    },
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// The port that answers *did the operator seal this grant?*
///
/// A trait rather than a function because the trust root is I/O — a file, a
/// token, a remote attestor — and this crate holds none. The domain states the
/// question; an adapter answers it. There is deliberately no companion `sign`
/// method: see this module's header.
pub trait HarvestSealVerifier {
    /// Return `Ok(())` iff `attestation` is a valid operator signature over
    /// `grant`'s canonical bytes by the trusted key.
    ///
    /// # Errors
    ///
    /// One [`AttestationError`] naming what an operator should do next.
    /// [`AttestationError::NoTrustRoot`] when nothing is pinned, which is a
    /// refusal and never a pass.
    fn verify(
        &self,
        grant: &HarvestGrant,
        attestation: &OperatorAttestation,
    ) -> Result<(), AttestationError>;

    /// The key this verifier trusts, recorded on every receipt so a
    /// substituted trust root shows up as a key change in the ledger.
    fn trusted_key_id(&self) -> OperatorKeyId;
}

/// The port holding the append-only record of what has been spent.
///
/// Split from the verifier because the two fail for different reasons and an
/// adapter for one is not an adapter for the other.
pub trait HarvestConsumptionLedger {
    /// The receipt for `permit`, if it has been spent.
    ///
    /// # Errors
    ///
    /// Adapter-defined; a lookup that cannot be performed must not be reported
    /// as "unspent", or a crash would turn into a second harvest.
    fn recorded(&self, permit: &PermitId) -> Result<Option<ConsumptionRecord>, String>;

    /// Append `record`. Called immediately before the first git mutation.
    ///
    /// # Errors
    ///
    /// Adapter-defined. A failure here must abort the harvest: an unspent
    /// permit plus a landed merge is exactly the double-spend the ledger
    /// exists to prevent.
    fn consume(&self, record: &ConsumptionRecord) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------

/// Decide whether `authorization` permits the harvest `facts` describe.
///
/// This is the I/O-free half of ADR-172 §D3. It is called at the effect
/// boundary — under the trunk lock, immediately before the first git mutation
/// — with facts re-derived there and with `prior` read from the consumption
/// ledger. Everything it needs that lives outside the domain arrives through
/// [`HarvestSealVerifier`] and the caller's ledger lookup.
///
/// The seal is checked first, so a refusal never leaks a comparison against
/// fields an unsealed caller chose.
///
/// # Errors
///
/// One [`HarvestRefusal`]. There is no permissive branch: an authorisation
/// that cannot be checked is refused.
///
/// # Examples
///
/// ```
/// # use chrono::Utc;
/// # use cosmon_core::harvest_authorization::*;
/// # use cosmon_core::id::MoleculeId;
/// # use cosmon_core::operator_attestation::{AttestationError, OperatorAttestation, OperatorKeyId};
/// // An adapter that trusts nothing refuses, rather than waving the harvest through.
/// struct NoTrustRoot;
/// impl HarvestSealVerifier for NoTrustRoot {
///     fn verify(&self, _: &HarvestGrant, _: &OperatorAttestation) -> Result<(), AttestationError> {
///         Err(AttestationError::NoTrustRoot)
///     }
///     fn trusted_key_id(&self) -> OperatorKeyId { OperatorKeyId::from_bytes([0; 8]) }
/// }
///
/// let molecule = MoleculeId::new("task-20260901-6da6").unwrap();
/// let grant = HarvestGrant::new(
///     "cosmon",
///     HarvestScope::Molecule { molecule: molecule.clone() },
///     "main",
///     HarvestAction::Done,
///     Vec::<String>::new(),
///     GrantEpoch::first(),
///     None,
/// ).unwrap();
/// let seal = OperatorHarvestSeal::new(grant, OperatorAttestation {
///     key_id: OperatorKeyId::from_bytes([0; 8]),
///     signature: String::new(),
///     global_signature: String::new(),
///     trusted_comment: String::new(),
///     untrusted_comment: String::new(),
/// }).unwrap();
///
/// let facts = HarvestFacts {
///     galaxy: "cosmon".into(),
///     molecule,
///     mission: None,
///     policy_digest: None,
///     base: "main".into(),
///     action: HarvestAction::Done,
///     reservations_crossed: Vec::new(),
///     epoch: GrantEpoch::first(),
///     now: Utc::now(),
/// };
///
/// let refusal = authorize(&DoneAuthorization::Ratified(seal), &facts, None, &NoTrustRoot)
///     .expect_err("no trust root must refuse");
/// assert!(matches!(refusal, HarvestRefusal::NotSealed(AttestationError::NoTrustRoot)));
/// ```
pub fn authorize(
    authorization: &DoneAuthorization,
    facts: &HarvestFacts,
    prior: Option<&ConsumptionRecord>,
    verifier: &dyn HarvestSealVerifier,
) -> Result<AuthorizedHarvest, HarvestRefusal> {
    let grant = authorization.grant();
    verifier.verify(grant, authorization.attestation())?;

    if grant.galaxy != facts.galaxy {
        return Err(HarvestRefusal::WrongGalaxy {
            sealed: grant.galaxy.clone(),
            actual: facts.galaxy.clone(),
        });
    }
    if !grant.covers(
        &facts.molecule,
        facts.mission.as_ref(),
        facts.policy_digest.as_deref(),
    ) {
        return Err(HarvestRefusal::OutOfScope {
            molecule: facts.molecule.clone(),
            scope: Box::new(grant.scope.clone()),
        });
    }
    if grant.base != facts.base {
        return Err(HarvestRefusal::WrongBase {
            sealed: grant.base.clone(),
            actual: facts.base.clone(),
        });
    }
    if grant.action != facts.action {
        return Err(HarvestRefusal::WrongAction {
            sealed: grant.action,
            actual: facts.action,
        });
    }
    for crossed in &facts.reservations_crossed {
        if !grant.reservations.iter().any(|named| named == crossed) {
            return Err(HarvestRefusal::ReservationNotNamed {
                reservation: crossed.clone(),
            });
        }
    }
    if grant.epoch != facts.epoch {
        return Err(HarvestRefusal::EpochSuperseded {
            sealed: grant.epoch,
            actual: facts.epoch,
        });
    }
    if let Some(expires) = grant.expires {
        if facts.now > expires {
            return Err(HarvestRefusal::Expired {
                expired_at: expires,
            });
        }
    }

    let effect = HarvestEffect::of(facts);
    if let Some(record) = prior {
        if record.effect == effect {
            return Ok(AuthorizedHarvest::AlreadyLanded(Box::new(record.clone())));
        }
        return Err(HarvestRefusal::AlreadySpentElsewhere {
            spent_on: Box::new(record.effect.clone()),
            requested: Box::new(effect),
        });
    }

    Ok(AuthorizedHarvest::Fresh(Box::new(HarvestPermit {
        permit: authorization.permit_id(&facts.molecule),
        grant: grant.fingerprint(),
        effect,
        key_id: verifier.trusted_key_id(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in operator key that accepts everything, so the tests below
    /// exercise the *fact-matching* half rather than the crypto. The crypto
    /// half is asserted end-to-end against a real minisign signature in
    /// `cosmon-cli/tests/done_authorization_unforgeable.rs`.
    struct AlwaysSealed;

    impl HarvestSealVerifier for AlwaysSealed {
        fn verify(
            &self,
            _grant: &HarvestGrant,
            _attestation: &OperatorAttestation,
        ) -> Result<(), AttestationError> {
            Ok(())
        }

        fn trusted_key_id(&self) -> OperatorKeyId {
            OperatorKeyId::from_bytes([9; 8])
        }
    }

    struct NeverSealed(AttestationError);

    impl HarvestSealVerifier for NeverSealed {
        fn verify(
            &self,
            _grant: &HarvestGrant,
            _attestation: &OperatorAttestation,
        ) -> Result<(), AttestationError> {
            Err(self.0.clone())
        }

        fn trusted_key_id(&self) -> OperatorKeyId {
            OperatorKeyId::from_bytes([9; 8])
        }
    }

    fn mol(raw: &str) -> MoleculeId {
        MoleculeId::new(raw).expect("fixture molecule id")
    }

    fn attestation() -> OperatorAttestation {
        OperatorAttestation {
            key_id: OperatorKeyId::from_bytes([9; 8]),
            signature: "AAAA".to_owned(),
            global_signature: "BBBB".to_owned(),
            trusted_comment: "fixture".to_owned(),
            untrusted_comment: "fixture".to_owned(),
        }
    }

    fn grant(scope: HarvestScope, reservations: &[&str]) -> HarvestGrant {
        HarvestGrant::new(
            "cosmon",
            scope,
            "main",
            HarvestAction::Done,
            reservations.iter().copied(),
            GrantEpoch::first(),
            None,
        )
        .expect("fixture grant")
    }

    fn ratified(molecule: &MoleculeId, reservations: &[&str]) -> DoneAuthorization {
        let g = grant(
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            reservations,
        );
        DoneAuthorization::Ratified(
            OperatorHarvestSeal::new(g, attestation()).expect("fixture seal"),
        )
    }

    fn delegated(mission: &MoleculeId, digest: &str) -> DoneAuthorization {
        let g = grant(
            HarvestScope::Mission {
                mission: mission.clone(),
                policy_digest: digest.to_owned(),
            },
            &[],
        );
        DoneAuthorization::Delegated(DelegatedHarvestCapability::new(g, attestation()))
    }

    fn facts(molecule: &MoleculeId) -> HarvestFacts {
        HarvestFacts {
            galaxy: "cosmon".to_owned(),
            molecule: molecule.clone(),
            mission: None,
            policy_digest: None,
            base: "main".to_owned(),
            action: HarvestAction::Done,
            reservations_crossed: Vec::new(),
            epoch: GrantEpoch::first(),
            now: DateTime::from_timestamp(1_756_700_000, 0).expect("fixture clock"),
        }
    }

    // -- encoding ----------------------------------------------------------

    #[test]
    fn canonical_bytes_and_display_are_the_same_preimage() {
        let g = grant(
            HarvestScope::Molecule {
                molecule: mol("task-20260901-6da6"),
            },
            &[],
        );
        assert_eq!(g.canonical_bytes(), g.to_string().into_bytes());
    }

    #[test]
    fn the_version_line_separates_this_domain_from_a_takeover_grant() {
        let g = grant(
            HarvestScope::Molecule {
                molecule: mol("task-20260901-6da6"),
            },
            &[],
        );
        let text = g.to_string();
        assert!(text.starts_with("cosmon-harvest-grant-v1\n"));
        assert!(
            !text.contains(crate::operator_attestation::CHALLENGE_V1_TAG),
            "a harvest grant must share no preimage with a takeover grant"
        );
    }

    #[test]
    fn every_sealed_field_changes_the_signed_bytes() {
        let molecule = mol("task-20260901-6da6");
        let base = grant(
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            &[],
        );

        let mut galaxy = base.clone();
        galaxy.galaxy = "other".to_owned();

        let mut scope = base.clone();
        scope.scope = HarvestScope::Molecule {
            molecule: mol("task-20260901-aaaa"),
        };

        let mut branch = base.clone();
        branch.base = "spore/math-attack".to_owned();

        let mut reservations = base.clone();
        reservations.reservations = vec!["needs-review".to_owned()];

        let mut epoch = base.clone();
        epoch.epoch = base.epoch.next();

        let mut expires = base.clone();
        expires.expires = DateTime::from_timestamp(1_756_700_000, 0);

        for (name, altered) in [
            ("galaxy", galaxy),
            ("scope", scope),
            ("base", branch),
            ("reservations", reservations),
            ("epoch", epoch),
            ("expires", expires),
        ] {
            assert_ne!(
                base.canonical_bytes(),
                altered.canonical_bytes(),
                "{name} must be inside the signed bytes"
            );
        }
    }

    #[test]
    fn reservations_are_canonicalised_so_two_orderings_are_one_preimage() {
        let molecule = mol("task-20260901-6da6");
        let scope = HarvestScope::Molecule {
            molecule: molecule.clone(),
        };
        let a = grant(scope.clone(), &["security", "needs-review", "security"]);
        let b = grant(scope, &["needs-review", "security"]);
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert!(a
            .to_string()
            .contains("reservations=needs-review,security\n"));
    }

    #[test]
    fn a_field_cannot_smuggle_a_line_into_the_grant() {
        let err = HarvestGrant::new(
            "cosmon\nbase=attacker",
            HarvestScope::Molecule {
                molecule: mol("task-20260901-6da6"),
            },
            "main",
            HarvestAction::Done,
            Vec::<String>::new(),
            GrantEpoch::first(),
            None,
        )
        .expect_err("a newline in galaxy must be refused");
        assert!(matches!(
            err,
            GrantError::FieldNotOneLine {
                field: "galaxy",
                ..
            }
        ));
    }

    #[test]
    fn a_reservation_cannot_smuggle_a_second_reservation() {
        let err = HarvestGrant::new(
            "cosmon",
            HarvestScope::Molecule {
                molecule: mol("task-20260901-6da6"),
            },
            "main",
            HarvestAction::Done,
            ["needs-review,security"],
            GrantEpoch::first(),
            None,
        )
        .expect_err("a comma inside a reservation must be refused");
        assert!(matches!(err, GrantError::ReservationHoldsSeparator { .. }));
    }

    #[test]
    fn a_ratification_cannot_be_mission_scoped() {
        let g = grant(
            HarvestScope::Mission {
                mission: mol("task-20260901-6da6"),
                policy_digest: "d34d".to_owned(),
            },
            &[],
        );
        assert!(matches!(
            OperatorHarvestSeal::new(g, attestation()),
            Err(GrantError::RatificationIsNotMissionScoped { .. })
        ));
    }

    // -- the reducer -------------------------------------------------------

    #[test]
    fn an_operator_sealed_ordinary_harvest_is_authorised() {
        let molecule = mol("task-20260901-6da6");
        let out = authorize(
            &ratified(&molecule, &[]),
            &facts(&molecule),
            None,
            &AlwaysSealed,
        )
        .expect("a sealed, matching harvest must be authorised");
        assert!(matches!(out, AuthorizedHarvest::Fresh(_)));
    }

    #[test]
    fn absence_of_a_trust_root_refuses_and_does_not_permit() {
        let molecule = mol("task-20260901-6da6");
        let err = authorize(
            &ratified(&molecule, &[]),
            &facts(&molecule),
            None,
            &NeverSealed(AttestationError::NoTrustRoot),
        )
        .expect_err("nothing pinned must refuse");
        assert!(matches!(
            err,
            HarvestRefusal::NotSealed(AttestationError::NoTrustRoot)
        ));
    }

    #[test]
    fn the_seal_is_checked_before_any_fact_is_compared() {
        // Facts that mismatch on *every* field. The refusal must still be the
        // seal, so a caller can never learn which field to forge next by
        // watching which comparison fires.
        let molecule = mol("task-20260901-6da6");
        let mut f = facts(&molecule);
        f.galaxy = "elsewhere".to_owned();
        f.base = "spore/x".to_owned();
        f.epoch = GrantEpoch::from_u64(99);
        let err = authorize(
            &ratified(&molecule, &[]),
            &f,
            None,
            &NeverSealed(AttestationError::DoesNotCoverTransfer),
        )
        .expect_err("an unsealed grant must be refused first");
        assert!(matches!(err, HarvestRefusal::NotSealed(_)));
    }

    #[test]
    fn a_delegation_never_harvests_outside_its_galaxy() {
        let molecule = mol("task-20260901-6da6");
        let mission = mol("delib-20260819-cda2");
        let mut f = facts(&molecule);
        f.galaxy = "pactum".to_owned();
        f.mission = Some(mission.clone());
        f.policy_digest = Some("d34d".to_owned());
        let err = authorize(&delegated(&mission, "d34d"), &f, None, &AlwaysSealed)
            .expect_err("a grant does not travel between galaxies");
        assert!(matches!(err, HarvestRefusal::WrongGalaxy { .. }));
    }

    #[test]
    fn a_delegation_lapses_when_the_policy_digest_changes() {
        let molecule = mol("task-20260901-6da6");
        let mission = mol("delib-20260819-cda2");
        let mut f = facts(&molecule);
        f.mission = Some(mission.clone());
        f.policy_digest = Some("a-new-policy".to_owned());
        let err = authorize(&delegated(&mission, "d34d"), &f, None, &AlwaysSealed)
            .expect_err("a policy edit must revoke the delegation");
        assert!(matches!(err, HarvestRefusal::OutOfScope { .. }));
    }

    #[test]
    fn a_delegation_reaches_every_member_of_its_mission() {
        let mission = mol("delib-20260819-cda2");
        let capability = delegated(&mission, "d34d");
        for raw in ["task-20260901-6da6", "task-20260901-aaaa"] {
            let molecule = mol(raw);
            let mut f = facts(&molecule);
            f.mission = Some(mission.clone());
            f.policy_digest = Some("d34d".to_owned());
            assert!(
                authorize(&capability, &f, None, &AlwaysSealed).is_ok(),
                "{raw} is inside the delegated mission"
            );
        }
    }

    #[test]
    fn a_ratification_does_not_approve_a_sibling() {
        let sealed_for = mol("task-20260901-6da6");
        let sibling = mol("task-20260901-aaaa");
        let err = authorize(
            &ratified(&sealed_for, &[]),
            &facts(&sibling),
            None,
            &AlwaysSealed,
        )
        .expect_err("one ratification is one molecule");
        assert!(matches!(err, HarvestRefusal::OutOfScope { .. }));
    }

    #[test]
    fn a_base_branch_change_refuses_the_grant() {
        let molecule = mol("task-20260901-6da6");
        let mut f = facts(&molecule);
        f.base = "spore/math-attack".to_owned();
        let err = authorize(&ratified(&molecule, &[]), &f, None, &AlwaysSealed)
            .expect_err("the resolved base is sealed");
        assert!(matches!(err, HarvestRefusal::WrongBase { .. }));
    }

    #[test]
    fn a_reservation_the_grant_did_not_name_is_never_crossed() {
        let molecule = mol("task-20260901-6da6");
        let mut f = facts(&molecule);
        f.reservations_crossed = vec!["needs-review".to_owned()];
        let err = authorize(&ratified(&molecule, &[]), &f, None, &AlwaysSealed)
            .expect_err("an unnamed reservation must refuse");
        assert!(matches!(
            err,
            HarvestRefusal::ReservationNotNamed { ref reservation } if reservation == "needs-review"
        ));
    }

    #[test]
    fn a_ratification_crosses_exactly_the_reservation_it_names() {
        let molecule = mol("task-20260901-6da6");
        let mut f = facts(&molecule);
        f.reservations_crossed = vec!["needs-review".to_owned()];

        assert!(authorize(
            &ratified(&molecule, &["needs-review"]),
            &f,
            None,
            &AlwaysSealed
        )
        .is_ok());

        // Approval to cross `needs-review` must not silently become approval
        // to cross `security` as well.
        f.reservations_crossed = vec!["needs-review".to_owned(), "security".to_owned()];
        assert!(matches!(
            authorize(
                &ratified(&molecule, &["needs-review"]),
                &f,
                None,
                &AlwaysSealed
            ),
            Err(HarvestRefusal::ReservationNotNamed { .. })
        ));
    }

    #[test]
    fn bumping_the_epoch_revokes_every_outstanding_grant() {
        let molecule = mol("task-20260901-6da6");
        let authority = ratified(&molecule, &[]);
        let mut f = facts(&molecule);
        assert!(authorize(&authority, &f, None, &AlwaysSealed).is_ok());

        // The operator's whole revocation gesture: increment a counter. Nobody
        // is notified, and the grant in flight stops working.
        f.epoch = f.epoch.next();
        assert!(matches!(
            authorize(&authority, &f, None, &AlwaysSealed),
            Err(HarvestRefusal::EpochSuperseded { .. })
        ));
    }

    #[test]
    fn an_expired_grant_is_refused() {
        let molecule = mol("task-20260901-6da6");
        let expires = DateTime::from_timestamp(1_756_600_000, 0).expect("fixture expiry");
        let g = HarvestGrant::new(
            "cosmon",
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            "main",
            HarvestAction::Done,
            Vec::<String>::new(),
            GrantEpoch::first(),
            Some(expires),
        )
        .expect("fixture grant");
        let authority = DoneAuthorization::Ratified(
            OperatorHarvestSeal::new(g, attestation()).expect("fixture seal"),
        );
        assert!(matches!(
            authorize(&authority, &facts(&molecule), None, &AlwaysSealed),
            Err(HarvestRefusal::Expired { .. })
        ));
    }

    // -- consumption -------------------------------------------------------

    fn receipt(authority: &DoneAuthorization, f: &HarvestFacts) -> ConsumptionRecord {
        ConsumptionRecord {
            permit: authority.permit_id(&f.molecule),
            grant: authority.grant().fingerprint(),
            effect: HarvestEffect::of(f),
            key_id: OperatorKeyId::from_bytes([9; 8]),
            invocation_id: "inv-1".to_owned(),
        }
    }

    #[test]
    fn replaying_a_consumed_permit_on_the_same_effect_returns_what_landed() {
        let molecule = mol("task-20260901-6da6");
        let authority = ratified(&molecule, &[]);
        let f = facts(&molecule);
        let prior = receipt(&authority, &f);

        let out = authorize(&authority, &f, Some(&prior), &AlwaysSealed)
            .expect("a retry of the same harvest is not an error");
        match out {
            AuthorizedHarvest::AlreadyLanded(record) => {
                assert_eq!(record.invocation_id, "inv-1");
            }
            AuthorizedHarvest::Fresh(_) => panic!("a consumed permit must not re-authorise"),
        }
    }

    #[test]
    fn a_consumed_seal_never_authorises_a_second_different_effect() {
        // ADR-172 falsifier 5. The seal is spent whole, so re-pointing it at
        // another base — the shape an out-of-band retarget would take — finds
        // nothing left to spend.
        let molecule = mol("task-20260901-6da6");
        let authority = ratified(&molecule, &[]);
        let first = facts(&molecule);
        let prior = receipt(&authority, &first);

        let mut retarget = facts(&molecule);
        retarget.base = "spore/math-attack".to_owned();
        // The grant seals `base=main`, so the base check fires first; make the
        // grant itself name the new base to isolate the consumption rule.
        let g = grant(
            HarvestScope::Molecule {
                molecule: molecule.clone(),
            },
            &[],
        );
        let mut retargeted_grant = g;
        retargeted_grant.base = "spore/math-attack".to_owned();
        let retargeted = DoneAuthorization::Ratified(
            OperatorHarvestSeal::new(retargeted_grant, attestation()).expect("fixture seal"),
        );

        // Same permit id would require the same grant bytes; what the ledger
        // actually protects is the pair. Spend the original permit, then offer
        // it for the other effect.
        let err = authorize(&authority, &retarget, Some(&prior), &AlwaysSealed)
            .expect_err("a spent permit must not authorise another effect");
        assert!(
            matches!(err, HarvestRefusal::WrongBase { .. }),
            "the sealed base is checked before consumption"
        );

        // And with a grant that does cover the new base, the *permit* differs,
        // so the ledger lookup for it is empty and nothing is double-spent.
        assert_ne!(
            retargeted.permit_id(&molecule),
            authority.permit_id(&molecule)
        );
    }

    #[test]
    fn a_spent_permit_offered_for_a_different_molecule_is_refused() {
        let mission = mol("delib-20260819-cda2");
        let capability = delegated(&mission, "d34d");
        let first = mol("task-20260901-6da6");
        let second = mol("task-20260901-aaaa");

        let mut f1 = facts(&first);
        f1.mission = Some(mission.clone());
        f1.policy_digest = Some("d34d".to_owned());
        let prior = receipt(&capability, &f1);

        let mut f2 = facts(&second);
        f2.mission = Some(mission.clone());
        f2.policy_digest = Some("d34d".to_owned());

        // Handing the *first* molecule's receipt to the second molecule's
        // authorisation is the double-spend shape. The ledger is keyed by
        // permit, so this cannot happen through the real lookup — and if it
        // does, the effect comparison refuses it.
        let err = authorize(&capability, &f2, Some(&prior), &AlwaysSealed)
            .expect_err("a receipt for another effect must not authorise this one");
        assert!(matches!(err, HarvestRefusal::AlreadySpentElsewhere { .. }));
    }

    #[test]
    fn a_delegation_derives_one_permit_per_molecule_and_a_seal_derives_one() {
        let mission = mol("delib-20260819-cda2");
        let capability = delegated(&mission, "d34d");
        let a = mol("task-20260901-6da6");
        let b = mol("task-20260901-aaaa");
        assert_ne!(capability.permit_id(&a), capability.permit_id(&b));

        let seal = ratified(&a, &[]);
        assert_eq!(seal.permit_id(&a), seal.permit_id(&b));
    }

    #[test]
    fn a_fresh_permit_carries_the_key_that_authorised_it() {
        let molecule = mol("task-20260901-6da6");
        let out = authorize(
            &ratified(&molecule, &[]),
            &facts(&molecule),
            None,
            &AlwaysSealed,
        )
        .expect("authorised");
        match out {
            AuthorizedHarvest::Fresh(permit) => {
                assert_eq!(permit.key_id, OperatorKeyId::from_bytes([9; 8]));
                assert_eq!(permit.effect.molecule, molecule);
            }
            AuthorizedHarvest::AlreadyLanded(_) => panic!("nothing was consumed yet"),
        }
    }
}
