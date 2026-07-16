// SPDX-License-Identifier: AGPL-3.0-only

//! Authentication primitives for the cosmon authorisation boundary.
//!
//! This module defines the **domain types** every cosmon admission point
//! eventually consumes — the `Subject` (who is acting, with what scopes),
//! the `Scope` newtype (a kebab-case capability label), the minimal
//! `JwtClaims` decoding surface, and the `AuthError` enum.
//!
//! # Why this lives in `cosmon-core`
//!
//! `cosmon-core` is the I/O-free domain crate. Putting `Subject` here
//! means `cosmon_state::ops::<verb>` can take `&Subject` in its
//! signature without dragging an HTTP framework, a JWT validator, or a
//! TLS stack into the build graph. The two transports that mint
//! Subjects — the operator-side CLI (`cs`) and the future RPP HTTPS
//! adapter (`cosmon-rpp-adapter`) — depend on `cosmon-core`, never the
//! reverse.
//!
//! # Constructors
//!
//! Three constructors are exposed today:
//!
//! * [`Subject::operator`] — used by every existing call-site in
//!   `cosmon-cli` and the workspace tests. Returns a Subject with the
//!   wildcard scope, modelling the unrestricted operator who runs
//!   commands locally on disk. The `NucleonId` is fixed to
//!   `"operator"`; once `cs-cli` reads operator identity from
//!   `.cosmon/identity.toml` (out of scope for this task), it will
//!   call a future `Subject::operator_with_id` constructor instead of
//!   inlining its own struct literal.
//! * [`Subject::from_jwt_claims`] — used by the future RPP adapter
//!   when an HTTPS request lands with an OIDC bearer token. It
//!   converts the *already-validated* claims (signature checked, `aud`
//!   / `iss` / `exp` checked by the adapter) into a domain `Subject`,
//!   enforcing only the cosmon-side invariants: non-empty subject,
//!   kebab-case scopes.
//! * [`Subject::builder`] — sealed forward-compatible constructor.
//!   Takes a [`TenantId`] (the principal's tenant) and yields a
//!   [`SubjectBuilder`] onto which scopes are layered. This is the
//!   path V1+ call-sites should reach for; existing constructors stay
//!   for backwards compatibility.
//!
//! # `#[non_exhaustive]` and the V1 BYOK roadmap
//!
//! [`Subject`] is `#[non_exhaustive]` and its fields are private. New
//! fields can therefore be added in a *minor* bump of `cosmon-core`
//! without breaking dependents. The V1 add will be a
//! `byok: Option<TenantApiKey>` — the bring-your-own-key field that
//! makes per-tenant LLM credentials reachable inside the runtime.
//! Until V1 lands, [`TenantApiKey`] exists as a typed slot so the
//! `LlmBackend` trait surface can already speak it. The strategic
//! credentials/billing roadmap that motivates this slot is tracked in the
//! ADR series under `docs/adr/`.
//!
//! # Downstream consumers
//!
//! `Subject` is the input type that `cosmon_state::ops::<verb>`
//! signatures will adopt starting at task **T3 — extract tag**, and
//! every subsequent `ops::*` verb will follow. `Scope` is the unit of
//! authorisation reasoning; the actual permission predicates
//! (`Subject::has_scope`, `Subject::can_observe`, …) are intentionally
//! *not* implemented in this task — see the ADR series under
//! `docs/adr/` for the RBAC plane proposal. This module ships the
//! brick; the wall comes later.
//!
//! # Non-goals
//!
//! * No token signing or signature verification — the adapter that
//!   speaks HTTPS owns that layer.
//! * No transport-shaped types — no `axum`, no `jsonwebtoken`, no
//!   `serde_json::Value` smuggling. `JwtClaims` is the bare wire-shape
//!   the adapter hands us, nothing more.
//! * No filesystem or environment lookup — `Subject::operator` is a
//!   pure constructor.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::id::{IdError, NucleonId};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors raised when constructing or decoding authentication primitives.
///
/// All variants are domain-level: they describe what went wrong with a
/// `Scope` string, a JWT claim shape, or a Nucléon identifier *after*
/// the transport layer has already accepted the request. Transport
/// failures (TLS handshake, signature verification, connection reset)
/// never surface here.
///
/// `#[non_exhaustive]` — V1 adds a `ByokRequired` variant for the
/// backend-bound bring-your-own-key flow; a minor bump must remain
/// non-breaking for downstream `match` sites.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// The provided scope string failed kebab-case validation.
    #[error("invalid scope \"{scope}\": {reason}")]
    InvalidScope {
        /// The offending scope string, verbatim.
        scope: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The JWT `sub` claim was empty or otherwise rejected by [`NucleonId`].
    #[error("invalid JWT subject: {0}")]
    InvalidSubject(#[from] IdError),

    /// The JWT `sub` claim was missing or empty.
    #[error("JWT claim `sub` is required and must be non-empty")]
    MissingSubject,

    /// The provided tenant identifier failed validation (empty).
    #[error("invalid tenant id: {0}")]
    InvalidTenant(String),
}

// ---------------------------------------------------------------------------
// TenantId
// ---------------------------------------------------------------------------

/// The tenant boundary identifier — V0 mono-tenant, V1+ multi-tenant.
///
/// Today the tenant is simply the principal acting on the system. In
/// V1+ (multi-tenant `SaaS`, BYOK billing) a tenant becomes a billable
/// account that may carry several Nucléons (sessions, devices). The
/// type already exists at the API surface so [`Subject::builder`] can
/// take it as input — V0 builds a `Subject` whose `id` is derived from
/// the `TenantId`'s string, V1 will add a `byok` slot keyed on the
/// same tenant.
///
/// Validation is intentionally minimal: non-empty string. The richer
/// shape (UUID, domain-bound, …) lives at the admission boundary, not
/// in this domain newtype — same discipline as [`NucleonId`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// Construct a `TenantId`, rejecting empty strings.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidTenant`] when the input is empty.
    pub fn new(s: impl Into<String>) -> Result<Self, AuthError> {
        let s = s.into();
        if s.is_empty() {
            return Err(AuthError::InvalidTenant(
                "tenant id must be non-empty".to_string(),
            ));
        }
        Ok(Self(s))
    }

    /// Inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for TenantId {
    type Error = AuthError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<TenantId> for String {
    fn from(t: TenantId) -> Self {
        t.0
    }
}

// ---------------------------------------------------------------------------
// TenantApiKey — BYOK slot for V1+
// ---------------------------------------------------------------------------

/// Per-tenant LLM credential — opaque, redacted, **never** serialised.
///
/// V0 prepares the type so [`crate::llm::LlmBackend`] can already speak
/// it; V1 wires it into [`Subject`] (the future `byok` slot) and the
/// `TenantContext` consumed by `LlmBackend::complete`.
///
/// # Why no `Serialize`
///
/// Deliberately omitted. Round-tripping a tenant key through JSON is a
/// foot-gun: every accidental log line, every `serde_json::to_string`
/// of the surrounding struct would leak the secret. Exposing the key
/// is an *explicit* gesture — the caller pulls
/// [`TenantApiKey::expose_secret`] when they truly need the bytes,
/// nowhere else.
///
/// # Debug discipline
///
/// The `Debug` impl prints `TenantApiKey(***)` and never the inner
/// bytes — same redaction discipline `secrecy::SecretString` enforces
/// on its own `Debug`, hoisted up so the wrapping type cannot leak
/// either.
#[derive(Clone)]
pub struct TenantApiKey(secrecy::SecretString);

impl TenantApiKey {
    /// Construct a `TenantApiKey` from an owned string.
    ///
    /// The input is moved into a `secrecy::SecretString`, which zeroes
    /// the underlying buffer on drop. Callers must not retain the raw
    /// `String` after this call.
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self(secrecy::SecretString::new(secret))
    }

    /// Expose the inner secret bytes — explicit gesture, no fallback.
    ///
    /// This is the only path to the underlying string. The caller is
    /// responsible for not logging, persisting, or transmitting the
    /// returned slice beyond the immediate use site.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        use secrecy::ExposeSecret;
        self.0.expose_secret()
    }
}

impl fmt::Debug for TenantApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TenantApiKey(***)")
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// A capability label — the unit of authorisation reasoning.
///
/// A `Scope` is a kebab-case string such as `"molecule-observe"` or
/// `"tag-write"`. The newtype wrapper is the structural anti-leak: any
/// `cosmon_state::ops::<verb>` that needs to check authorisation must
/// take a `&Scope`, never a `&str`, so that a free-form string from the
/// network can never silently slip past the boundary without
/// validation.
///
/// The wildcard scope `"*"` (constructed via [`Scope::wildcard`]) is
/// the only scope that bypasses kebab-case validation. It is the
/// representation of "all permissions" and is what
/// [`Subject::operator`] holds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Scope(String);

impl Scope {
    /// Construct a `Scope` from a string, validating kebab-case shape.
    ///
    /// # Validation
    ///
    /// * Non-empty.
    /// * ASCII lowercase letters, ASCII digits, and `-` only.
    /// * Must not start or end with `-`.
    /// * Must not contain two consecutive `-`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidScope`] on any rule violation.
    pub fn new(s: impl Into<String>) -> Result<Self, AuthError> {
        let s = s.into();
        validate_kebab(&s)?;
        Ok(Self(s))
    }

    /// The wildcard scope, representing "all permissions".
    ///
    /// This is the only scope whose construction bypasses kebab-case
    /// validation. It is intentionally not parseable from JWT claims —
    /// no remote caller can ask for `*` simply by writing it in a
    /// token. It is reachable only through this constructor, which is
    /// in turn called only by [`Subject::operator`].
    #[must_use]
    pub fn wildcard() -> Self {
        Self("*".to_string())
    }

    /// Returns `true` if this scope is the wildcard ("all permissions").
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.0 == "*"
    }

    /// Inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Scope {
    type Error = AuthError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<Scope> for String {
    fn from(s: Scope) -> Self {
        s.0
    }
}

fn validate_kebab(s: &str) -> Result<(), AuthError> {
    if s.is_empty() {
        return Err(AuthError::InvalidScope {
            scope: s.to_string(),
            reason: "scope must be non-empty".to_string(),
        });
    }

    let bytes = s.as_bytes();
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return Err(AuthError::InvalidScope {
            scope: s.to_string(),
            reason: "scope must not start or end with '-'".to_string(),
        });
    }

    let mut prev_hyphen = false;
    for c in s.chars() {
        let ok = c == '-' || c.is_ascii_digit() || (c.is_ascii_lowercase());
        if !ok {
            return Err(AuthError::InvalidScope {
                scope: s.to_string(),
                reason: format!(
                    "scope must contain only ASCII lowercase, digits, or '-'; got '{c}'"
                ),
            });
        }
        if c == '-' && prev_hyphen {
            return Err(AuthError::InvalidScope {
                scope: s.to_string(),
                reason: "scope must not contain consecutive '-'".to_string(),
            });
        }
        prev_hyphen = c == '-';
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JwtClaims
// ---------------------------------------------------------------------------

/// Minimal JWT claim shape consumed by [`Subject::from_jwt_claims`].
///
/// This struct exists so `cosmon-core` can reason about an OIDC token
/// without taking a dependency on `jsonwebtoken`, `axum`, or any
/// transport. The RPP adapter will own signature verification, claim
/// validation (`exp`, `aud`, `iss`), and the mapping from its
/// concrete claim type to this bare-bones shape.
///
/// Only two fields are modelled:
///
/// * `sub` — the JWT subject, which becomes the [`NucleonId`]. The
///   adapter is responsible for ensuring this is the *stable* Nucléon
///   handle (e.g. `you`, not an opaque opaque OIDC `sub` UUID); the
///   mapping policy lives there.
/// * `scopes` — flat list of scope strings as they appeared in the
///   token's `scope` claim (or `scp`, depending on the `IdP`). Each
///   string is validated through [`Scope::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwtClaims {
    /// JWT `sub` claim — must map to a non-empty [`NucleonId`].
    pub sub: String,
    /// Flat list of capability scope strings.
    pub scopes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Subject — sealed, builder-constructed
// ---------------------------------------------------------------------------

/// The acting Nucléon for a cosmon operation.
///
/// Every authorisation check at the `cosmon_state::ops::*` boundary
/// will eventually consume `&Subject`. The struct is **sealed**:
///
/// * `#[non_exhaustive]` so V1 may add a `byok: Option<TenantApiKey>`
///   field in a minor bump without breaking pattern matches in
///   downstream crates.
/// * Fields are *private* — read them through [`Subject::id`] and
///   [`Subject::scopes`] accessors. Construction outside this crate
///   goes through [`Subject::operator`], [`Subject::from_jwt_claims`],
///   or [`Subject::builder`].
///
/// # Invariants
///
/// * `id` is a non-empty [`NucleonId`].
/// * Each entry in `scopes` is either a kebab-case [`Scope`] or the
///   distinguished wildcard scope (constructed only by
///   [`Scope::wildcard`]).
/// * The wildcard scope is reachable only via [`Subject::operator`]
///   or by direct call of [`SubjectBuilder::scope`] with
///   [`Scope::wildcard`] — no remote JWT-authenticated caller can
///   self-elevate by crafting `*` in their token.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    /// The stable Nucléon handle that authored this action.
    id: NucleonId,
    /// Capability scopes carried by this subject.
    scopes: Vec<Scope>,
}

impl Subject {
    /// The unrestricted operator subject — used by `cs-cli` and tests.
    ///
    /// The CLI has full filesystem access to `.cosmon/state/` and is
    /// trusted to perform any operation. Modelling it as a `Subject`
    /// rather than a magic bypass keeps the authorisation surface
    /// uniform: every `ops::<verb>` takes the same shape regardless of
    /// caller. The wildcard scope is the inert representation of "yes
    /// to everything"; the actual permission check lives in the
    /// (forthcoming) `Subject::has_scope`-family predicates.
    ///
    /// The `NucleonId` returned here is the fixed string `"operator"`.
    /// Once `cs-cli` reads operator identity from
    /// `.cosmon/identity.toml`, a sibling constructor
    /// `operator_with_id(id)` will be added — keeping this no-arg
    /// form for tests and trivial call-sites.
    ///
    /// # Panics
    ///
    /// Infallible in practice: the static literal `"operator"` is
    /// non-empty, which is the only validation `NucleonId::new`
    /// performs. The `expect` therefore documents an unreachable
    /// branch — kept rather than `unwrap` to surface a clear message
    /// if the macro contract ever changes.
    #[must_use]
    pub fn operator() -> Self {
        let id = NucleonId::new("operator").expect("static literal \"operator\" is non-empty");
        Self {
            id,
            scopes: vec![Scope::wildcard()],
        }
    }

    /// Build a `Subject` from already-validated JWT claims.
    ///
    /// The caller (RPP adapter) has verified the signature and the
    /// standard claims (`iss`, `aud`, `exp`, `nbf`). This constructor
    /// enforces only the *domain* invariants: the `sub` is parseable
    /// as a [`NucleonId`], and every scope passes kebab-case
    /// validation.
    ///
    /// # Errors
    ///
    /// * [`AuthError::MissingSubject`] if `claims.sub` is empty.
    /// * [`AuthError::InvalidSubject`] if `claims.sub` fails
    ///   `NucleonId` validation.
    /// * [`AuthError::InvalidScope`] on any malformed scope string.
    pub fn from_jwt_claims(claims: &JwtClaims) -> Result<Self, AuthError> {
        if claims.sub.is_empty() {
            return Err(AuthError::MissingSubject);
        }
        let id = NucleonId::new(claims.sub.clone())?;
        let scopes = claims
            .scopes
            .iter()
            .map(|s| Scope::new(s.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { id, scopes })
    }

    /// Forward-compatible builder for [`Subject`].
    ///
    /// Takes a [`TenantId`] (the principal's tenant) and yields a
    /// [`SubjectBuilder`]. In V0 the tenant string is also the
    /// Nucléon handle (mono-tenant); V1 will decouple them by adding
    /// a `byok: Option<TenantApiKey>` slot keyed on the tenant.
    ///
    /// This is the entry point V1+ call-sites should reach for. The
    /// existing [`Subject::operator`] and [`Subject::from_jwt_claims`]
    /// remain canonical for their respective transports.
    ///
    /// # Panics
    ///
    /// Infallible in practice: a `TenantId` is non-empty by
    /// construction, which is the only validation `NucleonId::new`
    /// performs. The `expect` documents an unreachable branch.
    #[must_use]
    pub fn builder(tenant: TenantId) -> SubjectBuilder {
        // Move the tenant string into the NucleonId rather than
        // borrowing it — keeps the signature owned (clearer
        // builder-flow semantics) and avoids a needless clone.
        let id =
            NucleonId::new(String::from(tenant)).expect("TenantId is non-empty by construction");
        SubjectBuilder {
            id,
            scopes: Vec::new(),
        }
    }

    /// The stable Nucléon handle that authored this action.
    #[must_use]
    pub fn id(&self) -> &NucleonId {
        &self.id
    }

    /// Capability scopes carried by this subject.
    #[must_use]
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }
}

/// Builder for [`Subject`] — see [`Subject::builder`].
///
/// Layered constructor: start with a [`TenantId`], add scopes one at
/// a time, finalise with [`SubjectBuilder::build`]. The builder is
/// itself `#[non_exhaustive]` — V1 will add `byok(TenantApiKey)`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct SubjectBuilder {
    id: NucleonId,
    scopes: Vec<Scope>,
}

impl SubjectBuilder {
    /// Add a single capability scope.
    #[must_use]
    pub fn scope(mut self, s: Scope) -> Self {
        self.scopes.push(s);
        self
    }

    /// Add several scopes at once.
    #[must_use]
    pub fn scopes<I: IntoIterator<Item = Scope>>(mut self, iter: I) -> Self {
        self.scopes.extend(iter);
        self
    }

    /// Finalise into a [`Subject`].
    #[must_use]
    pub fn build(self) -> Subject {
        Subject {
            id: self.id,
            scopes: self.scopes,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Scope --

    #[test]
    fn scope_accepts_kebab_case() {
        let s = Scope::new("molecule-observe").unwrap();
        assert_eq!(s.as_str(), "molecule-observe");
        assert!(!s.is_wildcard());
    }

    #[test]
    fn scope_accepts_single_word() {
        assert!(Scope::new("observe").is_ok());
    }

    #[test]
    fn scope_accepts_digits() {
        assert!(Scope::new("ops-v2").is_ok());
    }

    #[test]
    fn scope_rejects_empty() {
        assert!(matches!(
            Scope::new(""),
            Err(AuthError::InvalidScope { .. })
        ));
    }

    #[test]
    fn scope_rejects_uppercase() {
        assert!(Scope::new("Molecule-Observe").is_err());
    }

    #[test]
    fn scope_rejects_underscore() {
        assert!(Scope::new("molecule_observe").is_err());
    }

    #[test]
    fn scope_rejects_leading_hyphen() {
        assert!(Scope::new("-observe").is_err());
    }

    #[test]
    fn scope_rejects_trailing_hyphen() {
        assert!(Scope::new("observe-").is_err());
    }

    #[test]
    fn scope_rejects_double_hyphen() {
        assert!(Scope::new("molecule--observe").is_err());
    }

    #[test]
    fn scope_rejects_wildcard_via_new() {
        // `*` must NOT be reachable through normal validation —
        // only Scope::wildcard() may produce it.
        assert!(Scope::new("*").is_err());
    }

    #[test]
    fn scope_wildcard_is_distinguished() {
        let w = Scope::wildcard();
        assert!(w.is_wildcard());
        assert_eq!(w.as_str(), "*");
    }

    #[test]
    fn scope_serde_roundtrip() {
        let s = Scope::new("tag-write").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"tag-write\"");
        let back: Scope = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // -- TenantId --

    #[test]
    fn tenant_id_rejects_empty() {
        assert!(matches!(
            TenantId::new(""),
            Err(AuthError::InvalidTenant(_))
        ));
    }

    #[test]
    fn tenant_id_accepts_simple_string() {
        let t = TenantId::new("you").unwrap();
        assert_eq!(t.as_str(), "you");
    }

    #[test]
    fn tenant_id_serde_roundtrip() {
        let t = TenantId::new("tenant-demo").unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let back: TenantId = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    // -- TenantApiKey --

    #[test]
    fn tenant_api_key_redacts_in_debug() {
        let k = TenantApiKey::new("sk-supersecret".to_string());
        let s = format!("{k:?}");
        assert!(!s.contains("sk-supersecret"));
        assert!(s.contains("***"));
    }

    #[test]
    fn tenant_api_key_exposes_only_on_request() {
        let k = TenantApiKey::new("sk-real".to_string());
        assert_eq!(k.expose_secret(), "sk-real");
    }

    // -- Subject::operator --

    #[test]
    fn operator_has_wildcard_scope() {
        let s = Subject::operator();
        assert_eq!(s.id().as_str(), "operator");
        assert_eq!(s.scopes().len(), 1);
        assert!(s.scopes()[0].is_wildcard());
    }

    // -- Subject::builder --

    #[test]
    fn builder_constructs_subject_with_no_scopes() {
        let s = Subject::builder(TenantId::new("tenant-demo").unwrap()).build();
        assert_eq!(s.id().as_str(), "tenant-demo");
        assert!(s.scopes().is_empty());
    }

    #[test]
    fn builder_layers_scopes() {
        let s = Subject::builder(TenantId::new("tenant-demo").unwrap())
            .scope(Scope::new("molecule-observe").unwrap())
            .scope(Scope::new("tag-write").unwrap())
            .build();
        assert_eq!(s.scopes().len(), 2);
        assert_eq!(s.scopes()[0].as_str(), "molecule-observe");
        assert_eq!(s.scopes()[1].as_str(), "tag-write");
    }

    #[test]
    fn builder_scopes_iterator() {
        let s = Subject::builder(TenantId::new("tenant-demo").unwrap())
            .scopes([
                Scope::new("a").unwrap(),
                Scope::new("b").unwrap(),
                Scope::new("c").unwrap(),
            ])
            .build();
        assert_eq!(s.scopes().len(), 3);
    }

    // -- Subject::from_jwt_claims --

    #[test]
    fn from_jwt_claims_accepts_valid() {
        let claims = JwtClaims {
            sub: "you".to_string(),
            scopes: vec!["molecule-observe".to_string(), "tag-write".to_string()],
        };
        let s = Subject::from_jwt_claims(&claims).unwrap();
        assert_eq!(s.id().as_str(), "you");
        assert_eq!(s.scopes().len(), 2);
        assert_eq!(s.scopes()[0].as_str(), "molecule-observe");
        assert_eq!(s.scopes()[1].as_str(), "tag-write");
        assert!(!s.scopes()[0].is_wildcard());
    }

    #[test]
    fn from_jwt_claims_accepts_no_scopes() {
        let claims = JwtClaims {
            sub: "you".to_string(),
            scopes: vec![],
        };
        let s = Subject::from_jwt_claims(&claims).unwrap();
        assert!(s.scopes().is_empty());
    }

    #[test]
    fn from_jwt_claims_rejects_empty_sub() {
        let claims = JwtClaims {
            sub: String::new(),
            scopes: vec![],
        };
        assert!(matches!(
            Subject::from_jwt_claims(&claims),
            Err(AuthError::MissingSubject)
        ));
    }

    #[test]
    fn from_jwt_claims_rejects_invalid_scope() {
        let claims = JwtClaims {
            sub: "you".to_string(),
            scopes: vec!["Invalid_Scope".to_string()],
        };
        assert!(matches!(
            Subject::from_jwt_claims(&claims),
            Err(AuthError::InvalidScope { .. })
        ));
    }

    #[test]
    fn from_jwt_claims_rejects_wildcard_in_token() {
        // A remote caller cannot self-elevate by writing "*" in their
        // token — wildcard is not kebab-case.
        let claims = JwtClaims {
            sub: "attacker".to_string(),
            scopes: vec!["*".to_string()],
        };
        assert!(matches!(
            Subject::from_jwt_claims(&claims),
            Err(AuthError::InvalidScope { .. })
        ));
    }

    #[test]
    fn subject_serde_roundtrip() {
        let s = Subject::from_jwt_claims(&JwtClaims {
            sub: "you".to_string(),
            scopes: vec!["molecule-observe".to_string()],
        })
        .unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let back: Subject = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
