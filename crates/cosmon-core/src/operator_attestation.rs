// SPDX-License-Identifier: AGPL-3.0-only

//! The operator gesture, made unforgeable by its beneficiary (ADR-171).
//!
//! # The hole this closes
//!
//! [`crate::pilot_lease`] states that a transfer of the PRIMARY seat is an
//! operator gesture and never a pilot's own decision — TAKEOVER-SUPERVISED.
//! Until this module existed, that sentence was enforced by a free-text field:
//! `cs sessions takeover grant --to X --by emmanuel` recorded the string
//! `emmanuel` and nothing attested that a human had typed it. The M7 dogfood
//! (`task-20260731-bd92` §8, friction F1) found the predictable consequence in
//! the field: the agent that ran the grant was the agent the grant seated.
//!
//! A supervision that its beneficiary can forge is not supervision. So the
//! grant now carries an **attestation**: a detached signature, over bytes that
//! name exactly this transfer, by a key cosmon can check and cannot use.
//!
//! # Why cosmon verifies and never signs
//!
//! The adversary here is not a remote attacker. It is a process running as the
//! operator, on the operator's host, with the operator's files. Such a process
//! reads every secret on that disk and runs every binary the operator can run.
//! Against it, no `if` in this codebase is a barrier: it can set any variable,
//! allocate any tty, and append any line to any ledger.
//!
//! What it cannot do is produce a signature under a key it does not hold. So
//! the whole mechanism reduces to one rule, and the rule is about what is
//! *absent* from this repository:
//!
//! > **cosmon owns no stamp.** There is no signing path for the operator key
//! > anywhere in the shipped tree — [`crate::operator_attestation`] and its
//! > filestore verifier parse and check, and that is all they do.
//!
//! The operator signs out of band, with a standard tool (`minisign`), from a
//! secret that is passphrase-protected, on a token, or on another machine. The
//! passphrase lives in a head. That is the one place an agent on this host
//! cannot read.
//!
//! # What is still assumed, stated plainly
//!
//! Verification needs a trusted public key, and the file holding it sits on
//! the same disk. An agent may overwrite it with its own. This mechanism does
//! not make that impossible; it makes it **recorded and visible**: every grant
//! line carries the key id that signed it, so a substituted trust root shows
//! up as a key change in an append-only ledger, and pinning the key in a
//! *tracked* file makes the substitution a diff a human reads. The honest
//! claim is therefore not "forgery is impossible" but "forgery now requires
//! either a secret the agent lacks or a trust-root swap it cannot hide".
//!
//! # What is signed
//!
//! A [`GrantChallenge`] — the transfer itself, in canonical text:
//!
//! ```text
//! cosmon-takeover-grant-v1
//! mission=task-20260731-9cf4
//! holder=claude:8ae462b2
//! epoch=2
//! granted_by=emmanuel
//! ttl=none
//! ```
//!
//! The `epoch` line is what makes a captured attestation worthless: epochs are
//! strictly increasing per mission, so a signature for epoch 2 authorises the
//! transfer to epoch 2 and never the next one. Replay protection falls out of
//! the arithmetic that already prevents split-brain, with no nonce store to
//! keep and no clock to trust.
//!
//! `granted_by` is inside the signed bytes rather than beside them, which is
//! the direct repair of F1: the operator name is now a claim the signature
//! covers, not a string the caller chose.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::id::{MoleculeId, SessionId};
use crate::pilot_lease::LeaseEpoch;

/// Version tag of the canonical challenge encoding.
///
/// First line of the signed bytes so that a future encoding cannot be
/// confused with this one: a v2 challenge and a v1 challenge never share a
/// preimage, so a v1 signature can never be replayed as a v2 grant.
pub const CHALLENGE_V1_TAG: &str = "cosmon-takeover-grant-v1";

/// Why a challenge could not be composed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChallengeError {
    /// The operator name was empty, or was only whitespace.
    #[error("granted_by is empty — a grant must name the operator it claims")]
    OperatorNameEmpty,
    /// The operator name held a character that would break the line-oriented
    /// encoding. A newline in this field would let a caller append lines of
    /// its own to the signed text.
    #[error(
        "granted_by {found:?} holds a control character — it would forge a line of the challenge"
    )]
    OperatorNameNotOneLine {
        /// The rejected name, quoted so the offending byte is visible.
        found: String,
    },
    /// A negative time-to-live. The encoding has no representation for it and
    /// a lease that expired before it was granted is not a lease.
    #[error("ttl {seconds}s is negative — a lease cannot expire before it is granted")]
    TtlNegative {
        /// The rejected value.
        seconds: i64,
    },
}

/// The exact transfer an operator is asked to sign.
///
/// Every field that decides *who may fly and until when* is in here, and
/// nothing else is. A signature over these bytes therefore authorises one
/// transfer of one mission to one session at one epoch — not a class of them.
///
/// # Examples
///
/// ```
/// use cosmon_core::id::{MoleculeId, SessionId};
/// use cosmon_core::operator_attestation::GrantChallenge;
/// use cosmon_core::pilot_lease::LeaseEpoch;
///
/// let challenge = GrantChallenge::new(
///     MoleculeId::new("task-20260731-9cf4").unwrap(),
///     SessionId::new("claude-sid").unwrap(),
///     LeaseEpoch::first(),
///     "emmanuel",
///     None,
/// )
/// .unwrap();
///
/// assert_eq!(
///     challenge.to_string(),
///     "cosmon-takeover-grant-v1\n\
///      mission=task-20260731-9cf4\n\
///      holder=claude-sid\n\
///      epoch=1\n\
///      granted_by=emmanuel\n\
///      ttl=none\n"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantChallenge {
    /// Mission whose controls are transferred.
    pub mission_id: MoleculeId,
    /// Session that the grant would seat as PRIMARY.
    pub holder_session_id: SessionId,
    /// Epoch this transfer lands on. Strictly greater than every epoch already
    /// in the mission's ledger, which is what makes the signature single-use.
    pub epoch: LeaseEpoch,
    /// Operator name the grant claims. Signed, therefore no longer a claim the
    /// caller makes about itself.
    pub granted_by: String,
    /// Seconds of validity, or `None` for a lease that holds until superseded.
    pub ttl_seconds: Option<i64>,
}

impl GrantChallenge {
    /// Compose a challenge, rejecting an operator name the line-oriented
    /// encoding could not hold unambiguously.
    ///
    /// # Errors
    ///
    /// [`ChallengeError::OperatorNameEmpty`] for a blank name,
    /// [`ChallengeError::OperatorNameNotOneLine`] for one holding a control
    /// character, and [`ChallengeError::TtlNegative`] for a negative ttl.
    pub fn new(
        mission_id: MoleculeId,
        holder_session_id: SessionId,
        epoch: LeaseEpoch,
        granted_by: impl Into<String>,
        ttl_seconds: Option<i64>,
    ) -> Result<Self, ChallengeError> {
        let granted_by = granted_by.into();
        if granted_by.trim().is_empty() {
            return Err(ChallengeError::OperatorNameEmpty);
        }
        if granted_by.chars().any(char::is_control) {
            return Err(ChallengeError::OperatorNameNotOneLine { found: granted_by });
        }
        if let Some(secs) = ttl_seconds {
            if secs < 0 {
                return Err(ChallengeError::TtlNegative { seconds: secs });
            }
        }
        Ok(Self {
            mission_id,
            holder_session_id,
            epoch,
            granted_by,
            ttl_seconds,
        })
    }

    /// The bytes an operator signs, and the bytes a verifier checks.
    ///
    /// Identical to [`Display`](fmt::Display) so that a challenge written to a
    /// file by `cs sessions takeover challenge` and the challenge rebuilt from
    /// a ledger line are the same preimage — the operator can hand the printed
    /// file to stock `minisign` and cosmon will check exactly what was signed.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }
}

impl fmt::Display for GrantChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{CHALLENGE_V1_TAG}")?;
        writeln!(f, "mission={}", self.mission_id.as_str())?;
        writeln!(f, "holder={}", self.holder_session_id.as_str())?;
        writeln!(f, "epoch={}", self.epoch)?;
        writeln!(f, "granted_by={}", self.granted_by)?;
        match self.ttl_seconds {
            Some(secs) => writeln!(f, "ttl={secs}"),
            None => writeln!(f, "ttl=none"),
        }
    }
}

/// The eight-byte identity of an operator's signing key.
///
/// Carried in every grant so the ledger answers "which key seated this pilot"
/// without anyone having to still hold the key. Its [`Display`](fmt::Display)
/// matches what `minisign -G` prints, so an operator can compare the two by
/// eye rather than by decoding base64.
///
/// # Examples
///
/// ```
/// use cosmon_core::operator_attestation::OperatorKeyId;
///
/// let id = OperatorKeyId::from_bytes([0xb4, 0x1b, 0x01, 0x54, 0x76, 0x17, 0xe8, 0xd9]);
/// assert_eq!(id.to_string(), "D9E8177654011BB4");
/// assert_eq!(OperatorKeyId::parse("D9E8177654011BB4").unwrap(), id);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperatorKeyId([u8; 8]);

impl OperatorKeyId {
    /// Wrap the eight bytes as they appear inside a minisign key or signature.
    #[must_use]
    pub fn from_bytes(raw: [u8; 8]) -> Self {
        Self(raw)
    }

    /// The eight bytes, in file order.
    #[must_use]
    pub fn as_bytes(self) -> [u8; 8] {
        self.0
    }

    /// Read back a key id printed in minisign's display order.
    ///
    /// # Errors
    ///
    /// Returns the offending text if it is not sixteen hex digits.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.len() != 16 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "{text:?} is not a key id — expected sixteen hex digits"
            ));
        }
        let mut raw = [0u8; 8];
        for (i, slot) in raw.iter_mut().enumerate() {
            // Display order is the reverse of file order; index from the end.
            let at = 14 - i * 2;
            *slot = u8::from_str_radix(&text[at..at + 2], 16)
                .map_err(|e| format!("{text:?} is not a key id — {e}"))?;
        }
        Ok(Self(raw))
    }
}

impl fmt::Display for OperatorKeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter().rev() {
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl Serialize for OperatorKeyId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for OperatorKeyId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// A detached operator signature over a [`GrantChallenge`].
///
/// Field-for-field a minisign signature file, kept in that shape on purpose:
/// [`OperatorAttestation::to_minisig_file`] reconstitutes the exact four lines
/// stock `minisign -V` accepts, so a grant recorded by cosmon stays verifiable
/// by somebody who does not trust cosmon's verifier — or who no longer has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorAttestation {
    /// Which operator key signed. Also present inside `signature`; recorded
    /// here so the ledger is greppable without base64-decoding every line.
    pub key_id: OperatorKeyId,
    /// Base64 of the minisign signature blob (algorithm, key id, 64-byte
    /// signature). Left encoded because this record's job is transport, and
    /// decoding is the verifier's job.
    pub signature: String,
    /// Base64 of minisign's global signature, which covers the signature and
    /// the trusted comment together.
    pub global_signature: String,
    /// The trusted comment — signed, so it is evidence rather than decoration.
    #[serde(default)]
    pub trusted_comment: String,
    /// The untrusted comment. Reproduced verbatim for byte-identical
    /// reconstruction; it is covered by nothing and proves nothing.
    #[serde(default)]
    pub untrusted_comment: String,
}

impl OperatorAttestation {
    /// Render the four lines of a minisign signature file.
    ///
    /// # Examples
    ///
    /// ```
    /// use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};
    ///
    /// let att = OperatorAttestation {
    ///     key_id: OperatorKeyId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
    ///     signature: "AAAA".to_owned(),
    ///     global_signature: "BBBB".to_owned(),
    ///     trusted_comment: "timestamp:1".to_owned(),
    ///     untrusted_comment: "signature from minisign secret key".to_owned(),
    /// };
    /// assert_eq!(
    ///     att.to_minisig_file(),
    ///     "untrusted comment: signature from minisign secret key\n\
    ///      AAAA\n\
    ///      trusted comment: timestamp:1\n\
    ///      BBBB\n"
    /// );
    /// ```
    #[must_use]
    pub fn to_minisig_file(&self) -> String {
        format!(
            "untrusted comment: {}\n{}\ntrusted comment: {}\n{}\n",
            self.untrusted_comment, self.signature, self.trusted_comment, self.global_signature
        )
    }
}

/// Why an attestation did not authorise a transfer.
///
/// Enumerated rather than collapsed into one "invalid", for the same reason
/// [`crate::pilot_lease::RefusalReason`] is: the operator reading it has to
/// know whether to pin a key, re-sign the current epoch, or stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum AttestationError {
    /// No operator public key is pinned, so no grant can be checked — and an
    /// unchecked grant is refused rather than trusted. Deleting the trust root
    /// therefore stops transfers; it does not re-open forgery.
    #[error("no operator public key pinned — nothing can attest a transfer, so none is honoured")]
    NoTrustRoot,
    /// The grant carried no attestation at all.
    #[error("the grant carries no operator attestation — `--by` is a label, not a gesture")]
    Missing,
    /// The signature is well-formed but by a key that is not the pinned one.
    #[error("signed by key {presented} — the pinned operator key is {trusted}")]
    UnknownKey {
        /// Key that signed.
        presented: OperatorKeyId,
        /// Key that is trusted.
        trusted: OperatorKeyId,
    },
    /// The bytes could not be parsed as a minisign public key or signature.
    #[error("malformed attestation: {0}")]
    Malformed(String),
    /// The signature does not match the challenge. This is the case a
    /// beneficiary hits when it edits the mission, the holder, the epoch or
    /// the operator name after the operator signed.
    #[error("the signature does not cover this transfer — mission, holder, epoch, operator or ttl differ from what was signed")]
    DoesNotCoverTransfer,
    /// The trusted comment's own signature failed, so the comment is not
    /// evidence even though the main signature held.
    #[error("the trusted comment is not covered by its signature")]
    TrustedCommentUnsigned,
}

/// The port a lease store calls to decide whether a grant was supervised.
///
/// A trait rather than a function because the trust root is I/O — a file, a
/// token, a remote attestor — and this crate holds none. The domain states the
/// question; an adapter answers it.
pub trait OperatorGestureVerifier {
    /// Return `Ok(())` iff `attestation` is a valid operator signature over
    /// `challenge` by the trusted key.
    ///
    /// # Errors
    ///
    /// One [`AttestationError`] naming what an operator should do next.
    fn verify(
        &self,
        challenge: &GrantChallenge,
        attestation: &OperatorAttestation,
    ) -> Result<(), AttestationError>;

    /// The key this verifier trusts, for display in `takeover show`.
    fn trusted_key_id(&self) -> OperatorKeyId;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> MoleculeId {
        MoleculeId::new("task-20260731-9cf4").expect("fixture mission id")
    }

    fn sid(raw: &str) -> SessionId {
        SessionId::new(raw).expect("fixture session id")
    }

    fn challenge(granted_by: &str, ttl: Option<i64>) -> GrantChallenge {
        GrantChallenge::new(
            mission(),
            sid("claude-sid"),
            LeaseEpoch::first(),
            granted_by,
            ttl,
        )
        .expect("fixture challenge")
    }

    #[test]
    fn canonical_bytes_and_display_are_the_same_preimage() {
        let c = challenge("emmanuel", None);
        assert_eq!(c.canonical_bytes(), c.to_string().into_bytes());
    }

    #[test]
    fn a_ttl_changes_the_signed_bytes() {
        assert_ne!(
            challenge("emmanuel", None).canonical_bytes(),
            challenge("emmanuel", Some(900)).canonical_bytes()
        );
    }

    #[test]
    fn the_epoch_changes_the_signed_bytes_so_a_capture_is_single_use() {
        let first = challenge("emmanuel", None);
        let mut second = first.clone();
        second.epoch = first.epoch.next();
        assert_ne!(first.canonical_bytes(), second.canonical_bytes());
    }

    #[test]
    fn an_operator_name_cannot_smuggle_a_line_into_the_challenge() {
        let smuggled = "emmanuel\nholder=attacker";
        let err = GrantChallenge::new(
            mission(),
            sid("claude-sid"),
            LeaseEpoch::first(),
            smuggled,
            None,
        )
        .expect_err("a newline in granted_by must be refused");
        assert!(matches!(err, ChallengeError::OperatorNameNotOneLine { .. }));
    }

    #[test]
    fn a_blank_operator_name_is_refused() {
        assert_eq!(
            GrantChallenge::new(mission(), sid("s"), LeaseEpoch::first(), "   ", None),
            Err(ChallengeError::OperatorNameEmpty)
        );
    }

    #[test]
    fn a_negative_ttl_is_refused() {
        assert_eq!(
            GrantChallenge::new(mission(), sid("s"), LeaseEpoch::first(), "op", Some(-1)),
            Err(ChallengeError::TtlNegative { seconds: -1 })
        );
    }

    #[test]
    fn key_ids_round_trip_through_minisign_display_order() {
        let id = OperatorKeyId::from_bytes([0xb4, 0x1b, 0x01, 0x54, 0x76, 0x17, 0xe8, 0xd9]);
        assert_eq!(id.to_string(), "D9E8177654011BB4");
        assert_eq!(OperatorKeyId::parse(&id.to_string()), Ok(id));
    }

    #[test]
    fn a_key_id_that_is_not_sixteen_hex_digits_is_refused() {
        assert!(OperatorKeyId::parse("nope").is_err());
        assert!(OperatorKeyId::parse("D9E8177654011BB").is_err());
    }

    #[test]
    fn key_ids_serialise_as_the_string_an_operator_reads() {
        let id = OperatorKeyId::from_bytes([0xb4, 0x1b, 0x01, 0x54, 0x76, 0x17, 0xe8, 0xd9]);
        let json = serde_json::to_string(&id).expect("serialise key id");
        assert_eq!(json, "\"D9E8177654011BB4\"");
        assert_eq!(
            serde_json::from_str::<OperatorKeyId>(&json).expect("deserialise key id"),
            id
        );
    }
}
