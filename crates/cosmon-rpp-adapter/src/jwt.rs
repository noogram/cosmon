// SPDX-License-Identifier: AGPL-3.0-only

//! JWT validation — clause (a) of the §8j HTTPS+JWT instantiation.
//!
//! The verifier enforces a strict whitelist of signature algorithms
//! (RS256 / ES256 only — turing G1), pins the trusted issuers
//! host-side, refuses `jku` / `x5u` claim-driven key resolution
//! (turing G7/G8), and applies posture-aware lifetime caps.
//!
//! # Key delivery — host-side allowlist, not "never fetch"
//!
//! The signing keys (`(iss, kid) → DecodingKey`) reach this store by
//! one of two delivery paths, both equivalent from the validator's
//! point of view:
//!
//! - **HTTP-fetch (primary, OIDC standard)** — for each issuer in the
//!   host-side allowlist (`<state_dir>/security/trusted-issuers.toml`),
//!   the adapter fetches the JWKS from that issuer's `jwks_uri` and
//!   refreshes it periodically (see [`crate::jwks_fetch`]). This is the
//!   provisioning path that replaces the file-stage + `SIGHUP` of v2.4
//!   (smithy spec `jwks-http-fetch-provisioning.md`).
//! - **File-stage (compat fallback)** — [`JwksStore::load`] reads
//!   `<state_dir>/security/jwks/*.json` and the `SIGHUP` listener
//!   ([`crate::reload::reload_jwks`]) re-reads it. Kept for the test
//!   bench and the `oidc-mock`; superseded by HTTP-fetch in prod.
//!
//! **The structural defence is the host-side allowlist + the authz
//! pin, not the absence of a network round-trip.** Earlier this module
//! forbade fetching ("a stolen JWKS endpoint cannot inject forged
//! signing keys"). That was a *belt* over the *braces*: the real
//! ceiling on an `IdP` compromise is the `(iss, sub) → noyau` pin
//! ([`crate::nucleon_map`], deny-by-default) — a forged-but-valid
//! signature still resolves to nothing it is not pinned to open. The
//! HTTP-fetch path replaces the belt with two equivalent, standard
//! guards: (1) the adapter only ever fetches issuers **declared
//! host-side** — an arbitrary endpoint is never contacted nor trusted;
//! (2) the `jwks_uri` comes **only** from that host-side list (or the
//! issuer's own `.well-known`), never from a token claim (`jku`/`x5u`
//! stay refused). Net: same security posture, the manual rotation
//! gesture removed. See smithy spec §4 (posture (b) preserved) and
//! ADR-0023 (multi-issuer MVP-A).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_swap::{ArcSwap, Guard};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::error::RppRejectReason;
use crate::Posture;

/// Maximum JWT `exp - iat` accepted in `Active` posture (15 min, per
/// ADR-080 §6.5).
pub const ACTIVE_MAX_LIFETIME_SEC: u64 = 15 * 60;

/// Maximum JWT `exp - iat` accepted in `Prepared` posture (24 h, with
/// a warning logged on every laxity).
pub const PREPARED_MAX_LIFETIME_SEC: u64 = 24 * 60 * 60;

/// JWT algorithm whitelist. Anything else (including `none`, `HS256`,
/// `EdDSA`, `RS384`, `RS512`) is rejected at parse time.
pub const ALG_WHITELIST: &[Algorithm] = &[Algorithm::RS256, Algorithm::ES256];

/// Pinned JWKS store — loaded once at boot, consulted on every request.
#[derive(Clone, Debug, Default)]
pub struct JwksStore {
    /// `kid → DecodingKey` per issuer.
    by_iss_kid: HashMap<(String, String), JwkRecord>,
    /// Allowed (`iss`, `aud`) pairs. The validator rejects requests
    /// whose `iss` is not in this set (clause a — issuer pinning).
    allowed_audiences: HashMap<String, Vec<String>>,
}

/// One key record carried by the store.
#[derive(Clone)]
struct JwkRecord {
    decoding_key: DecodingKey,
    alg: Algorithm,
}

impl std::fmt::Debug for JwkRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The DecodingKey carries opaque key material we deliberately
        // do not Debug-print; the alg is the only safe field to surface.
        f.debug_struct("JwkRecord")
            .field("alg", &self.alg)
            .field("decoding_key", &"<opaque>")
            .finish()
    }
}

/// On-disk representation of a JWKS file (subset). The file layout
/// mirrors RFC 7517 — the keys list is materialised under
/// `<state_dir>/security/jwks/<iss_hash>.json`.
#[derive(Debug, Deserialize)]
struct JwksFile {
    keys: Vec<Jwk>,
    /// Issuer the file pins. The boot loader stores the JWKS by this
    /// issuer; the file name is purely informational.
    #[serde(rename = "iss")]
    issuer: String,
    /// Allowed audiences for this issuer.
    #[serde(default)]
    audiences: Vec<String>,
}

/// `kty` per RFC 7517 §4.1 — case-sensitive string. The RFC fixes
/// `"RSA"` and `"EC"` (uppercase); we accept the lowercase spellings
/// as a lenient courtesy so hand-crafted JWKS fixtures keep loading.
#[derive(Debug, Deserialize)]
enum KeyKind {
    #[serde(rename = "RSA", alias = "rsa")]
    Rsa,
    #[serde(rename = "EC", alias = "ec")]
    Ec,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: String,
    alg: String,
    kty: KeyKind,
    /// RSA modulus (`n` in JWK), base64url. Required for `kty=RSA`.
    #[serde(default)]
    n: Option<String>,
    /// RSA public exponent (`e` in JWK), base64url. Required for `kty=RSA`.
    #[serde(default)]
    e: Option<String>,
    /// EC `x` coordinate, base64url. Required for `kty=EC`.
    #[serde(default)]
    x: Option<String>,
    /// EC `y` coordinate, base64url. Required for `kty=EC`.
    #[serde(default)]
    y: Option<String>,
    /// EC curve, e.g. `"P-256"`. Required for `kty=EC`. The
    /// algorithm whitelist (only ES256 / P-256 in V0) makes this
    /// field load-bearing for parse-time rejection of `P-384`+ keys
    /// that would otherwise satisfy `decode_jwk`.
    #[serde(default)]
    crv: Option<String>,
}

impl JwksStore {
    /// Load every JWKS file under `<state_dir>/security/jwks/`.
    /// Malformed files are logged and skipped.
    ///
    /// # Errors
    ///
    /// Returns the underlying IO error if the directory cannot be
    /// enumerated. Per-file failures are tolerated.
    pub fn load(state_dir: &Path) -> std::io::Result<Self> {
        let mut out = Self::default();
        let dir = state_dir.join("security/jwks");
        if !dir.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable jwks file");
                    continue;
                }
            };
            let file: JwksFile = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping malformed jwks file");
                    continue;
                }
            };
            for jwk in file.keys {
                let Some(alg) = parse_alg(&jwk.alg) else {
                    tracing::warn!(alg = %jwk.alg, "skipping JWK with non-whitelisted alg");
                    continue;
                };
                let Some(decoding_key) = decode_jwk(&jwk) else {
                    tracing::warn!(kid = %jwk.kid, "skipping malformed JWK");
                    continue;
                };
                out.by_iss_kid.insert(
                    (file.issuer.clone(), jwk.kid.clone()),
                    JwkRecord { decoding_key, alg },
                );
            }
            out.allowed_audiences
                .insert(file.issuer.clone(), file.audiences);
        }
        Ok(out)
    }

    /// Construct a store directly from in-memory keys (test helper).
    #[must_use]
    pub fn from_pem(
        issuer: impl Into<String>,
        kid: impl Into<String>,
        alg: Algorithm,
        decoding_key: DecodingKey,
        audiences: Vec<String>,
    ) -> Self {
        let issuer = issuer.into();
        let mut store = Self::default();
        store.by_iss_kid.insert(
            (issuer.clone(), kid.into()),
            JwkRecord { decoding_key, alg },
        );
        store.allowed_audiences.insert(issuer, audiences);
        store
    }

    /// Add a *second* (or N-th) issuer to an existing store, in memory.
    ///
    /// This is the in-memory counterpart of dropping another
    /// `<iss>.json` under `<state_dir>/security/jwks/`: the store is
    /// already multi-issuer (keyed by `(iss, kid)`), so learning a
    /// federated peer's JWKS is purely additive — no new type, no
    /// special "federated" bucket. Used to construct the
    /// Dave↔Casey↔`speck` two-issuer fixture (ADR-0023 MVP-A) without
    /// round-tripping through disk; the boot path
    /// ([`Self::load`](Self::load)) reaches the same shape by reading two
    /// files.
    #[must_use]
    pub fn with_pem(
        mut self,
        issuer: impl Into<String>,
        kid: impl Into<String>,
        alg: Algorithm,
        decoding_key: DecodingKey,
        audiences: Vec<String>,
    ) -> Self {
        let issuer = issuer.into();
        self.by_iss_kid.insert(
            (issuer.clone(), kid.into()),
            JwkRecord { decoding_key, alg },
        );
        self.allowed_audiences.insert(issuer, audiences);
        self
    }

    /// Return the pinned key + algorithm for `(iss, kid)`, or `None`
    /// if the issuer is not pinned or the `kid` is unknown.
    #[must_use]
    pub fn get(&self, iss: &str, kid: &str) -> Option<(&DecodingKey, Algorithm)> {
        self.by_iss_kid
            .get(&(iss.to_owned(), kid.to_owned()))
            .map(|r| (&r.decoding_key, r.alg))
    }

    /// Return the pinned audiences for `iss`, or `None` if the issuer
    /// is not pinned at boot.
    #[must_use]
    pub fn audiences_for(&self, iss: &str) -> Option<&[String]> {
        self.allowed_audiences.get(iss).map(Vec::as_slice)
    }

    /// Return the per-issuer key counts, sorted by issuer for stable
    /// boot-log output. Used by `cosmon-rpp-adapter` to surface JWKS
    /// loading evidence at startup; one info line per issuer makes
    /// silent-empty-JWKS bugs trivially greppable.
    #[must_use]
    pub fn key_counts_by_issuer(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (iss, _kid) in self.by_iss_kid.keys() {
            *counts.entry(iss.clone()).or_default() += 1;
        }
        // Issuers without keys (pinned audience-only entries) still
        // matter at boot — surface them with a zero count so the
        // operator sees the audience pin and the missing keys.
        for iss in self.allowed_audiences.keys() {
            counts.entry(iss.clone()).or_insert(0);
        }
        let mut out: Vec<_> = counts.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// `true` when a key for `(iss, kid)` is pinned. The cache-miss
    /// trigger ([`crate::jwks_fetch::JwksProvider::ensure_kid`]) reads
    /// this to decide whether an on-demand refetch is warranted: a token
    /// whose `kid` is already present needs no network round-trip.
    #[must_use]
    pub fn contains_kid(&self, iss: &str, kid: &str) -> bool {
        self.by_iss_kid
            .contains_key(&(iss.to_owned(), kid.to_owned()))
    }

    /// Replace every key pinned for `issuer` with the keys carried by a
    /// fetched RFC 7517 JWKS document (`{ "keys": [...] }`), and pin
    /// `audiences` for that issuer. This is the **HTTP-fetch seam**: the
    /// network layer ([`crate::jwks_fetch`]) owns the round-trip, this
    /// method owns the parse + algorithm whitelist — byte-for-byte the
    /// same acceptance logic as the boot-file path ([`Self::load`]), so
    /// a key that loads from disk loads from the wire and vice versa.
    ///
    /// Existing keys for `issuer` are dropped first, so an upstream
    /// rotation that *removes* a `kid` is reflected once the new document
    /// is fetched. Keys for *other* issuers are untouched (the store is
    /// keyed by `(iss, kid)`), so a single-issuer refetch never disturbs
    /// a federated peer's keys. Returns the count of whitelisted,
    /// well-formed keys inserted (malformed or non-whitelisted keys are
    /// skipped, exactly as on the file path).
    ///
    /// # Errors
    ///
    /// Returns the deserialisation error if `jwks_json` is not a valid
    /// JWKS document. A parse failure leaves the store **unchanged** (the
    /// existing keys for `issuer` survive — fail-closed never regresses a
    /// live key to empty).
    pub fn replace_remote_jwks(
        &mut self,
        issuer: &str,
        audiences: Vec<String>,
        jwks_json: &str,
    ) -> Result<usize, serde_json::Error> {
        // Parse FIRST, before mutating — a malformed document must not
        // wipe the live keys for this issuer (fail-closed).
        let doc: RemoteJwksDoc = serde_json::from_str(jwks_json)?;
        self.by_iss_kid.retain(|(iss, _kid), _| iss != issuer);
        let mut inserted = 0;
        for jwk in doc.keys {
            let Some(alg) = parse_alg(&jwk.alg) else {
                tracing::warn!(alg = %jwk.alg, "skipping fetched JWK with non-whitelisted alg");
                continue;
            };
            let Some(decoding_key) = decode_jwk(&jwk) else {
                tracing::warn!(kid = %jwk.kid, "skipping malformed fetched JWK");
                continue;
            };
            self.by_iss_kid.insert(
                (issuer.to_owned(), jwk.kid.clone()),
                JwkRecord { decoding_key, alg },
            );
            inserted += 1;
        }
        self.allowed_audiences.insert(issuer.to_owned(), audiences);
        Ok(inserted)
    }
}

/// A JWKS document fetched over HTTP (RFC 7517 §5 — `{ "keys": [...] }`).
/// Unlike the on-disk [`JwksFile`], the wire form carries **no** `iss`
/// or `audiences`: those come host-side from `trusted-issuers.toml`
/// (the `iss` that matches the token is the *external* issuer URL, while
/// the `jwks_uri` the document was fetched from is the *internal*
/// container address — smithy spec §2.1, the load-bearing `iss ≠
/// jwks_uri` nuance).
#[derive(Debug, Deserialize)]
struct RemoteJwksDoc {
    keys: Vec<Jwk>,
}

/// Live, atomically-swappable handle to the [`JwksStore`] — the **authn
/// door**, made symmetric with [`crate::nucleon_map::SharedHabilitationMap`]
/// (the **authz door**).
///
/// # Why this exists (ADR-0023 MVP-A, federated bridge)
///
/// Federating with a peer instance (the Dave↔Casey↔`speck` north star)
/// is *two* host-side gestures, not one (ADR-0023 addendum, point 2 —
/// *« JWKS (authN) ≠ pin (authZ) — deux portes distinctes »*):
///
/// 1. **trust the peer's JWKS** — add a second issuer to this store (authn);
/// 2. **pin `(iss, sub) → noyau`** — add a line to the
///    [`crate::nucleon_map::HabilitationMap`] (authz).
///
/// Both deny-by-default. The pin door has been hot-reloadable on `SIGHUP`
/// since the Pierre-P2 reload primitive — staging a
/// binding never required a reboot. The JWKS door, however, was loaded
/// **once at boot** into a bare `Arc` and never refreshed, so onboarding a
/// federated peer's keys *did* require a reboot — which tears down every
/// in-flight tmux worker, the exact failure [`crate::reload`] exists to
/// avoid. That left the two doors asymmetric and broke the ADR-0023 D6
/// promise (*« réversibilité native … deux `rm` + SIGHUP »*) for the authn
/// half of a federation grant.
///
/// Wrapping the store in the same `arc-swap` handle the binding map already
/// uses closes that asymmetry: dropping a peer's `<iss>.json` under
/// `<state_dir>/security/jwks/` (or removing it) and sending one `SIGHUP`
/// now refreshes **both** doors atomically, with no reboot and no dropped
/// worker. Reads on the admission hot path stay lock-free (a single
/// `Guard` load); a reload is a single atomic pointer store.
///
/// This is deliberately *not* a new federation type — there is no
/// `enum LocalOrFederated`, no `bool external` (ADR-0023 garde-fou
/// structurel). A peer instance is just another issuer in the same store;
/// the federated case is the multi-issuer case, with zero special-casing.
#[derive(Clone, Debug)]
pub struct SharedJwksStore(Arc<ArcSwap<JwksStore>>);

impl SharedJwksStore {
    /// Wrap an initial store (typically [`JwksStore::load`] at boot).
    #[must_use]
    pub fn new(store: JwksStore) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(store)))
    }

    /// Load a consistent, lock-free snapshot of the live store. Hold the
    /// returned guard for the lifetime of any borrow taken from it
    /// (`get`, `audiences_for` return references into the snapshot). The
    /// guard derefs through `Arc<JwksStore>` to `&JwksStore`, so it drops
    /// straight into [`JwtVerifier::validate`].
    #[must_use]
    pub fn load(&self) -> Guard<Arc<JwksStore>> {
        self.0.load()
    }

    /// Atomically publish a new store. Readers holding an existing guard
    /// keep their old snapshot; subsequent [`Self::load`] calls observe
    /// the new one. The swap is a single store with no reader blocking —
    /// in-flight requests validating against the prior JWKS finish against
    /// it; the next request sees the refreshed key set.
    pub fn store(&self, store: JwksStore) {
        self.0.store(Arc::new(store));
    }
}

fn parse_alg(s: &str) -> Option<Algorithm> {
    match s {
        "RS256" => Some(Algorithm::RS256),
        "ES256" => Some(Algorithm::ES256),
        _ => None,
    }
}

fn decode_jwk(jwk: &Jwk) -> Option<DecodingKey> {
    match jwk.kty {
        KeyKind::Rsa => {
            let n = jwk.n.as_deref()?;
            let e = jwk.e.as_deref()?;
            DecodingKey::from_rsa_components(n, e).ok()
        }
        KeyKind::Ec => {
            // ES256 implies P-256 — refuse anything else parse-time.
            if jwk.crv.as_deref() != Some("P-256") {
                return None;
            }
            let x = jwk.x.as_deref()?;
            let y = jwk.y.as_deref()?;
            DecodingKey::from_ec_components(x, y).ok()
        }
    }
}

/// Validated claims surfaced to admission. The struct deliberately
/// drops the raw token so downstream callers cannot accidentally log
/// or echo it.
#[derive(Clone, Debug)]
pub struct ValidatedJwt {
    /// `iss` claim — pinned, matches a JWKS entry.
    pub iss: String,
    /// `sub` claim — the principal identifier.
    pub sub: String,
    /// `aud` claim — pinned to this RPP instance.
    pub aud: String,
    /// `jti` claim (V0 requires presence — see ADR-080 §6.2).
    pub jti: String,
    /// Lifetime in seconds (`exp - iat`).
    pub lifetime_sec: u64,
    /// `exp` claim — Unix epoch seconds at which the token expires.
    /// Surfaced for the `/v1/auth/me` whoami route
    /// so the tenant can read the absolute expiry without parsing the
    /// token themselves.
    pub exp: u64,
    /// Scopes carried by the token. Empty when absent. Encoded by the
    /// identity provider either as a `scopes` claim (JSON array) or a
    /// space-separated `scope` claim (RFC 8693 / OIDC convention).
    pub scopes: Vec<String>,
}

impl ValidatedJwt {
    /// Returns true if `wanted` is present in the validated token's
    /// scope set. Scope check is exact-match (no prefix semantics yet
    /// — V0 grid is flat).
    #[must_use]
    pub fn has_scope(&self, wanted: &str) -> bool {
        self.scopes.iter().any(|s| s == wanted)
    }
}

/// Raw claim shape carried by the JWT. Matches ADR-080 §6.2 envelope.
#[derive(Debug, Deserialize, Serialize)]
struct RawClaims {
    iss: String,
    sub: String,
    aud: AudClaim,
    iat: u64,
    exp: u64,
    #[serde(default)]
    jti: Option<String>,
    /// Reject `delegate_for` in V0/V1 — turing red-line.
    #[serde(default)]
    delegate_for: Option<String>,
    /// Scopes claim — accept either a JSON array (`scopes`) or a
    /// space-separated string (`scope`). Both shapes feed the same
    /// downstream `Vec<String>`. The cosmon `cs-oidc-mock` identity
    /// provider emits the array form; OIDC-spec providers emit the
    /// string form.
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug)]
enum AudClaim {
    One(String),
    Many(Vec<String>),
}

impl<'de> serde::Deserialize<'de> for AudClaim {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // RFC 7519 allows `aud` as string or array of strings.
        let v = serde_json::Value::deserialize(d)?;
        match v {
            serde_json::Value::String(s) => Ok(Self::One(s)),
            serde_json::Value::Array(arr) => {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    if let serde_json::Value::String(s) = item {
                        out.push(s);
                    } else {
                        return Err(serde::de::Error::custom("aud must be string or [string]"));
                    }
                }
                Ok(Self::Many(out))
            }
            _ => Err(serde::de::Error::custom("aud must be string or [string]")),
        }
    }
}

impl serde::Serialize for AudClaim {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::One(v) => v.serialize(s),
            Self::Many(v) => v.serialize(s),
        }
    }
}

/// Stateless JWT verifier — pure function over `(&JwksStore, &raw_token)`.
#[derive(Debug)]
pub struct JwtVerifier;

impl JwtVerifier {
    /// Validate a bearer token against the pinned JWKS and apply the
    /// posture's lifetime cap.
    ///
    /// # Errors
    ///
    /// Returns one of the identity-clause variants of
    /// [`RppRejectReason`] (`MissingAuthorization`, `MalformedJwt`,
    /// `UnsupportedAlg`, `SignatureInvalid`, `Expired`, `NotYetValid`,
    /// `AudienceMismatch`, `AmbiguousAudience`, `IssuerNotPinned`).
    pub fn validate(
        jwks: &JwksStore,
        token: &str,
        posture: Posture,
    ) -> Result<ValidatedJwt, RppRejectReason> {
        // Header parse (unverified) → decide alg + kid lookup.
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| RppRejectReason::MalformedJwt)?;
        if !ALG_WHITELIST.contains(&header.alg) {
            return Err(RppRejectReason::UnsupportedAlg(format!("{:?}", header.alg)));
        }
        // Reject jku / x5u — they smuggle key resolution past the
        // pinning we do at boot (turing G7, G8).
        if header.jku.is_some() || header.x5u.is_some() {
            return Err(RppRejectReason::SignatureInvalid);
        }
        let kid = header.kid.ok_or(RppRejectReason::SignatureInvalid)?;
        // Unverified peek at `iss` so we can pick the right JWKS bucket.
        // Parse only the payload — we do NOT trust this until the
        // signature passes.
        let unverified_iss = peek_iss(token).ok_or(RppRejectReason::MalformedJwt)?;
        let (key, registered_alg) = jwks
            .get(&unverified_iss, &kid)
            .ok_or(RppRejectReason::IssuerNotPinned)?;
        if registered_alg != header.alg {
            // Algorithm switching attack — header says ES256 but the
            // pinned key is RS256.
            return Err(RppRejectReason::SignatureInvalid);
        }
        let mut validation = Validation::new(header.alg);
        validation.set_required_spec_claims(&["exp", "iat", "iss", "sub"]);
        validation.set_issuer(std::slice::from_ref(&unverified_iss));
        validation.validate_exp = true;
        validation.validate_nbf = false;
        validation.leeway = 0;
        // Defer audience check to manual code after decode — we want
        // to set an explicit error reason rather than rely on the
        // crate's ErrorKind::InvalidAudience.
        validation.validate_aud = false;
        let token_data =
            jsonwebtoken::decode::<RawClaims>(token, key, &validation).map_err(|e| {
                use jsonwebtoken::errors::ErrorKind;
                match e.into_kind() {
                    ErrorKind::ExpiredSignature => RppRejectReason::Expired,
                    ErrorKind::ImmatureSignature => RppRejectReason::NotYetValid,
                    ErrorKind::InvalidIssuer => RppRejectReason::IssuerNotPinned,
                    _ => RppRejectReason::SignatureInvalid,
                }
            })?;
        let claims = token_data.claims;
        if claims.delegate_for.is_some() {
            // Reserved for V2+ (ADR-080 §5.2 / §6.2).
            return Err(RppRejectReason::SignatureInvalid);
        }
        let pinned_audiences = jwks
            .audiences_for(&claims.iss)
            .ok_or(RppRejectReason::IssuerNotPinned)?;
        let aud = match &claims.aud {
            AudClaim::One(s) => s.clone(),
            AudClaim::Many(v) => {
                // Fail-closed (Rp F3, delib-20260710-33b7): a multi-aud
                // token is admitted only when **exactly one** of its
                // audiences is pinned for this issuer. Zero matches is a
                // plain mismatch; two or more pinned matches make the
                // downstream isolation key `(iss, sub, aud)` ambiguous —
                // the old `.find()` picked the first token-listed match,
                // letting whoever ordered the `aud` array choose which
                // pinned audience (i.e. which privilege) the RS resolves
                // to. We reject the ambiguity instead of least-privilege
                // guessing: the RS must be able to name *the* audience it
                // was addressed as, not one of several.
                let mut matches = v
                    .iter()
                    .filter(|a| pinned_audiences.iter().any(|p| p == *a));
                let first = matches
                    .next()
                    .ok_or(RppRejectReason::AudienceMismatch)?
                    .clone();
                if matches.next().is_some() {
                    return Err(RppRejectReason::AmbiguousAudience);
                }
                first
            }
        };
        if !pinned_audiences.iter().any(|a| a == &aud) {
            return Err(RppRejectReason::AudienceMismatch);
        }
        let lifetime_sec = claims.exp.saturating_sub(claims.iat);
        let max = match posture {
            Posture::Active => ACTIVE_MAX_LIFETIME_SEC,
            Posture::Prepared => PREPARED_MAX_LIFETIME_SEC,
        };
        if lifetime_sec > max {
            // In `Prepared` we emit a warning but still admit (the
            // posture is dev-only). In `Active` we hard-reject.
            match posture {
                Posture::Active => return Err(RppRejectReason::Expired),
                Posture::Prepared => {
                    tracing::warn!(
                        lifetime_sec,
                        max,
                        "jwt lifetime exceeds posture cap (prepared posture: warn-only)"
                    );
                }
            }
        }
        // `jti` is optional per RFC 7519 / OIDC 1.0 and most standard
        // IdPs (Forgejo, Google, Auth0) do not emit it on id_tokens.
        // ADR-080 §6.2 reserves replay defence to a future jti store —
        // until that lands, accept absent jti and synthesise a stable
        // identifier from the token's signature segment so the audit
        // log keeps a per-token correlation key.
        let jti = claims.jti.unwrap_or_else(|| {
            let sig = token.rsplit('.').next().unwrap_or("");
            format!("synthetic-{}", &sig[..sig.len().min(32)])
        });
        let scopes = match (claims.scopes, claims.scope) {
            (Some(arr), _) => arr,
            (None, Some(s)) => s
                .split_whitespace()
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect(),
            (None, None) => Vec::new(),
        };
        Ok(ValidatedJwt {
            iss: claims.iss,
            sub: claims.sub,
            aud,
            jti,
            lifetime_sec,
            exp: claims.exp,
            scopes,
        })
    }
}

fn peek_iss(token: &str) -> Option<String> {
    use base64::Engine;
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("iss").and_then(|i| i.as_str()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    /// 2048-bit test RSA key (DO NOT REUSE — generated for tests only
    /// via `openssl genrsa`).
    const TEST_RSA_PRIVATE_PEM: &str = include_str!("../tests/fixtures/test_rsa_private.pem");
    const TEST_RSA_PUBLIC_PEM: &str = include_str!("../tests/fixtures/test_rsa_public.pem");

    fn jwks_from_test_key() -> JwksStore {
        let key = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        JwksStore::from_pem(
            "https://idp.test",
            "kid-1",
            Algorithm::RS256,
            key,
            vec!["cosmon-rpp-tenant-demo".to_owned()],
        )
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn build_token(iss: &str, sub: &str, aud: &str, exp_offset: i64, jti: Option<&str>) -> String {
        let now = now_secs();
        let claims = serde_json::json!({
            "iss": iss,
            "sub": sub,
            "aud": aud,
            "iat": now,
            "exp": now + exp_offset,
            "jti": jti.unwrap_or("tok-1"),
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("kid-1".into());
        let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes()).unwrap();
        encode(&header, &claims, &key).unwrap()
    }

    /// Build a token whose `aud` claim is a JSON **array** of strings —
    /// the RFC 7519 multi-audience shape that `AudClaim::Many` decodes.
    fn build_token_multi_aud(iss: &str, sub: &str, auds: &[&str], jti: &str) -> String {
        let now = now_secs();
        let claims = serde_json::json!({
            "iss": iss,
            "sub": sub,
            "aud": auds,
            "iat": now,
            "exp": now + 60,
            "jti": jti,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("kid-1".into());
        let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes()).unwrap();
        encode(&header, &claims, &key).unwrap()
    }

    #[test]
    fn admits_valid_token() {
        let jwks = jwks_from_test_key();
        let token = build_token(
            "https://idp.test",
            "sub-1",
            "cosmon-rpp-tenant-demo",
            60,
            Some("tok-1"),
        );
        let v = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap();
        assert_eq!(v.iss, "https://idp.test");
        assert_eq!(v.sub, "sub-1");
        assert_eq!(v.aud, "cosmon-rpp-tenant-demo");
        assert_eq!(v.jti, "tok-1");
    }

    #[test]
    fn rejects_alg_none() {
        use base64::Engine;
        let jwks = jwks_from_test_key();
        // Hand-craft an "alg=none" token. `jsonwebtoken` does not
        // expose `none`, so we build the raw concatenation directly.
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","kid":"kid-1"}"#);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"iss":"https://idp.test","sub":"sub-1","aud":"cosmon-rpp-tenant-demo","iat":0,"exp":9999999999,"jti":"tok-1"}"#);
        let token = format!("{header}.{claims}.");
        let err = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap_err();
        assert!(matches!(
            err,
            RppRejectReason::MalformedJwt | RppRejectReason::UnsupportedAlg(_)
        ));
    }

    #[test]
    fn rejects_unknown_issuer() {
        let jwks = jwks_from_test_key();
        let token = build_token(
            "https://other-idp",
            "sub-1",
            "cosmon-rpp-tenant-demo",
            60,
            Some("tok-1"),
        );
        let err = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap_err();
        assert!(matches!(err, RppRejectReason::IssuerNotPinned));
    }

    #[test]
    fn rejects_audience_mismatch() {
        let jwks = jwks_from_test_key();
        let token = build_token(
            "https://idp.test",
            "sub-1",
            "cosmon-rpp-other",
            60,
            Some("tok-1"),
        );
        let err = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap_err();
        assert!(matches!(err, RppRejectReason::AudienceMismatch));
    }

    /// The negative-audience test for the two provisioned OAuth apps
    /// (kahneman-F5, delib-20260710-33b7 §C1/§C8): the RS-side audience
    /// wall is a **closed allowlist**, so a token minted for app A
    /// (`cs-rpp-adapter`) is rejected by a resource pinned to app B
    /// (`claude-web`) only, and vice versa. Isolation is proved by
    /// *rejection*, which a single-audience deployment cannot demonstrate
    /// by accident. This is the server twin of the client-side store
    /// audience-isolation key `(iss, sub, aud)`.
    #[test]
    fn rejects_cross_app_audience_a_vs_b() {
        let key = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();

        // Resource pinned to B only. A token for A must not open it.
        let jwks_b = JwksStore::from_pem(
            "https://idp.test",
            "kid-1",
            Algorithm::RS256,
            key.clone(),
            vec!["claude-web".to_owned()],
        );
        let token_a = build_token(
            "https://idp.test",
            "sub-1",
            "cs-rpp-adapter",
            60,
            Some("t-a"),
        );
        let err = JwtVerifier::validate(&jwks_b, &token_a, Posture::Prepared).unwrap_err();
        assert!(
            matches!(err, RppRejectReason::AudienceMismatch),
            "aud=cs-rpp-adapter (A) must be rejected by a claude-web (B)-only resource"
        );

        // Symmetric: a B token must not open an A-only resource.
        let jwks_a = JwksStore::from_pem(
            "https://idp.test",
            "kid-1",
            Algorithm::RS256,
            key,
            vec!["cs-rpp-adapter".to_owned()],
        );
        let token_b = build_token("https://idp.test", "sub-1", "claude-web", 60, Some("t-b"));
        let err = JwtVerifier::validate(&jwks_a, &token_b, Posture::Prepared).unwrap_err();
        assert!(matches!(err, RppRejectReason::AudienceMismatch));

        // And the token whose aud IS pinned is admitted (no wildcard, but
        // no false-negative either).
        let token_a_ok = build_token(
            "https://idp.test",
            "sub-1",
            "cs-rpp-adapter",
            60,
            Some("t-ok"),
        );
        let v = JwtVerifier::validate(&jwks_a, &token_a_ok, Posture::Prepared).unwrap();
        assert_eq!(v.aud, "cs-rpp-adapter");
    }

    /// Rp F3 — a multi-aud token that lists exactly ONE pinned audience
    /// (alongside unrelated ones the RS does not recognise) is admitted,
    /// and the resolved `aud` is that single pinned value. This is the
    /// legitimate RFC 7519 case where the same token is minted for
    /// several resources and only one of them is *this* RS.
    #[test]
    fn admits_multi_aud_with_single_pinned_match() {
        let jwks = jwks_from_test_key(); // pins "cosmon-rpp-tenant-demo"
        let token = build_token_multi_aud(
            "https://idp.test",
            "sub-1",
            &["some-other-resource", "cosmon-rpp-tenant-demo", "third"],
            "tok-multi-ok",
        );
        let v = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap();
        assert_eq!(
            v.aud, "cosmon-rpp-tenant-demo",
            "the one pinned audience must be the resolved aud"
        );
    }

    /// Rp F3 — a multi-aud token with **zero** pinned audiences is a plain
    /// mismatch (unchanged behaviour, now covered for the array shape).
    #[test]
    fn rejects_multi_aud_with_no_pinned_match() {
        let jwks = jwks_from_test_key();
        let token = build_token_multi_aud(
            "https://idp.test",
            "sub-1",
            &["nope-a", "nope-b"],
            "tok-multi-none",
        );
        let err = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap_err();
        assert!(matches!(err, RppRejectReason::AudienceMismatch));
    }

    /// Rp F3 — the core fix. A multi-aud token where **two or more**
    /// audiences are pinned for this issuer is *ambiguous*: the old
    /// `.find()` silently resolved to whichever pinned audience appeared
    /// first in the array, handing audience selection (and thus the
    /// `(iss, sub, aud)` isolation key / privilege) to the token minter.
    /// Fail-closed: reject with the dedicated `AmbiguousAudience` reason.
    #[test]
    fn rejects_ambiguous_multi_aud_with_two_pinned_matches() {
        let key = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        // Resource pins BOTH app audiences (A and B) for this issuer.
        let jwks = JwksStore::from_pem(
            "https://idp.test",
            "kid-1",
            Algorithm::RS256,
            key,
            vec!["cs-rpp-adapter".to_owned(), "claude-web".to_owned()],
        );
        // Token carries both — ambiguous which one this request is for.
        let token = build_token_multi_aud(
            "https://idp.test",
            "sub-1",
            &["cs-rpp-adapter", "claude-web"],
            "tok-ambiguous",
        );
        let err = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap_err();
        assert!(
            matches!(err, RppRejectReason::AmbiguousAudience),
            "two pinned audiences in one token must fail-closed, got {err:?}"
        );

        // Order independence: reversing the array must not flip the
        // decision (the old .find() was order-sensitive).
        let token_rev = build_token_multi_aud(
            "https://idp.test",
            "sub-1",
            &["claude-web", "cs-rpp-adapter"],
            "tok-ambiguous-rev",
        );
        let err_rev = JwtVerifier::validate(&jwks, &token_rev, Posture::Prepared).unwrap_err();
        assert!(matches!(err_rev, RppRejectReason::AmbiguousAudience));
    }

    #[test]
    fn rejects_expired_token() {
        let jwks = jwks_from_test_key();
        let token = build_token(
            "https://idp.test",
            "sub-1",
            "cosmon-rpp-tenant-demo",
            -10,
            Some("tok-1"),
        );
        let err = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap_err();
        assert!(matches!(err, RppRejectReason::Expired));
    }

    #[test]
    fn active_posture_caps_lifetime_at_15min() {
        let jwks = jwks_from_test_key();
        // 30 minutes — over the 15-min `Active` cap.
        let token = build_token(
            "https://idp.test",
            "sub-1",
            "cosmon-rpp-tenant-demo",
            30 * 60,
            Some("tok-1"),
        );
        let err = JwtVerifier::validate(&jwks, &token, Posture::Active).unwrap_err();
        assert!(matches!(err, RppRejectReason::Expired));
    }

    #[test]
    fn prepared_posture_warns_but_admits_long_token() {
        let jwks = jwks_from_test_key();
        let token = build_token(
            "https://idp.test",
            "sub-1",
            "cosmon-rpp-tenant-demo",
            60 * 60,
            Some("tok-1"),
        );
        // 1 hour < 24 h prepared cap, so admits.
        let v = JwtVerifier::validate(&jwks, &token, Posture::Prepared).unwrap();
        assert_eq!(v.sub, "sub-1");
    }

    // ── Multi-issuer / federated bridge (ADR-0023 MVP-A) ───────────────
    //
    // The peer instance (Dave's, in the north-star scenario) is just a
    // second issuer in the same store. These tests reuse the embedded
    // test key for both issuers — they prove the `(iss, kid)` *routing*
    // and the authn-door hot-swap, not cryptographic key separation
    // (which the production loader gets for free: each issuer ships its
    // own JWKS file). The federated full-stack proof lives in
    // `tests/federation_bridge.rs`.

    const PEER_ISS: &str = "https://dave.instance.peer";

    fn jwks_local_and_peer() -> JwksStore {
        let local = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        let peer = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        JwksStore::from_pem(
            "https://idp.test",
            "kid-1",
            Algorithm::RS256,
            local,
            vec!["cosmon-rpp-tenant-demo".to_owned()],
        )
        .with_pem(
            PEER_ISS,
            "kid-1",
            Algorithm::RS256,
            peer,
            vec!["cosmon-rpp-speck".to_owned()],
        )
    }

    #[test]
    fn multi_issuer_validates_local_and_federated_peer() {
        let jwks = jwks_local_and_peer();
        let local = build_token(
            "https://idp.test",
            "casey",
            "cosmon-rpp-tenant-demo",
            60,
            Some("t1"),
        );
        assert_eq!(
            JwtVerifier::validate(&jwks, &local, Posture::Prepared)
                .unwrap()
                .iss,
            "https://idp.test"
        );
        let peer = build_token(PEER_ISS, "dave", "cosmon-rpp-speck", 60, Some("t2"));
        let v = JwtVerifier::validate(&jwks, &peer, Posture::Prepared).unwrap();
        assert_eq!(v.iss, PEER_ISS);
        assert_eq!(v.sub, "dave");
    }

    #[test]
    fn federated_peer_rejected_when_its_jwks_absent() {
        // JWKS(authn) ≠ pin(authz): without the peer's JWKS, the peer
        // token is not even authenticated — IssuerNotPinned, before any
        // authz decision is reached.
        let jwks = jwks_from_test_key(); // local issuer only
        let peer = build_token(PEER_ISS, "dave", "cosmon-rpp-speck", 60, Some("t2"));
        let err = JwtVerifier::validate(&jwks, &peer, Posture::Prepared).unwrap_err();
        assert!(matches!(err, RppRejectReason::IssuerNotPinned));
    }

    #[test]
    fn shared_jwks_store_hot_adds_federated_issuer() {
        // The authn door is hot-swappable: a peer token fails before the
        // peer's JWKS is published, then validates after the atomic swap
        // — no reboot (ADR-0023 MVP-A, D6). This is the in-memory mirror
        // of `cp <iss>.json security/jwks/ && kill -HUP`.
        let shared = SharedJwksStore::new(jwks_from_test_key());
        let peer = build_token(PEER_ISS, "dave", "cosmon-rpp-speck", 60, Some("t2"));
        assert!(matches!(
            JwtVerifier::validate(&shared.load(), &peer, Posture::Prepared).unwrap_err(),
            RppRejectReason::IssuerNotPinned
        ));
        // Operator stages the peer's JWKS and SIGHUPs → store swapped.
        shared.store(jwks_local_and_peer());
        let v = JwtVerifier::validate(&shared.load(), &peer, Posture::Prepared).unwrap();
        assert_eq!(v.iss, PEER_ISS);
        assert_eq!(v.sub, "dave");
    }

    #[test]
    fn shared_jwks_store_revokes_issuer_on_swap() {
        // The mirror gesture (D6): swapping back to a store WITHOUT the
        // peer revokes its authentication. `rm <iss>.json && kill -HUP`.
        let shared = SharedJwksStore::new(jwks_local_and_peer());
        let peer = build_token(PEER_ISS, "dave", "cosmon-rpp-speck", 60, Some("t2"));
        assert!(JwtVerifier::validate(&shared.load(), &peer, Posture::Prepared).is_ok());
        shared.store(jwks_from_test_key()); // peer removed
        assert!(matches!(
            JwtVerifier::validate(&shared.load(), &peer, Posture::Prepared).unwrap_err(),
            RppRejectReason::IssuerNotPinned
        ));
    }
}
