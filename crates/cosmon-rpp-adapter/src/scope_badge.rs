// SPDX-License-Identifier: AGPL-3.0-only

//! `scope_badge` — the detached federation badge (`ScopeBadge`), MVP-B of
//! ADR-0023 (smithy).
//!
//! # What this is
//!
//! A [`ScopeBadge`] is a small, versioned, **detached** Ed25519 credential
//! that one instance (Dave) presents to another (Casey) to *prove an
//! identity tied to a specific galaxy*, **offline and sneakernet-first**. It
//! is the offline form of the live JWT bridge (MVP-A): a JWT needs the
//! issuer's JWKS reachable online, so it cannot make the *exit* sovereign;
//! a detached badge travels on a `git bundle` on a disk and survives the
//! death of its issuer (ADR-0023 §D4/§D6, DNA §D6 sneakernet-first).
//!
//! # The one rule it must not break: auth ≠ authz
//!
//! Verifying a badge answers exactly one question — *"is this byte string a
//! valid signature by the key I already trust, over claims that have not
//! expired?"* — and **creates no state and grants no access** (ADR-0023
//! §D2, the turing verdict). [`verify_badge`] is a pure function: it
//! authenticates. **Authorization is a separate, host-side decision**: the
//! receiving instance reads *its own* sealed pin (the `HabilitationMap`,
//! deny-by-default) and intersects what the badge *claims* with what the
//! pin *concedes*. A badge that over-declares is inert — the pin never
//! derives a grant from the badge. *La signature propose ; le pin dispose.*
//!
//! # Why CBOR, positional, with a domain separator
//!
//! The signed form is a **positional CBOR array** (not a map): positional
//! encoding has no map-key-ordering to canonicalize, and `ciborium` emits
//! preferred (shortest-int, definite-length) encodings, so the bytes are
//! reproducible on the verifier's side from the parsed fields alone — the
//! property a detached credential lives or dies by. A raw domain-separator
//! prefix ([`SCOPE_BADGE_DOMAIN_V1`]) is prepended before signing so a
//! signature minted for some *other* cosmon artifact (e.g. a
//! `cosmon-notary` mint commitment) can never be replayed as a badge.
//!
//! # Forward-compat without an enum-of-cases
//!
//! Per the torvalds garde-fou (ADR-0023 §MVP), the federated case must
//! **not** become an `enum LocalOrFederated` or a `bool external` that
//! contaminates admission. The only structural seam is the additive
//! [`FederatedProvenance`] carried as `Option<…>` on
//! [`crate::nucleon_map::Resolved`] — `None` for every local binding.
//!
//! # What does NOT live here
//!
//! - Computing a `galaxy-seed`. That is `cosmon_hash::galaxy_seed`
//!   (smithy G2): the badge *carries* a precomputed [`GalaxyRef::digest`]
//!   as an opaque [`Hash`]; it never recomputes it.
//! - Revocation lists. Revocation is **passive** (ADR-0023 §D5): short
//!   `not_after`, renewed by re-signing. There is no CRL.
//! - Any `ForgeFed` / `ActivityPub` type. The badge references no transport
//!   (ADR-0023 §D3); the bytes are the same down git-plain, `ForgeFed`, or a
//!   disk in a bag.

use ciborium::value::{Integer, Value};
use cosmon_hash::Hash;
use cosmon_notary::{Ed25519Scheme, PublicKey, Scheme, Signature as NotarySignature, SigningError};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Schema version emitted by this build. Bumped only on a
/// **breaking** change to the signed-field layout (a new field, a
/// reordering, a type change) — never for an additive,
/// non-signed-field change.
pub const SCOPE_BADGE_VERSION: u16 = 1;

/// Domain-separation tag prepended (raw, before the CBOR payload) to
/// the bytes that are signed and verified. Keeps a `ScopeBadge`
/// signature from colliding with any other Ed25519 signature in the
/// cosmon ecosystem (e.g. a `cosmon-notary` mint). The trailing NUL
/// makes it unambiguously not the start of a CBOR array header.
pub const SCOPE_BADGE_DOMAIN_V1: &[u8] = b"cosmon/scope-badge/v1\0";

/// Signature algorithm declared by a [`ScopeBadge`].
///
/// This is a closed, versioned enum of the algorithms a badge schema
/// *may declare* — distinct from `cosmon-notary`'s open `Scheme`
/// trait. It exists so the wire form carries a single compact tag and
/// the verifier dispatches without parsing a free-form string. New
/// algorithms are added here as new variants (each with a fixed CBOR
/// integer tag); old tags are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SigAlg {
    /// Ed25519 over `SCOPE_BADGE_DOMAIN_V1 ++ canonical-CBOR(signed
    /// fields)`. 32-byte public key, 64-byte signature. The only
    /// scheme this build verifies.
    Ed25519,
}

impl SigAlg {
    /// Stable CBOR integer tag for this algorithm. Persisted; never
    /// reassigned across versions.
    #[must_use]
    pub const fn cbor_tag(self) -> u8 {
        match self {
            SigAlg::Ed25519 => 1,
        }
    }

    /// Parse an algorithm from its CBOR integer tag.
    #[must_use]
    pub const fn from_cbor_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(SigAlg::Ed25519),
            _ => None,
        }
    }
}

/// Reference to the galaxy a badge is scoped to.
///
/// `name` is the human handle (`"speck"`) — convenient, *not* the
/// authority. `digest` is the **`galaxy-seed`**: `BLAKE3` of the
/// canonical `GenesisEvent` (smithy G2, `cosmon_hash::galaxy_seed`).
/// The digest is the self-certifying referent — the verifier can
/// recompute it from the received bundle and it survives the death of
/// the issuer and is independent of either instance's namespace
/// (ADR-0023 §D4). A badge proves *"I have a right to THIS"* by object
/// identity; it never enumerates a namespace prefix (no wildcard ⇒
/// fine granularity forced by the form, capability not ACL).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GalaxyRef {
    /// Human-facing galaxy handle. Display/routing only.
    pub name: String,
    /// The `galaxy-seed` (BLAKE3 of the canonical genesis). Serialized
    /// as lowercase hex in JSON, as a 32-byte CBOR byte string in the
    /// canonical signed form.
    #[ts(type = "string")]
    pub digest: Hash,
}

/// A detached, signed federation badge (ADR-0023 MVP-B).
///
/// Field order here is also the **canonical signed order**; the signed
/// payload is the positional CBOR array of every field *except* `sig`.
/// JSON serialization (used for the Rust→TS contract and for human
/// inspection) renders `digest` and `sig` as hex strings and `alg` as
/// a kebab string; the authoritative interchange form for
/// offline/sneakernet transit is [`ScopeBadge::to_cbor`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScopeBadge {
    /// Schema version (see [`SCOPE_BADGE_VERSION`]).
    pub v: u16,
    /// Declared signature algorithm.
    pub alg: SigAlg,
    /// Issuer identity — the federated instance that minted the badge
    /// (ADR-0023: the foreign `iss`). Authenticated, never trusted to
    /// self-authorize.
    pub iss: String,
    /// Subject — the machine/identity key handle the badge speaks for.
    pub sub: String,
    /// The galaxy this badge is scoped to (one object, never a prefix).
    pub galaxy: GalaxyRef,
    /// Expiry, Unix seconds (UTC). Passive revocation (ADR-0023 §D5):
    /// badges are short-lived and renewed by re-signing; a stale badge
    /// simply stops verifying.
    pub not_after: i64,
    /// Detached signature over `SCOPE_BADGE_DOMAIN_V1 ++
    /// canonical-CBOR(signed fields)`. Hex in JSON, raw CBOR byte
    /// string on the wire.
    #[ts(type = "string")]
    pub sig: HexBytes,
}

/// Opaque variable-length byte string that serializes as lowercase hex
/// (JSON) and as a CBOR byte string (canonical form). Used for the
/// signature so the badge's JSON projection stays a flat, TS-friendly
/// `string` rather than an array of integers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexBytes(pub Vec<u8>);

impl HexBytes {
    /// Borrow the raw bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for HexBytes {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&hex_encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        hex_decode(&s)
            .map(HexBytes)
            .map_err(serde::de::Error::custom)
    }
}

/// Authenticated claims returned by [`verify_badge`].
///
/// This is **not** an authorization. It carries only what the
/// signature *proved* — the receiving instance must still intersect it
/// with its own sealed pin (deny-by-default) before any access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBadge {
    /// Authenticated issuer.
    pub iss: String,
    /// Authenticated subject.
    pub sub: String,
    /// Authenticated galaxy reference (name + `galaxy-seed`).
    pub galaxy: GalaxyRef,
    /// Authenticated expiry (Unix seconds).
    pub not_after: i64,
}

/// Additive provenance recorded on a [`crate::nucleon_map::Resolved`]
/// when (and only when) the binding was admitted via a verified
/// federation badge rather than a local pin.
///
/// This is the *entire* structural footprint of the federated case in
/// the admission core (ADR-0023 §MVP garde-fou): an `Option<…>` field,
/// `None` for every local binding. There is deliberately **no**
/// `enum LocalOrFederated` and **no** `bool external` — the federated
/// path must not become an `if` that contaminates admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FederatedProvenance {
    /// The badge issuer (`iss`) whose signature was verified.
    pub badge_iss: String,
    /// The galaxy the badge authenticated.
    pub galaxy: GalaxyRef,
}

/// Errors from badge encoding, decoding, or verification.
#[derive(Debug, thiserror::Error)]
pub enum BadgeError {
    /// The badge declares a schema version this build does not
    /// understand. Fail closed (never best-effort a future layout).
    #[error("unsupported ScopeBadge version {0}; this build supports {SCOPE_BADGE_VERSION}")]
    UnsupportedVersion(u16),
    /// The badge declares an algorithm tag this build cannot verify.
    #[error("unsupported signature algorithm tag {0}")]
    UnsupportedAlg(u8),
    /// `now > not_after`: the badge has expired (passive revocation).
    #[error("badge expired: not_after={not_after}, now={now}")]
    Expired {
        /// The badge's declared expiry (Unix seconds).
        not_after: i64,
        /// The verifier's clock at check time (Unix seconds).
        now: i64,
    },
    /// The Ed25519 signature did not verify against the trusted key.
    #[error("signature verification failed: {0}")]
    Signature(#[from] SigningError),
    /// CBOR encode failure while building the canonical bytes.
    #[error("CBOR encode error: {0}")]
    CborEncode(String),
    /// CBOR decode failure or a malformed badge array on the wire.
    #[error("CBOR decode error: {0}")]
    CborDecode(String),
}

impl ScopeBadge {
    /// The signed fields, in canonical order, as CBOR `Value`s. The
    /// signature is **not** among them. `digest` becomes a 32-byte CBOR
    /// byte string; the integers use preferred (shortest) encoding.
    fn signed_values(&self) -> Vec<Value> {
        vec![
            Value::Integer(Integer::from(self.v)),
            Value::Integer(Integer::from(self.alg.cbor_tag())),
            Value::Text(self.iss.clone()),
            Value::Text(self.sub.clone()),
            Value::Text(self.galaxy.name.clone()),
            Value::Bytes(self.galaxy.digest.as_bytes().to_vec()),
            Value::Integer(Integer::from(self.not_after)),
        ]
    }

    /// The exact bytes that are signed and verified:
    /// `SCOPE_BADGE_DOMAIN_V1 ++ canonical-CBOR(array of signed
    /// fields)`. Deterministic: encoding the same fields twice yields
    /// identical bytes, and the verifier reproduces them from the
    /// parsed badge alone.
    ///
    /// # Errors
    ///
    /// [`BadgeError::CborEncode`] if CBOR serialization fails (it does
    /// not, for these value types — the arm exists for honesty).
    pub fn signing_bytes(&self) -> Result<Vec<u8>, BadgeError> {
        let mut buf = SCOPE_BADGE_DOMAIN_V1.to_vec();
        ciborium::into_writer(&Value::Array(self.signed_values()), &mut buf)
            .map_err(|e| BadgeError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Canonical CBOR wire form of the whole badge (signed fields **plus**
    /// the signature), as a positional array. This is the
    /// offline/sneakernet interchange artifact.
    ///
    /// # Errors
    ///
    /// [`BadgeError::CborEncode`] on CBOR serialization failure.
    pub fn to_cbor(&self) -> Result<Vec<u8>, BadgeError> {
        let mut values = self.signed_values();
        values.push(Value::Bytes(self.sig.0.clone()));
        let mut buf = Vec::new();
        ciborium::into_writer(&Value::Array(values), &mut buf)
            .map_err(|e| BadgeError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Parse a badge from its canonical CBOR wire form (the inverse of
    /// [`ScopeBadge::to_cbor`]). Structural only — does **not** verify
    /// the signature; call [`verify_badge`] for that.
    ///
    /// # Errors
    ///
    /// [`BadgeError::CborDecode`] if the bytes are not an 8-element badge
    /// array of the expected shape, or [`BadgeError::UnsupportedAlg`] if
    /// the algorithm tag is unknown.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, BadgeError> {
        let value: Value =
            ciborium::from_reader(bytes).map_err(|e| BadgeError::CborDecode(e.to_string()))?;
        let arr = match value {
            Value::Array(a) => a,
            other => {
                return Err(BadgeError::CborDecode(format!(
                    "expected CBOR array, got {other:?}"
                )))
            }
        };
        let [v, alg, iss, sub, name, digest, not_after, sig] = <[Value; 8]>::try_from(arr)
            .map_err(|a| {
                BadgeError::CborDecode(format!("expected 8 badge fields, got {}", a.len()))
            })?;

        let v = cbor_u16(&v, "v")?;
        let alg_tag = cbor_u8(&alg, "alg")?;
        let alg = SigAlg::from_cbor_tag(alg_tag).ok_or(BadgeError::UnsupportedAlg(alg_tag))?;
        let iss = cbor_text(iss, "iss")?;
        let sub = cbor_text(sub, "sub")?;
        let name = cbor_text(name, "galaxy.name")?;
        let digest = cbor_hash(&digest)?;
        let not_after = cbor_i64(&not_after, "not_after")?;
        let sig = cbor_bytes(sig, "sig")?;

        Ok(Self {
            v,
            alg,
            iss,
            sub,
            galaxy: GalaxyRef { name, digest },
            not_after,
            sig: HexBytes(sig),
        })
    }
}

/// Mint a badge: sign the canonical bytes with an Ed25519 scheme.
///
/// This is the issuer side (Dave). The verifier side never needs the
/// secret; it only needs the public key and [`verify_badge`].
///
/// # Errors
///
/// Propagates [`BadgeError::CborEncode`] from canonicalization or
/// [`BadgeError::Signature`] from the signer.
pub fn issue_badge(
    scheme: &Ed25519Scheme,
    iss: impl Into<String>,
    sub: impl Into<String>,
    galaxy: GalaxyRef,
    not_after: i64,
) -> Result<ScopeBadge, BadgeError> {
    // Build with a placeholder signature so `signing_bytes` (which
    // excludes `sig`) can be computed from the assembled struct.
    let mut badge = ScopeBadge {
        v: SCOPE_BADGE_VERSION,
        alg: SigAlg::Ed25519,
        iss: iss.into(),
        sub: sub.into(),
        galaxy,
        not_after,
        sig: HexBytes(Vec::new()),
    };
    let bytes = badge.signing_bytes()?;
    let sig = scheme.sign(&bytes)?;
    badge.sig = HexBytes(sig.to_bytes());
    Ok(badge)
}

/// **Authenticate** a badge — the pure oracle of ADR-0023 §D2.
///
/// Returns the badge's claims *iff* (1) the schema version is
/// supported, (2) the algorithm is supported, (3) `now <= not_after`,
/// and (4) the Ed25519 signature verifies against `pubkey` over the
/// canonical bytes. It creates no state and grants no access.
///
/// **This is not authorization.** The caller (the receiving instance)
/// must take the returned [`VerifiedBadge`] and intersect it with its
/// own host-side sealed pin (`HabilitationMap`, deny-by-default). A
/// verified badge whose `(iss, sub)` has no pin line resolves to *no
/// access* — the signature succeeds and authorization still fails.
///
/// `now` is the verifier's clock in Unix seconds, taken as a parameter
/// so the function stays pure and testable.
///
/// # Errors
///
/// [`BadgeError::UnsupportedVersion`], [`BadgeError::UnsupportedAlg`],
/// [`BadgeError::Expired`], or [`BadgeError::Signature`].
pub fn verify_badge(
    badge: &ScopeBadge,
    pubkey: &PublicKey,
    now: i64,
) -> Result<VerifiedBadge, BadgeError> {
    if badge.v != SCOPE_BADGE_VERSION {
        return Err(BadgeError::UnsupportedVersion(badge.v));
    }
    // The enum already constrains `alg`, but check explicitly so adding
    // a not-yet-verifiable variant later fails closed here rather than
    // silently mis-dispatching.
    match badge.alg {
        SigAlg::Ed25519 => {}
    }
    if now > badge.not_after {
        return Err(BadgeError::Expired {
            not_after: badge.not_after,
            now,
        });
    }

    let bytes = badge.signing_bytes()?;
    let signature = NotarySignature::new(Ed25519Scheme::TAG, badge.sig.as_slice());
    Ed25519Scheme::verify(pubkey, &bytes, &signature)?;

    Ok(VerifiedBadge {
        iss: badge.iss.clone(),
        sub: badge.sub.clone(),
        galaxy: badge.galaxy.clone(),
        not_after: badge.not_after,
    })
}

// --- small CBOR field extractors (keep `from_cbor` readable) ---------

fn cbor_text(v: Value, field: &str) -> Result<String, BadgeError> {
    match v {
        Value::Text(s) => Ok(s),
        other => Err(BadgeError::CborDecode(format!(
            "{field}: expected text, got {other:?}"
        ))),
    }
}

fn cbor_bytes(v: Value, field: &str) -> Result<Vec<u8>, BadgeError> {
    match v {
        Value::Bytes(b) => Ok(b),
        other => Err(BadgeError::CborDecode(format!(
            "{field}: expected bytes, got {other:?}"
        ))),
    }
}

fn cbor_i64(v: &Value, field: &str) -> Result<i64, BadgeError> {
    match v {
        Value::Integer(i) => i128::from(*i)
            .try_into()
            .map_err(|_| BadgeError::CborDecode(format!("{field}: integer out of i64 range"))),
        other => Err(BadgeError::CborDecode(format!(
            "{field}: expected integer, got {other:?}"
        ))),
    }
}

fn cbor_u16(v: &Value, field: &str) -> Result<u16, BadgeError> {
    let n = cbor_i64(v, field)?;
    u16::try_from(n).map_err(|_| BadgeError::CborDecode(format!("{field}: out of u16 range")))
}

fn cbor_u8(v: &Value, field: &str) -> Result<u8, BadgeError> {
    let n = cbor_i64(v, field)?;
    u8::try_from(n).map_err(|_| BadgeError::CborDecode(format!("{field}: out of u8 range")))
}

fn cbor_hash(v: &Value) -> Result<Hash, BadgeError> {
    match v {
        Value::Bytes(b) => {
            let arr: [u8; 32] = b.as_slice().try_into().map_err(|_| {
                BadgeError::CborDecode(format!("galaxy.digest: expected 32 bytes, got {}", b.len()))
            })?;
            Ok(Hash::from_bytes(arr))
        }
        other => Err(BadgeError::CborDecode(format!(
            "galaxy.digest: expected bytes, got {other:?}"
        ))),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string has odd length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("non-hex character"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_galaxy() -> GalaxyRef {
        // A stand-in galaxy-seed. In production this is
        // `cosmon_hash::galaxy_seed(genesis_line)` (smithy G2); the
        // badge only ever carries the precomputed digest.
        GalaxyRef {
            name: "speck".to_owned(),
            digest: Hash::of_bytes(b"speck genesis (test fixture)"),
        }
    }

    fn dave() -> Ed25519Scheme {
        Ed25519Scheme::generate_from_seed([7u8; 32])
    }

    #[test]
    fn issue_then_verify_roundtrips() {
        let scheme = dave();
        let badge = issue_badge(
            &scheme,
            "dave.example",
            "dave-key-1",
            fixture_galaxy(),
            2_000,
        )
        .unwrap();

        let verified = verify_badge(&badge, &scheme.public_key(), 1_000).unwrap();
        assert_eq!(verified.iss, "dave.example");
        assert_eq!(verified.sub, "dave-key-1");
        assert_eq!(verified.galaxy, fixture_galaxy());
    }

    #[test]
    fn signing_bytes_are_deterministic() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 9_999).unwrap();
        // Same fields, re-encoded, must produce identical bytes — the
        // detached-credential invariant.
        assert_eq!(
            badge.signing_bytes().unwrap(),
            badge.signing_bytes().unwrap()
        );
    }

    #[test]
    fn verifier_reconstructs_bytes_from_wire() {
        // The cross-instance path: Dave ships CBOR, Casey parses it and
        // verifies from the parsed fields alone (no shared in-memory state).
        let scheme = dave();
        let badge = issue_badge(&scheme, "dave", "k1", fixture_galaxy(), 5_000).unwrap();

        let wire = badge.to_cbor().unwrap();
        let parsed = ScopeBadge::from_cbor(&wire).unwrap();
        assert_eq!(parsed, badge);
        verify_badge(&parsed, &scheme.public_key(), 100).unwrap();
    }

    #[test]
    fn wire_form_is_byte_stable() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 42).unwrap();
        assert_eq!(badge.to_cbor().unwrap(), badge.to_cbor().unwrap());
    }

    #[test]
    fn wrong_key_is_refused() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 5_000).unwrap();
        let impostor = Ed25519Scheme::generate_from_seed([9u8; 32]);
        let err = verify_badge(&badge, &impostor.public_key(), 100).unwrap_err();
        assert!(matches!(err, BadgeError::Signature(_)));
    }

    #[test]
    fn tampered_claim_breaks_signature() {
        let scheme = dave();
        let mut badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 5_000).unwrap();
        // A receiver who escalates the scope to a neighbouring galaxy must
        // not get away with it: the signature covers the digest.
        badge.galaxy.name = "secret-neighbour".to_owned();
        let err = verify_badge(&badge, &scheme.public_key(), 100).unwrap_err();
        assert!(matches!(err, BadgeError::Signature(_)));
    }

    #[test]
    fn expired_badge_is_refused() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 1_000).unwrap();
        let err = verify_badge(&badge, &scheme.public_key(), 1_001).unwrap_err();
        assert!(matches!(
            err,
            BadgeError::Expired {
                not_after: 1_000,
                now: 1_001
            }
        ));
    }

    #[test]
    fn not_after_boundary_is_inclusive() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 1_000).unwrap();
        // now == not_after is still valid.
        verify_badge(&badge, &scheme.public_key(), 1_000).unwrap();
    }

    #[test]
    fn unsupported_version_fails_closed() {
        let scheme = dave();
        let mut badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 5_000).unwrap();
        badge.v = 99;
        let err = verify_badge(&badge, &scheme.public_key(), 100).unwrap_err();
        assert!(matches!(err, BadgeError::UnsupportedVersion(99)));
    }

    #[test]
    fn json_projection_is_flat_hex_strings() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 5_000).unwrap();
        let json = serde_json::to_value(&badge).unwrap();
        assert!(json["digest"].is_null()); // digest is nested under galaxy
        assert!(json["galaxy"]["digest"].is_string());
        assert!(json["sig"].is_string());
        assert_eq!(json["alg"], "ed25519");
        // Round-trips through JSON too (display/debug channel).
        let back: ScopeBadge = serde_json::from_value(json).unwrap();
        assert_eq!(back, badge);
    }

    #[test]
    fn sig_alg_tag_roundtrips() {
        assert_eq!(
            SigAlg::from_cbor_tag(SigAlg::Ed25519.cbor_tag()),
            Some(SigAlg::Ed25519)
        );
        assert_eq!(SigAlg::from_cbor_tag(0), None);
        assert_eq!(SigAlg::from_cbor_tag(2), None);
    }

    #[test]
    fn domain_separation_prefixes_signing_bytes() {
        let scheme = dave();
        let badge = issue_badge(&scheme, "iss", "sub", fixture_galaxy(), 5_000).unwrap();
        assert!(badge
            .signing_bytes()
            .unwrap()
            .starts_with(SCOPE_BADGE_DOMAIN_V1));
    }
}
