// SPDX-License-Identifier: AGPL-3.0-only

//! The credential-store key — the audience-isolation mechanism (C1).
//!
//! Credentials are keyed on the **claim identity tuple `(issuer, sub, aud)`**,
//! where `aud == client_id` (Forgejo hardcodes `aud = client_id`). This is the
//! single most important decision in the contract: the key *is* the isolation.
//! Because aud=A (`cs-rpp-adapter`) and aud=B (`claude-web`) have globally
//! unique client_ids, they land in physically distinct slots — presenting an
//! A-token to a B-resource is *structurally impossible* because the lookup for
//! B never retrieves A's token.
//!
//! Two subtleties the panel insisted on:
//!
//! - `issuer` is **what the token is** (its minting authority, the discovered
//!   `iss`), *not* `host` (**where it is spent**). Keying on `host` would let a
//!   token minted for one issuer be reused against a look-alike host.
//! - `sub` distinguishes multiple identities on one machine (operator + avatar,
//!   or a multi-tenant box). Omitting it collapses two humans into one slot.
//!
//! The fields are **private** (tolnay): the tuple can widen later — a fourth
//! component, a normalization change — without altering any method signature,
//! because callers construct via [`CredentialKey::new`] and address slots via
//! the opaque [`CredentialKey::storage_id`].

/// A credential-store key: the claim identity `(issuer, sub, aud)`.
///
/// Construct with [`CredentialKey::new`]. Equality and the derived
/// [`CredentialKey::storage_id`] are exact over all three components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialKey {
    issuer: String,
    sub: String,
    aud: String,
}

impl CredentialKey {
    /// Build a key from the claim identity tuple.
    ///
    /// ```
    /// use cosmon_remote::credential::CredentialKey;
    /// let a = CredentialKey::new("https://forge.example", "operator", "cs-rpp-adapter");
    /// let b = CredentialKey::new("https://forge.example", "operator", "claude-web");
    /// // Different audiences → different physical slots.
    /// assert_ne!(a.storage_id(), b.storage_id());
    /// ```
    pub fn new(issuer: impl Into<String>, sub: impl Into<String>, aud: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            sub: sub.into(),
            aud: aud.into(),
        }
    }

    /// The token's minting authority (the discovered OIDC `iss`).
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The subject identity (`sub` claim) the token was minted for.
    pub fn sub(&self) -> &str {
        &self.sub
    }

    /// The audience (`aud` claim, `== client_id`) the token is scoped to.
    pub fn aud(&self) -> &str {
        &self.aud
    }

    /// A stable, collision-resistant, filesystem- and keyring-safe identifier
    /// for this key's slot: the BLAKE3 hex of the canonical serialization of
    /// `(issuer, sub, aud)`.
    ///
    /// The components are joined with the ASCII **unit separator** (`0x1F`) —
    /// which cannot appear in a URL, a `sub`, or a client_id — so the encoding
    /// is unambiguous before hashing, and the hash makes distinct tuples map to
    /// distinct ids with negligible collision probability. A leading scheme tag
    /// (`v1`) lets the id layout evolve without silently aliasing old slots.
    pub fn storage_id(&self) -> String {
        let canonical = format!(
            "cosmon-remote\u{1f}v1\u{1f}{}\u{1f}{}\u{1f}{}",
            self.issuer, self.sub, self.aud
        );
        blake3::hash(canonical.as_bytes()).to_hex().to_string()
    }
}
