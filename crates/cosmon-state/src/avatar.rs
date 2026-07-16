// SPDX-License-Identifier: AGPL-3.0-only

//! Projection of an instance's event ledger into moule-or-avatar state.
//!
//! The fold is a pure function of the `events.jsonl` bytes — no clock,
//! no network, no state outside the ledger. Re-playing the same bytes
//! produces the same `InstanceProjection` bit-for-bit, including the
//! cicatrice (spec §6.2 idempotence).
//!
//! Spec: `docs/specs/avatar-incarnation-event-v1.md` (smithy d958).

use cosmon_core::avatar::{IncarnationAt, IncarnationError, InstanceEvent};
use cosmon_hash::Hash;

/// Projection of an instance after folding its `events.jsonl`.
///
/// Two ontological states:
/// - `Mould` — pre-incarnation, free, no bound pilote.
/// - `BoundInstance` — post-incarnation, avatar with cicatrice.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum InstanceProjection {
    /// The ledger is empty or its first event is not `IncarnationAt`.
    Mould,
    /// The ledger's line 1 is a valid `IncarnationAt`.
    BoundInstance(Box<BoundInstance>),
}

/// A fully incarnated instance — the avatar projection.
///
/// Named `BoundInstance` (not `Avatar`) per ADR-0020 §3 G1.
#[derive(Debug, Clone)]
pub struct BoundInstance {
    /// The incarnation event, captured verbatim for audit replay.
    pub incarnation: IncarnationAt,
    /// `blake3(canonical_bytes(line_1))` — the cicatrice (jr §peau-cicatrice).
    pub cicatrice: Hash,
}

impl InstanceProjection {
    /// Fold a sequence of instance events into a projection.
    ///
    /// Idempotent: `fold(events) == fold(events.clone())` for any input.
    /// The cicatrice is computed from the canonical serialization of the
    /// first event — deterministic across platforms.
    #[must_use]
    pub fn fold(events: &[InstanceEvent]) -> Self {
        match events.first() {
            Some(InstanceEvent::IncarnationAt(inc)) => {
                let cicatrice = cicatrice_of(inc);
                Self::BoundInstance(Box::new(BoundInstance {
                    incarnation: inc.clone(),
                    cicatrice,
                }))
            }
            Some(_) | None => Self::Mould,
        }
    }

    /// Fold from raw JSONL bytes — each line is one `InstanceEvent`.
    ///
    /// Ignores blank lines and lines that fail to parse (lenient read,
    /// strict on the first line only). Returns `Mould` if the first
    /// parseable line is not `IncarnationAt`.
    ///
    /// The cicatrice uses the raw bytes of line 1 (spec §5.3 — byte-exact,
    /// not semantic-equivalent). This is intentionally different from
    /// `fold()` which uses canonical serialization — `fold_raw` is the
    /// forensic path.
    #[must_use]
    pub fn fold_raw(raw: &[u8]) -> Self {
        let Some(first_line) = raw.split(|&b| b == b'\n').find(|l| !l.is_empty()) else {
            return Self::Mould;
        };

        let Ok(evt) = serde_json::from_slice::<InstanceEvent>(first_line) else {
            return Self::Mould;
        };

        let InstanceEvent::IncarnationAt(inc) = evt else {
            return Self::Mould;
        };
        let cicatrice = Hash::of_bytes(first_line);
        Self::BoundInstance(Box::new(BoundInstance {
            incarnation: inc,
            cicatrice,
        }))
    }

    /// Returns `true` if this projection is a `BoundInstance` (avatar).
    #[must_use]
    pub fn is_bound(&self) -> bool {
        matches!(self, Self::BoundInstance(_))
    }

    /// Returns `true` if this projection is a `Mould`.
    #[must_use]
    pub fn is_mould(&self) -> bool {
        matches!(self, Self::Mould)
    }

    /// Returns the bound instance if incarnated, `None` otherwise.
    #[must_use]
    pub fn as_bound(&self) -> Option<&BoundInstance> {
        match self {
            Self::BoundInstance(b) => Some(b),
            Self::Mould => None,
        }
    }
}

/// Compute the cicatrice from canonical serialization of the incarnation event.
fn cicatrice_of(inc: &IncarnationAt) -> Hash {
    let canonical = cosmon_hash::canonical_serialize(inc).expect("IncarnationAt is serializable");
    Hash::of_bytes(&canonical)
}

/// Validate whether an event can be appended to an existing ledger.
///
/// Enforces the three rules from spec §5.5:
/// - R1: no `IncarnationAt` after non-incarnation events
/// - R2: no duplicate `IncarnationAt`
/// - R3: non-incarnation events rejected on empty avatar-candidate ledger
///
/// `require_incarnation_first` controls R3 — set `true` for ledgers
/// that belong to avatar-candidate instances.
///
/// # Errors
/// Returns [`IncarnationError`] if the append would violate an invariant.
pub fn validate_pre_append(
    existing: &[InstanceEvent],
    proposed: &InstanceEvent,
    require_incarnation_first: bool,
) -> Result<(), IncarnationError> {
    let has_incarnation = existing
        .iter()
        .any(|e| matches!(e, InstanceEvent::IncarnationAt(_)));

    let is_incarnation = matches!(proposed, InstanceEvent::IncarnationAt(_));

    if is_incarnation {
        if has_incarnation {
            return Err(IncarnationError::AlreadyPresent);
        }
        if !existing.is_empty() {
            return Err(IncarnationError::AfterOther);
        }
    }

    if existing.is_empty() && require_incarnation_first && !is_incarnation {
        return Err(IncarnationError::PreIncarnationRejected);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use cosmon_core::auth::TenantId;
    use cosmon_core::avatar::*;

    fn sample_incarnation() -> IncarnationAt {
        IncarnationAt {
            ts: "2026-05-24T12:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            moule_sha: MouleSha::new(
                "blake3:7f4a3c2d1e0b9a8f7e6d5c4b3a291807f6e5d4c3b2a19087f6e5d4c3b2a19087",
            )
            .unwrap(),
            tenant_id: TenantId::new("democorp-internal").unwrap(),
            juridiction: JurisdictionCode::new("FR").unwrap(),
            pilote_id: PiloteId::new("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK")
                .unwrap(),
            instance_id: InstanceId::new("smithy-you-001").unwrap(),
            signature_pilote: Signature {
                algo: SignatureAlgo::Ed25519,
                sig_b64: "MEUCIQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
                key_id: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK#key-1".to_owned(),
            },
        }
    }

    fn sample_event() -> InstanceEvent {
        InstanceEvent::IncarnationAt(sample_incarnation())
    }

    // -- fold --

    #[test]
    fn empty_ledger_is_mould() {
        let projection = InstanceProjection::fold(&[]);
        assert!(projection.is_mould());
    }

    #[test]
    fn ledger_with_incarnation_is_bound() {
        let events = vec![sample_event()];
        let projection = InstanceProjection::fold(&events);
        assert!(projection.is_bound());
        let bound = projection.as_bound().unwrap();
        assert_eq!(bound.incarnation.tenant_id.as_str(), "democorp-internal");
        assert_eq!(bound.incarnation.juridiction.as_str(), "FR");
    }

    #[test]
    fn fold_is_idempotent() {
        let events = vec![sample_event()];
        let a = InstanceProjection::fold(&events);
        let b = InstanceProjection::fold(&events);
        let ca = a.as_bound().unwrap().cicatrice;
        let cb = b.as_bound().unwrap().cicatrice;
        assert_eq!(ca, cb, "cicatrice must be identical across fold calls");
    }

    #[test]
    fn fold_raw_matches_fold_cicatrice_shape() {
        let evt = sample_event();
        let json = serde_json::to_string(&evt).unwrap();
        let raw = format!("{json}\n");

        let from_fold = InstanceProjection::fold(&[evt]);
        let from_raw = InstanceProjection::fold_raw(raw.as_bytes());

        assert!(from_fold.is_bound());
        assert!(from_raw.is_bound());

        assert_eq!(
            from_raw.as_bound().unwrap().incarnation,
            from_fold.as_bound().unwrap().incarnation,
            "parsed incarnation must match"
        );
    }

    #[test]
    fn fold_raw_empty_is_mould() {
        assert!(InstanceProjection::fold_raw(b"").is_mould());
        assert!(InstanceProjection::fold_raw(b"\n\n").is_mould());
    }

    #[test]
    fn fold_raw_bad_json_is_mould() {
        assert!(InstanceProjection::fold_raw(b"not json at all\n").is_mould());
    }

    #[test]
    fn cicatrice_is_stable_across_reparse() {
        let evt = sample_event();
        let json = serde_json::to_string(&evt).unwrap();
        let raw = format!("{json}\n");

        let p1 = InstanceProjection::fold_raw(raw.as_bytes());
        let p2 = InstanceProjection::fold_raw(raw.as_bytes());
        assert_eq!(
            p1.as_bound().unwrap().cicatrice,
            p2.as_bound().unwrap().cicatrice,
            "cicatrice must survive full re-parse cycle"
        );
    }

    // -- validate_pre_append --

    #[test]
    fn validate_accepts_first_incarnation() {
        let result = validate_pre_append(&[], &sample_event(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_rejects_second_incarnation() {
        let existing = vec![sample_event()];
        let result = validate_pre_append(&existing, &sample_event(), false);
        assert!(matches!(result, Err(IncarnationError::AlreadyPresent)));
    }

    #[test]
    fn validate_rejects_double_incarnation() {
        let existing = vec![sample_event()];
        let second = sample_event();
        let result = validate_pre_append(&existing, &second, false);
        assert!(result.is_err());
    }

    #[test]
    fn validate_error_codes() {
        let err = IncarnationError::AlreadyPresent;
        assert!(err.to_string().contains("incarnation_already_present"));

        let err = IncarnationError::AfterOther;
        assert!(err.to_string().contains("incarnation_after_other"));

        let err = IncarnationError::PreIncarnationRejected;
        assert!(err.to_string().contains("pre_incarnation_event_rejected"));
    }
}
