// SPDX-License-Identifier: AGPL-3.0-only

//! Secret material held in memory with best-effort zeroization (C6).
//!
//! The load-bearing honesty of this module lives on [`SecretToken`]: zeroize
//! shrinks the window during which a token sits readable in *this* process's
//! heap; it is **not** a containment boundary. See the type doc for the full
//! caveat. The real containment is the 15-minute access-token TTL, minimal
//! scopes, and independent revocation at the issuer.

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use zeroize::Zeroizing;

/// A bearer or refresh token held in process memory, wiped on drop.
///
/// # Zeroize is not a containment boundary
///
/// [`Zeroizing`] overwrites the backing heap allocation when the value is
/// dropped. That shrinks the in-process exposure window — but it does **not**
/// wipe copies the token spawns elsewhere: the TLS record buffers, `reqwest`'s
/// internal header clones, kernel socket buffers, a core dump, or the ghost
/// left by a `realloc` that moved the string. Treat this type as a *hygiene*
/// measure, never as a guarantee that the secret is gone from the machine.
///
/// # Escape hatches, deliberately absent
///
/// There is no `Clone`, no `Serialize`, and no `Display`. The only way to read
/// the plaintext is [`SecretToken::expose`], which names the one legitimate use
/// (constructing an `Authorization: Bearer` header). `Debug` is redacted so a
/// stray `{:?}` in a log line cannot leak the token.
///
/// ```
/// use cosmon_remote::credential::SecretToken;
/// let t = SecretToken::new("s3cr3t");
/// assert_eq!(t.expose(), "s3cr3t");
/// // A stray debug-print never leaks the bytes:
/// assert_eq!(format!("{t:?}"), "SecretToken(REDACTED)");
/// ```
pub struct SecretToken(Zeroizing<String>);

impl SecretToken {
    /// Wrap a raw token string. The input is moved (not copied) into the
    /// zeroizing allocation.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(Zeroizing::new(raw.into()))
    }

    /// Borrow the plaintext for the one legitimate use — building the
    /// `Authorization: Bearer <token>` header. The borrow is scoped; nothing
    /// is copied out unless the caller chooses to.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    /// Whether the token is the empty string. An empty refresh token marks a
    /// static (env-supplied) bearer that must never be refreshed.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretToken {
    /// Redacted — never prints the token bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken(REDACTED)")
    }
}

/// The credential triple persisted as one blob under one key
/// (delib-20260710-33b7 C1/C2): `{access_token, refresh_token, expires_at}`.
///
/// `expires_at` is an **absolute** instant (UTC), computed at mint/refresh time
/// from the server's relative `expires_in`. Storing the absolute instant makes
/// expiry a pure comparison against the wall clock at read time, with no
/// dependence on when the blob was written.
///
/// The type carries no `Serialize`/`Clone` — persistence goes through the
/// store's private wire form so plaintext never escapes accidentally.
pub struct StoredCredential {
    access_token: SecretToken,
    refresh_token: SecretToken,
    expires_at: DateTime<Utc>,
}

impl StoredCredential {
    /// Assemble a credential from its parts.
    pub fn new(
        access_token: SecretToken,
        refresh_token: SecretToken,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_at,
        }
    }

    /// A static bearer with no refresh capability and no meaningful expiry —
    /// the shape returned by the env (`$COSMON_REMOTE_TOKEN`) backend. The
    /// refresh token is empty ([`SecretToken::is_empty`]) and `expires_at` is
    /// [`DateTime::<Utc>::MAX_UTC`], so [`Self::is_expired`] is always `false`
    /// and the caller must never attempt to refresh it.
    pub fn static_bearer(access_token: SecretToken) -> Self {
        Self {
            access_token,
            refresh_token: SecretToken::new(String::new()),
            expires_at: DateTime::<Utc>::MAX_UTC,
        }
    }

    /// The access token (presented as the request bearer).
    pub fn access_token(&self) -> &SecretToken {
        &self.access_token
    }

    /// The refresh token (used by `oidc` to mint a fresh access token).
    pub fn refresh_token(&self) -> &SecretToken {
        &self.refresh_token
    }

    /// The absolute expiry instant of the access token.
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// Whether a non-empty refresh token is present. `false` marks a static
    /// (env) bearer that must be re-obtained via a full login rather than a
    /// refresh grant.
    pub fn has_refresh(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// Whether the access token has expired as of `now`. Expiry is inclusive:
    /// exactly at `expires_at` the token is considered expired.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Whether the access token expires within `leeway` of `now` — the
    /// proactive-refresh trigger, so a request is not sent with a token about
    /// to lapse mid-flight. Computed via `signed_duration_since` to stay
    /// overflow-safe against [`DateTime::<Utc>::MAX_UTC`].
    pub fn is_expired_within(&self, now: DateTime<Utc>, leeway: Duration) -> bool {
        self.expires_at.signed_duration_since(now) <= leeway
    }
}

impl fmt::Debug for StoredCredential {
    /// Redacts both tokens; shows only the (non-secret) expiry instant.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCredential")
            .field("access_token", &"REDACTED")
            .field("refresh_token", &"REDACTED")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}
