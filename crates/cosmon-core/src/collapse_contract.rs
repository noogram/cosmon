// SPDX-License-Identifier: AGPL-3.0-only

//! The conditional, idempotent collapse contract (issue #207, ADR-187).
//!
//! An external broker confirms a collapse with an operator and then executes
//! it. To do that safely it must be able to say *which* state it confirmed
//! (an expected status and revision), to retry after a lost response without
//! collapsing twice (a caller idempotency key), and to read back what happened
//! (a receipt). This module is the pure half of that contract: the value
//! types, the request digest, and the decision table. The critical section
//! that reads the store, calls [`decide`] and writes the result lives in
//! `cosmon_state::ops::collapse`; nothing here performs I/O.
//!
//! # Decision table (first match wins)
//!
//! | Outcome | HTTP |
//! |---|---|
//! | [`CollapseOutcomeKind::IdempotencyKeyReused`] | 422 |
//! | [`CollapseOutcomeKind::Replayed`] | 200 |
//! | [`CollapseOutcomeKind::UnleasedPilotGesture`] | 403 |
//! | [`CollapseOutcomeKind::AlreadyCompleted`] | 409 |
//! | [`CollapseOutcomeKind::AlreadyCollapsed`] | 409 |
//! | [`CollapseOutcomeKind::ApprovalExpired`] | 412 |
//! | [`CollapseOutcomeKind::UnexpectedStatus`] | 409 |
//! | [`CollapseOutcomeKind::StaleRevision`] | 412 |
//! | [`CollapseOutcomeKind::Transitioned`] | 201 |
//!
//! Replay comes before every precondition: a retry after a lost response
//! must get the stored receipt back even though the revision moved (the
//! collapse itself changed it) and the approval may since have expired.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::MoleculeId;
use crate::molecule::MoleculeStatus;

/// Domain tag prefixed to the canonical intent before hashing.
pub const REQUEST_DIGEST_DOMAIN: &[u8] = b"cosmon.collapse-request.v1\0";

/// Domain tag prefixed to a key before hashing it into a file name.
pub const KEY_FILE_DOMAIN: &[u8] = b"cosmon.collapse-key.v1\0";

/// Schema tag carried by every receipt and key record, so a reader can
/// refuse a future layout instead of misreading it.
pub const RECEIPT_SCHEMA: &str = "cosmon.collapse-receipt.v1";

/// Prefix of every BLAKE3 token this module renders.
const B3_PREFIX: &str = "b3:";

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// A caller-generated idempotency key, `[A-Za-z0-9_-]{16,128}`.
///
/// The caller generates it once per intended collapse and resends it on
/// every retry. The charset stays mixed-case: the key never names a file
/// directly (see [`IdempotencyKey::file_stem`]), so a case-folding
/// filesystem cannot merge two keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Shortest accepted key.
    pub const MIN_LEN: usize = 16;
    /// Longest accepted key.
    pub const MAX_LEN: usize = 128;

    /// Validate a key.
    ///
    /// # Errors
    /// [`ContractError::InvalidKey`] when the length or charset is wrong.
    pub fn parse(raw: &str) -> Result<Self, ContractError> {
        let len_ok = (Self::MIN_LEN..=Self::MAX_LEN).contains(&raw.len());
        let charset_ok = raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if len_ok && charset_ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(ContractError::InvalidKey(raw.chars().take(140).collect()))
        }
    }

    /// The key as sent by the caller.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// File stem of this key's record: `hex(BLAKE3(KEY_FILE_DOMAIN || key))`.
    ///
    /// Hashing removes path traversal and case-folding collisions, which is
    /// what lets the key charset stay mixed-case.
    #[must_use]
    pub fn file_stem(&self) -> String {
        let mut bytes = KEY_FILE_DOMAIN.to_vec();
        bytes.extend_from_slice(self.0.as_bytes());
        cosmon_hash::Hash::of_bytes(&bytes).to_hex()
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = ContractError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<IdempotencyKey> for String {
    fn from(value: IdempotencyKey) -> Self {
        value.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate a `b3:<64 hex>` token.
fn parse_b3(raw: &str) -> Option<String> {
    let hex = raw.strip_prefix(B3_PREFIX)?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{B3_PREFIX}{}", hex.to_ascii_lowercase()))
}

/// Content revision of a molecule's state file, `b3:<hex>`.
///
/// BLAKE3 over the canonical JSON of the parsed file, computed on read and
/// never stored. Canonicalising the parsed value makes the token independent
/// of key order and whitespace (a `HashMap` rewrite does not move it) while
/// keeping unknown keys, so two binaries agree on it. Only equality is
/// meaningful: a revision has no order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MoleculeRevision(String);

impl MoleculeRevision {
    /// Compute the revision of a state file's bytes.
    ///
    /// # Errors
    /// [`ContractError::Revision`] when the bytes are not JSON.
    pub fn of_state_bytes(bytes: &[u8]) -> Result<Self, ContractError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| ContractError::Revision(e.to_string()))?;
        Self::of_value(&value)
    }

    /// Compute the revision of an already-parsed state value.
    ///
    /// # Errors
    /// [`ContractError::Revision`] when canonical serialization fails.
    pub fn of_value<T: Serialize>(value: &T) -> Result<Self, ContractError> {
        let hash =
            cosmon_hash::hash_value(value).map_err(|e| ContractError::Revision(e.to_string()))?;
        Ok(Self(format!("{B3_PREFIX}{}", hash.to_hex())))
    }

    /// Parse a revision a caller sent back.
    ///
    /// # Errors
    /// [`ContractError::InvalidRevision`] unless the token is `b3:<64 hex>`.
    pub fn parse(raw: &str) -> Result<Self, ContractError> {
        parse_b3(raw)
            .map(Self)
            .ok_or_else(|| ContractError::InvalidRevision(raw.chars().take(80).collect()))
    }

    /// The `b3:<hex>` rendering.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MoleculeRevision {
    type Error = ContractError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<MoleculeRevision> for String {
    fn from(value: MoleculeRevision) -> Self {
        value.0
    }
}

impl fmt::Display for MoleculeRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Digest of a collapse intent, `b3:<hex>`. Computed by cosmon and returned
/// to the caller, who compares it as an opaque string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestDigest(String);

impl RequestDigest {
    /// The `b3:<hex>` rendering.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RequestDigest {
    type Error = ContractError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_b3(&value)
            .map(Self)
            .ok_or_else(|| ContractError::InvalidDigest(value.chars().take(80).collect()))
    }
}

impl From<RequestDigest> for String {
    fn from(value: RequestDigest) -> Self {
        value.0
    }
}

impl fmt::Display for RequestDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A non-terminal status a caller may expect before collapsing.
///
/// A separate type so that "expect it to be collapsed" is not expressible:
/// a terminal expectation would be a request that can only be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollapsibleStatus {
    /// Created, not assigned.
    Pending,
    /// Assigned, not executing.
    Queued,
    /// Executing.
    Running,
    /// Suspended.
    Frozen,
    /// Refused service by an external authority.
    Starved,
}

impl CollapsibleStatus {
    /// The lifecycle status this expectation names.
    #[must_use]
    pub const fn status(self) -> MoleculeStatus {
        match self {
            Self::Pending => MoleculeStatus::Pending,
            Self::Queued => MoleculeStatus::Queued,
            Self::Running => MoleculeStatus::Running,
            Self::Frozen => MoleculeStatus::Frozen,
            Self::Starved => MoleculeStatus::Starved,
        }
    }

    /// Wire spelling, identical to [`MoleculeStatus`]'s.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Frozen => "frozen",
            Self::Starved => "starved",
        }
    }
}

impl FromStr for CollapsibleStatus {
    type Err = ContractError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "queued" => Ok(Self::Queued),
            "running" | "active" => Ok(Self::Running),
            "frozen" => Ok(Self::Frozen),
            "starved" => Ok(Self::Starved),
            other => Err(ContractError::InvalidExpectedStatus(other.to_owned())),
        }
    }
}

/// Who asks for a collapse, as the surface established it.
///
/// Passed explicitly into the operation rather than read from the
/// environment there: the CLI resolves it from its own environment, a
/// network surface from what it verified, and the server's environment is
/// never consulted for a network caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerContext {
    /// A local caller (CLI, stdio MCP). `session` is the pilot session its
    /// environment names, if any.
    Local {
        /// Session id resolved from the caller's environment.
        session: Option<crate::id::SessionId>,
    },
    /// A network caller (HTTP MCP). It has no mapped pilot session in v1, so
    /// it holds no lease (ADR-168 §D6, "unknown session ⇒ read-only").
    Remote {
        /// Verified subject of the bearer token.
        subject: String,
    },
}

impl CallerContext {
    /// The actor recorded in the intent digest.
    #[must_use]
    pub fn actor(&self) -> Option<String> {
        match self {
            Self::Local { session } => session.as_ref().map(|s| s.as_str().to_owned()),
            Self::Remote { subject } => Some(format!("jwt:{subject}")),
        }
    }
}

/// The conditions a keyed collapse carries. All-or-nothing on the surfaces:
/// a key without an expected status and revision is not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapseCondition {
    /// Caller idempotency key.
    pub key: IdempotencyKey,
    /// Status the caller confirmed.
    pub expect_status: CollapsibleStatus,
    /// Revision the caller confirmed.
    pub expect_revision: MoleculeRevision,
    /// Opaque digest of the operator approval, recorded in the intent.
    pub approval_digest: Option<String>,
    /// Deadline after which the approval no longer authorizes the collapse.
    pub not_after: Option<DateTime<Utc>>,
}

/// Every field a collapse request commits to, in the shape that is hashed.
///
/// All fields are always present (`null` when absent) so that adding a value
/// can never be confused with omitting one. Reason bytes are hashed exactly:
/// no trimming, no Unicode normalization.
#[derive(Debug, Clone, Serialize)]
pub struct CollapseIntentV1<'a> {
    /// Molecule to collapse.
    pub target: &'a str,
    /// Free-form reason, exact bytes.
    pub reason: &'a str,
    /// Structured cause as sent.
    pub cause: Option<&'a str>,
    /// Account alias as sent.
    pub account: Option<&'a str>,
    /// Quota kind as sent.
    pub kind: Option<&'a str>,
    /// Reason kind as sent.
    pub reason_kind: Option<&'a str>,
    /// Expected status.
    pub expected_status: &'a str,
    /// Expected revision.
    pub expected_revision: &'a str,
    /// Caller actor, see [`CallerContext::actor`].
    pub actor: Option<&'a str>,
    /// Approval digest as sent.
    pub approval_digest: Option<&'a str>,
    /// Approval deadline, RFC 3339 with nanoseconds.
    pub not_after: Option<String>,
}

impl CollapseIntentV1<'_> {
    /// `BLAKE3(REQUEST_DIGEST_DOMAIN || canonical_serialize(self))`.
    ///
    /// # Errors
    /// [`ContractError::Revision`] if canonical serialization fails, which
    /// only happens if `Serialize` itself fails.
    pub fn digest(&self) -> Result<RequestDigest, ContractError> {
        let body = cosmon_hash::canonical_serialize(self)
            .map_err(|e| ContractError::Revision(e.to_string()))?;
        let mut bytes = REQUEST_DIGEST_DOMAIN.to_vec();
        bytes.extend_from_slice(&body);
        Ok(RequestDigest(format!(
            "{B3_PREFIX}{}",
            cosmon_hash::Hash::of_bytes(&bytes).to_hex()
        )))
    }
}

/// Render a deadline the way the digest hashes it.
#[must_use]
pub fn render_not_after(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

/// What a committed keyed collapse returns, and what a retry gets back.
///
/// Stored twice: in the key's record file and embedded in the molecule's
/// state file, whose rename is the commit point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseReceipt {
    /// Always [`RECEIPT_SCHEMA`].
    pub schema: String,
    /// Key the collapse was admitted under.
    pub key: IdempotencyKey,
    /// Digest of the admitted intent.
    pub digest: RequestDigest,
    /// Molecule collapsed.
    pub molecule: MoleculeId,
    /// Status before the collapse.
    pub previous_status: MoleculeStatus,
    /// Revision the decision was taken on.
    pub previous_revision: MoleculeRevision,
    /// When the decision was taken.
    pub committed_at: DateTime<Utc>,
}

impl CollapseReceipt {
    /// Canonical bytes of the receipt. Two callers holding the same receipt
    /// render identical bytes, whichever process wrote it.
    ///
    /// # Errors
    /// [`ContractError::Revision`] if serialization fails.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ContractError> {
        cosmon_hash::canonical_serialize(self).map_err(|e| ContractError::Revision(e.to_string()))
    }
}

/// Phase of a key record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPhase {
    /// The key is claimed for this molecule and digest; the state write may
    /// or may not have happened.
    Reserved,
    /// The state write happened and the receipt is final.
    Committed,
}

/// Tenant-wide record of one key, `collapse-receipts/<file_stem>.json`.
///
/// Created once (create-only) as [`KeyPhase::Reserved`] and rewritten once
/// as [`KeyPhase::Committed`]. It binds the key to one molecule and one
/// digest for good: there is no expiry, because an expired record would turn
/// a known outcome into an unknown one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseKeyRecord {
    /// Always [`RECEIPT_SCHEMA`].
    pub schema: String,
    /// The key.
    pub key: IdempotencyKey,
    /// Molecule the key is bound to.
    pub molecule: MoleculeId,
    /// Digest the key is bound to.
    pub digest: RequestDigest,
    /// Reserved or committed.
    pub phase: KeyPhase,
    /// The receipt, once committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<CollapseReceipt>,
}

impl CollapseKeyRecord {
    /// A fresh reservation.
    #[must_use]
    pub fn reserved(key: IdempotencyKey, molecule: MoleculeId, digest: RequestDigest) -> Self {
        Self {
            schema: RECEIPT_SCHEMA.to_owned(),
            key,
            molecule,
            digest,
            phase: KeyPhase::Reserved,
            receipt: None,
        }
    }

    /// The committed form of this record.
    #[must_use]
    pub fn committed(&self, receipt: CollapseReceipt) -> Self {
        Self {
            phase: KeyPhase::Committed,
            receipt: Some(receipt),
            ..self.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Outcomes and the decision
// ---------------------------------------------------------------------------

/// Every outcome of a keyed collapse, in decision order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollapseOutcomeKind {
    /// The key is bound to another digest or another molecule.
    IdempotencyKeyReused,
    /// Same key and digest: the stored receipt is returned.
    Replayed,
    /// The mission is leased and the caller does not hold the lease.
    UnleasedPilotGesture,
    /// The molecule is completed.
    AlreadyCompleted,
    /// The molecule is collapsed (by another key, or without one).
    AlreadyCollapsed,
    /// `not_after` is in the past.
    ApprovalExpired,
    /// The molecule is not in the expected status.
    UnexpectedStatus,
    /// Same status, but the state changed since the caller read it.
    StaleRevision,
    /// The collapse happened now.
    Transitioned,
}

impl CollapseOutcomeKind {
    /// Wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdempotencyKeyReused => "idempotency_key_reused",
            Self::Replayed => "replayed",
            Self::UnleasedPilotGesture => "unleased_pilot_gesture",
            Self::AlreadyCompleted => "already_completed",
            Self::AlreadyCollapsed => "already_collapsed",
            Self::ApprovalExpired => "approval_expired",
            Self::UnexpectedStatus => "unexpected_status",
            Self::StaleRevision => "stale_revision",
            Self::Transitioned => "transitioned",
        }
    }

    /// HTTP status the API surfaces use for this outcome.
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::IdempotencyKeyReused => 422,
            Self::Replayed => 200,
            Self::UnleasedPilotGesture => 403,
            Self::AlreadyCompleted | Self::AlreadyCollapsed | Self::UnexpectedStatus => 409,
            Self::ApprovalExpired | Self::StaleRevision => 412,
            Self::Transitioned => 201,
        }
    }

    /// `true` for the two outcomes after which the molecule is collapsed
    /// under this key.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Replayed | Self::Transitioned)
    }
}

/// A refusal. Refusals write nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollapseRefusal {
    /// Key bound to another digest or molecule.
    IdempotencyKeyReused {
        /// Molecule the key is bound to.
        bound_molecule: MoleculeId,
    },
    /// Lease refusal, in the guard's words.
    UnleasedPilotGesture {
        /// Why the guard refused.
        why: String,
    },
    /// Molecule completed.
    AlreadyCompleted,
    /// Molecule collapsed.
    AlreadyCollapsed {
        /// Key of the collapse that happened, `None` for an unkeyed one.
        by_key: Option<IdempotencyKey>,
    },
    /// Approval deadline passed.
    ApprovalExpired {
        /// The deadline.
        not_after: DateTime<Utc>,
    },
    /// Status differs from the expectation.
    UnexpectedStatus {
        /// Status found.
        actual: MoleculeStatus,
    },
    /// Revision differs from the expectation.
    StaleRevision {
        /// Revision found.
        actual: MoleculeRevision,
    },
}

impl CollapseRefusal {
    /// The outcome this refusal reports.
    #[must_use]
    pub const fn kind(&self) -> CollapseOutcomeKind {
        match self {
            Self::IdempotencyKeyReused { .. } => CollapseOutcomeKind::IdempotencyKeyReused,
            Self::UnleasedPilotGesture { .. } => CollapseOutcomeKind::UnleasedPilotGesture,
            Self::AlreadyCompleted => CollapseOutcomeKind::AlreadyCompleted,
            Self::AlreadyCollapsed { .. } => CollapseOutcomeKind::AlreadyCollapsed,
            Self::ApprovalExpired { .. } => CollapseOutcomeKind::ApprovalExpired,
            Self::UnexpectedStatus { .. } => CollapseOutcomeKind::UnexpectedStatus,
            Self::StaleRevision { .. } => CollapseOutcomeKind::StaleRevision,
        }
    }
}

/// What the critical section should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollapseDecision {
    /// Write nothing; report the refusal.
    Refuse(CollapseRefusal),
    /// Return the stored receipt. `finalize` is `true` when the state file
    /// carries the receipt but the key record is still reserved — a crash
    /// between the commit point and the key commit — so the caller must
    /// finish the commit (and is the one that announces the transition).
    Replay {
        /// The receipt to return.
        receipt: CollapseReceipt,
        /// Whether the key record still has to be marked committed.
        finalize: bool,
    },
    /// Collapse now. `reserve` is `false` when a reservation with the same
    /// digest already exists (a retry after a crash before the state write).
    Admit {
        /// Whether the key record must still be created.
        reserve: bool,
    },
}

/// Everything [`decide`] reads, gathered inside the critical section.
#[derive(Debug, Clone, Copy)]
pub struct DecisionInput<'a> {
    /// Molecule targeted.
    pub target: &'a MoleculeId,
    /// The caller's conditions.
    pub condition: &'a CollapseCondition,
    /// Digest of the caller's intent.
    pub digest: &'a RequestDigest,
    /// The key's record, if any.
    pub key_record: Option<&'a CollapseKeyRecord>,
    /// Current status on disk.
    pub status: MoleculeStatus,
    /// Current revision on disk.
    pub revision: &'a MoleculeRevision,
    /// Receipt embedded in the current state, if any.
    pub state_receipt: Option<&'a CollapseReceipt>,
    /// The lease guard's refusal reason, if it refused.
    pub lease_refusal: Option<&'a str>,
    /// Decision time.
    pub now: DateTime<Utc>,
}

/// The decision table. Pure and total: every input yields exactly one row.
#[must_use]
pub fn decide(input: &DecisionInput<'_>) -> CollapseDecision {
    let key = &input.condition.key;

    // 1. Key bound to another digest or another molecule. The state's own
    //    receipt counts as a binding too: it survives a lost key file.
    if let Some(record) = input.key_record {
        if record.molecule != *input.target || record.digest != *input.digest {
            return CollapseDecision::Refuse(CollapseRefusal::IdempotencyKeyReused {
                bound_molecule: record.molecule.clone(),
            });
        }
    }
    let state_receipt_for_key = input.state_receipt.filter(|r| r.key == *key);
    if let Some(receipt) = state_receipt_for_key {
        if receipt.digest != *input.digest {
            return CollapseDecision::Refuse(CollapseRefusal::IdempotencyKeyReused {
                bound_molecule: receipt.molecule.clone(),
            });
        }
    }

    // 2. Replay — before every precondition.
    if let Some(receipt) = state_receipt_for_key {
        let finalize = input
            .key_record
            .is_none_or(|record| record.phase != KeyPhase::Committed);
        return CollapseDecision::Replay {
            receipt: receipt.clone(),
            finalize,
        };
    }
    if let Some(record) = input.key_record {
        if let (KeyPhase::Committed, Some(receipt)) = (record.phase, record.receipt.as_ref()) {
            return CollapseDecision::Replay {
                receipt: receipt.clone(),
                finalize: false,
            };
        }
    }

    // 3–8. Preconditions, in table order.
    if let Some(why) = input.lease_refusal {
        return CollapseDecision::Refuse(CollapseRefusal::UnleasedPilotGesture {
            why: why.to_owned(),
        });
    }
    match input.status {
        MoleculeStatus::Completed => {
            return CollapseDecision::Refuse(CollapseRefusal::AlreadyCompleted);
        }
        MoleculeStatus::Collapsed => {
            return CollapseDecision::Refuse(CollapseRefusal::AlreadyCollapsed {
                by_key: input.state_receipt.map(|r| r.key.clone()),
            });
        }
        _ => {}
    }
    if let Some(not_after) = input.condition.not_after {
        if input.now > not_after {
            return CollapseDecision::Refuse(CollapseRefusal::ApprovalExpired { not_after });
        }
    }
    if input.status != input.condition.expect_status.status() {
        return CollapseDecision::Refuse(CollapseRefusal::UnexpectedStatus {
            actual: input.status,
        });
    }
    if *input.revision != input.condition.expect_revision {
        return CollapseDecision::Refuse(CollapseRefusal::StaleRevision {
            actual: input.revision.clone(),
        });
    }

    // 9. Admit.
    CollapseDecision::Admit {
        reserve: input.key_record.is_none(),
    }
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// Answer of a lock-free receipt lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptLookup {
    /// The collapse happened under this key; here is its receipt.
    Committed(CollapseReceipt),
    /// The key is reserved for this molecule and digest, and the state does
    /// not carry the collapse. Resubmitting the identical request settles it.
    ReservedNotApplied {
        /// The digest the reservation is bound to.
        digest: RequestDigest,
    },
    /// The key record says committed and the state disagrees.
    Inconsistent {
        /// What disagrees.
        detail: String,
    },
    /// The key is bound to another molecule; its receipt is not disclosed.
    BoundElsewhere,
    /// Nothing was admitted under this key as of this read. Resubmitting the
    /// identical request settles it.
    Unknown,
}

impl ReceiptLookup {
    /// Wire label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Committed(_) => "committed",
            Self::ReservedNotApplied { .. } => "reserved_not_applied",
            Self::Inconsistent { .. } => "inconsistent",
            Self::BoundElsewhere => "idempotency_key_reused",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify a lookup from what was read, without a lock.
///
/// The state's rename is the commit point, so a receipt embedded in the
/// state for this key reads as committed even when the key record is still
/// reserved (a crash between the two writes).
#[must_use]
pub fn classify_lookup(
    target: &MoleculeId,
    key: &IdempotencyKey,
    key_record: Option<&CollapseKeyRecord>,
    status: MoleculeStatus,
    state_receipt: Option<&CollapseReceipt>,
) -> ReceiptLookup {
    if key_record.is_some_and(|r| r.molecule != *target) {
        return ReceiptLookup::BoundElsewhere;
    }
    let in_state = state_receipt.filter(|r| r.key == *key);
    match (key_record, in_state) {
        (None, None) => ReceiptLookup::Unknown,
        (record, Some(receipt)) => {
            if status != MoleculeStatus::Collapsed {
                return ReceiptLookup::Inconsistent {
                    detail: format!("state carries the receipt but its status is {status}"),
                };
            }
            if let Some(record) = record {
                if record.digest != receipt.digest {
                    return ReceiptLookup::Inconsistent {
                        detail: "key record and state receipt name different digests".to_owned(),
                    };
                }
                if let Some(stored) = record.receipt.as_ref() {
                    if stored != receipt {
                        return ReceiptLookup::Inconsistent {
                            detail: "key record and state carry different receipts".to_owned(),
                        };
                    }
                }
            }
            ReceiptLookup::Committed(receipt.clone())
        }
        (Some(record), None) => match record.phase {
            KeyPhase::Reserved => ReceiptLookup::ReservedNotApplied {
                digest: record.digest.clone(),
            },
            KeyPhase::Committed => ReceiptLookup::Inconsistent {
                detail: format!(
                    "key record is committed but the state of {target} does not carry its receipt (status {status})"
                ),
            },
        },
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Malformed contract input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// Key outside `[A-Za-z0-9_-]{16,128}`.
    #[error("invalid idempotency key `{0}`: expected [A-Za-z0-9_-]{{16,128}}")]
    InvalidKey(String),
    /// Expected revision not of the form `b3:<64 hex>`.
    #[error("invalid revision `{0}`: expected b3:<64 hex chars>")]
    InvalidRevision(String),
    /// Digest not of the form `b3:<64 hex>`.
    #[error("invalid digest `{0}`: expected b3:<64 hex chars>")]
    InvalidDigest(String),
    /// Expected status terminal or unknown.
    #[error(
        "invalid expected status `{0}`: expected one of pending, queued, running, frozen, starved"
    )]
    InvalidExpectedStatus(String),
    /// Revision could not be computed.
    #[error("cannot compute revision: {0}")]
    Revision(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> IdempotencyKey {
        IdempotencyKey::parse("broker-key-0001-abcdef").unwrap()
    }

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20261016-aaaa").unwrap()
    }

    fn rev(n: u8) -> MoleculeRevision {
        MoleculeRevision::of_value(&serde_json::json!({ "n": n })).unwrap()
    }

    fn condition() -> CollapseCondition {
        CollapseCondition {
            key: key(),
            expect_status: CollapsibleStatus::Running,
            expect_revision: rev(1),
            approval_digest: None,
            not_after: None,
        }
    }

    fn digest(reason: &str) -> RequestDigest {
        CollapseIntentV1 {
            target: "task-20261016-aaaa",
            reason,
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            expected_status: "running",
            expected_revision: rev(1).as_str(),
            actor: None,
            approval_digest: None,
            not_after: None,
        }
        .digest()
        .unwrap()
    }

    fn receipt(d: &RequestDigest) -> CollapseReceipt {
        CollapseReceipt {
            schema: RECEIPT_SCHEMA.to_owned(),
            key: key(),
            digest: d.clone(),
            molecule: mol(),
            previous_status: MoleculeStatus::Running,
            previous_revision: rev(1),
            committed_at: Utc::now(),
        }
    }

    fn input<'a>(
        condition: &'a CollapseCondition,
        digest: &'a RequestDigest,
        target: &'a MoleculeId,
        revision: &'a MoleculeRevision,
    ) -> DecisionInput<'a> {
        DecisionInput {
            target,
            condition,
            digest,
            key_record: None,
            status: MoleculeStatus::Running,
            revision,
            state_receipt: None,
            lease_refusal: None,
            now: Utc::now(),
        }
    }

    #[test]
    fn key_charset_and_length_are_enforced() {
        assert!(IdempotencyKey::parse("abcdefghijklmnop").is_ok());
        assert!(
            IdempotencyKey::parse("abcdefghijklmno").is_err(),
            "15 chars"
        );
        assert!(IdempotencyKey::parse(&"a".repeat(129)).is_err());
        assert!(IdempotencyKey::parse("abcdefghijklmn/../x").is_err());
        assert!(IdempotencyKey::parse("abcdefghijklmnop q").is_err());
    }

    #[test]
    fn keys_differing_only_in_case_get_distinct_files() {
        let lower = IdempotencyKey::parse("abcdefghijklmnop").unwrap();
        let upper = IdempotencyKey::parse("ABCDEFGHIJKLMNOP").unwrap();
        assert_ne!(lower.file_stem(), upper.file_stem());
        assert_eq!(lower.file_stem().len(), 64);
    }

    #[test]
    fn revision_ignores_key_order_and_whitespace() {
        let a = MoleculeRevision::of_state_bytes(br#"{"a":1,"b":{"c":2,"d":3}}"#).unwrap();
        let b = MoleculeRevision::of_state_bytes(b"{\n \"b\": {\"d\": 3, \"c\": 2},\n \"a\": 1\n}")
            .unwrap();
        assert_eq!(a, b);
        let c = MoleculeRevision::of_state_bytes(br#"{"a":1,"b":{"c":2,"d":4}}"#).unwrap();
        assert_ne!(a, c);
        assert!(a.as_str().starts_with("b3:"));
        assert_eq!(MoleculeRevision::parse(a.as_str()).unwrap(), a);
    }

    #[test]
    fn one_byte_of_reason_changes_the_digest() {
        assert_ne!(digest("operator approved"), digest("operator approved."));
        assert_eq!(digest("operator approved"), digest("operator approved"));
        // No normalization: a trailing space is a different request.
        assert_ne!(digest("x"), digest("x "));
    }

    #[test]
    fn a_fresh_matching_request_is_admitted_and_reserves() {
        let (c, d, m, r) = (condition(), digest("r"), mol(), rev(1));
        assert_eq!(
            decide(&input(&c, &d, &m, &r)),
            CollapseDecision::Admit { reserve: true }
        );
    }

    #[test]
    fn a_reservation_with_the_same_digest_is_reused() {
        let (c, d, m, r) = (condition(), digest("r"), mol(), rev(1));
        let record = CollapseKeyRecord::reserved(key(), mol(), d.clone());
        let mut i = input(&c, &d, &m, &r);
        i.key_record = Some(&record);
        assert_eq!(decide(&i), CollapseDecision::Admit { reserve: false });
    }

    #[test]
    fn reuse_is_refused_before_replay_and_on_another_molecule() {
        let (c, m, r) = (condition(), mol(), rev(1));
        let original = digest("r");
        let changed = digest("r.");
        let committed = CollapseKeyRecord::reserved(key(), mol(), original.clone())
            .committed(receipt(&original));
        let mut i = input(&c, &changed, &m, &r);
        i.key_record = Some(&committed);
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::IdempotencyKeyReused)
        );

        let other = MoleculeId::new("task-20261016-bbbb").unwrap();
        let mut i = input(&c, &original, &other, &r);
        i.key_record = Some(&committed);
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::IdempotencyKeyReused)
        );
    }

    #[test]
    fn replay_wins_over_every_precondition() {
        let (c, d, m) = (condition(), digest("r"), mol());
        let rcpt = receipt(&d);
        let committed =
            CollapseKeyRecord::reserved(key(), mol(), d.clone()).committed(rcpt.clone());
        let moved = rev(9);
        let mut c = c;
        c.not_after = Some(Utc::now() - chrono::Duration::hours(1));
        let mut i = input(&c, &d, &m, &moved);
        i.key_record = Some(&committed);
        i.status = MoleculeStatus::Collapsed;
        i.state_receipt = Some(&rcpt);
        i.lease_refusal = Some("not the holder");
        assert_eq!(
            decide(&i),
            CollapseDecision::Replay {
                receipt: rcpt,
                finalize: false
            }
        );
    }

    #[test]
    fn a_receipt_in_state_with_a_reserved_record_must_be_finalized() {
        let (c, d, m, r) = (condition(), digest("r"), mol(), rev(2));
        let rcpt = receipt(&d);
        let record = CollapseKeyRecord::reserved(key(), mol(), d.clone());
        let mut i = input(&c, &d, &m, &r);
        i.key_record = Some(&record);
        i.status = MoleculeStatus::Collapsed;
        i.state_receipt = Some(&rcpt);
        assert_eq!(
            decide(&i),
            CollapseDecision::Replay {
                receipt: rcpt,
                finalize: true
            }
        );
    }

    #[test]
    fn preconditions_refuse_in_table_order() {
        let (c, d, m) = (condition(), digest("r"), mol());
        let stale = rev(2);

        // Lease before completed.
        let mut i = input(&c, &d, &m, &stale);
        i.status = MoleculeStatus::Completed;
        i.lease_refusal = Some("why");
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::UnleasedPilotGesture)
        );

        // Completed; collapsed by another key.
        i.lease_refusal = None;
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::AlreadyCompleted)
        );
        let other_key_receipt = CollapseReceipt {
            key: IdempotencyKey::parse("another-broker-key-01").unwrap(),
            ..receipt(&digest("other"))
        };
        i.status = MoleculeStatus::Collapsed;
        i.state_receipt = Some(&other_key_receipt);
        assert_eq!(
            decide(&i),
            CollapseDecision::Refuse(CollapseRefusal::AlreadyCollapsed {
                by_key: Some(other_key_receipt.key.clone())
            })
        );

        // Expired before status and revision.
        let mut expired = condition();
        expired.not_after = Some(Utc::now() - chrono::Duration::seconds(1));
        let mut i = input(&expired, &d, &m, &stale);
        i.status = MoleculeStatus::Frozen;
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::ApprovalExpired)
        );

        // Wrong status and stale revision are distinct.
        let mut i = input(&c, &d, &m, &stale);
        i.status = MoleculeStatus::Frozen;
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::UnexpectedStatus)
        );
        i.status = MoleculeStatus::Running;
        assert_eq!(
            decide(&i).refusal_kind(),
            Some(CollapseOutcomeKind::StaleRevision)
        );
    }

    #[test]
    fn lookup_reads_the_commit_point() {
        let d = digest("r");
        let rcpt = receipt(&d);
        let reserved = CollapseKeyRecord::reserved(key(), mol(), d.clone());
        let committed = reserved.committed(rcpt.clone());
        let m = mol();
        let k = key();

        assert_eq!(
            classify_lookup(&m, &k, None, MoleculeStatus::Running, None),
            ReceiptLookup::Unknown
        );
        assert_eq!(
            classify_lookup(&m, &k, Some(&reserved), MoleculeStatus::Running, None),
            ReceiptLookup::ReservedNotApplied { digest: d.clone() }
        );
        assert_eq!(
            classify_lookup(
                &m,
                &k,
                Some(&reserved),
                MoleculeStatus::Collapsed,
                Some(&rcpt)
            ),
            ReceiptLookup::Committed(rcpt.clone())
        );
        assert_eq!(
            classify_lookup(
                &m,
                &k,
                Some(&committed),
                MoleculeStatus::Collapsed,
                Some(&rcpt)
            ),
            ReceiptLookup::Committed(rcpt.clone())
        );
        assert_eq!(
            classify_lookup(&m, &k, Some(&committed), MoleculeStatus::Running, None).as_str(),
            "inconsistent"
        );
        let other = MoleculeId::new("task-20261016-bbbb").unwrap();
        assert_eq!(
            classify_lookup(&other, &k, Some(&committed), MoleculeStatus::Running, None),
            ReceiptLookup::BoundElsewhere
        );
    }

    #[test]
    fn outcome_labels_and_codes_match_the_issue_table() {
        use CollapseOutcomeKind as K;
        let table = [
            (K::IdempotencyKeyReused, "idempotency_key_reused", 422),
            (K::Replayed, "replayed", 200),
            (K::UnleasedPilotGesture, "unleased_pilot_gesture", 403),
            (K::AlreadyCompleted, "already_completed", 409),
            (K::AlreadyCollapsed, "already_collapsed", 409),
            (K::ApprovalExpired, "approval_expired", 412),
            (K::UnexpectedStatus, "unexpected_status", 409),
            (K::StaleRevision, "stale_revision", 412),
            (K::Transitioned, "transitioned", 201),
        ];
        for (kind, label, code) in table {
            assert_eq!(kind.as_str(), label);
            assert_eq!(kind.http_status(), code);
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(label)
            );
        }
    }

    impl CollapseDecision {
        fn refusal_kind(&self) -> Option<CollapseOutcomeKind> {
            match self {
                Self::Refuse(r) => Some(r.kind()),
                _ => None,
            }
        }
    }
}
