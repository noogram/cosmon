// SPDX-License-Identifier: AGPL-3.0-only

//! The OAuth2-PKCE flow error taxonomy (delib-20260710-33b7 C4).
//!
//! [`OidcError`] is an **own** `#[non_exhaustive]` enum whose semver
//! `cosmon-remote` controls. It is folded into [`crate::Error`] via **one owned
//! transparent `#[from]`**, exactly like [`crate::CredentialStoreError`]. Every
//! *foreign* error (`reqwest::Error` from the network, a `url::ParseError` while
//! building the authorize URL) is captured **opaquely** as
//! [`OidcError::Transport`] — it is never named in a public signature, so a
//! churn in the HTTP stack cannot break this crate's API.
//!
//! The load-bearing recoverable variant the caller branches on is
//! [`OidcError::RefreshExpired`]: it is the one signal that means "the silent
//! refresh cannot save you — run `login` again". Everything else is either a
//! transient transport failure or a protocol violation to surface verbatim.
//!
//! This enum is deliberately **separate** from the `Error::Auth` string variant
//! used by the Claude manual-paste flow (`pkce.rs`). The two flows do not share
//! an error type: overloading `Error::Auth` for both would erase the distinction
//! between "the Anthropic device-code paste was empty" and "the Forgejo refresh
//! token has been revoked".

use thiserror::Error;

/// Failures raised by the OAuth2-PKCE login and silent-refresh flow.
///
/// New variants may be added in a minor release — downstream matches must carry
/// a `_` arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OidcError {
    /// OIDC provider metadata (`.well-known/openid-configuration`) or the
    /// cosmon client-registry (`.well-known/cosmon-oauth-clients`) could not be
    /// fetched, parsed, or declared a `schema_version` newer than this binary
    /// understands (fail-closed — we never guess at a future shape).
    #[error("OIDC discovery failed: {reason}")]
    Discovery {
        /// Human-readable cause, safe to print (never carries secret material).
        reason: String,
    },

    /// The `state` value echoed on the loopback callback did not match the one
    /// this process generated for the flow — a CSRF / session-fixation signal.
    /// The authorization code is discarded unused.
    #[error("OAuth state mismatch — the callback did not originate from this login attempt")]
    StateMismatch,

    /// The authorization server (or the loopback callback) reported a structured
    /// OAuth error (`error` + optional `error_description`), e.g. `access_denied`
    /// when the operator refuses consent, or `invalid_grant` when a code or
    /// refresh token is rejected.
    #[error("authorization server error: {error}{}", .description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default())]
    Server {
        /// The RFC 6749 `error` code (`invalid_grant`, `access_denied`, …).
        error: String,
        /// The optional human-readable `error_description`.
        description: Option<String>,
    },

    /// The stored refresh token was rejected by the token endpoint and no peer
    /// on this machine holds a fresher one — the silent refresh is exhausted and
    /// a full browser `login` is required. This is the one recoverable signal the
    /// caller acts on (re-run `login`).
    #[error("refresh token expired or revoked — run `login` again")]
    RefreshExpired,

    /// A freshly minted or rotated credential could not be durably persisted
    /// because the resolved backend is read-only ([`crate::BackendKind::Env`] —
    /// the static `$COSMON_REMOTE_TOKEN` bearer). Persist-before-use cannot
    /// hold, so the flow fails loud rather than hand out a token the store
    /// silently discarded (adversarial review F4). Unset `$COSMON_REMOTE_TOKEN`
    /// to resolve a writable keyring/file backend, then re-run `login`.
    #[error(
        "credential not persisted: the resolved backend is read-only \
         (static $COSMON_REMOTE_TOKEN bearer) — unset it to use a stored credential"
    )]
    CredentialNotPersisted,

    /// The loopback callback could not be captured: the listener could not bind
    /// the redirect port, the browser never returned, or the request was
    /// malformed. Carries a diagnostic, never a secret.
    #[error("loopback callback capture failed: {reason}")]
    Callback {
        /// What went wrong on the loopback listener (bind error, timeout, parse).
        reason: String,
    },

    /// An opaque transport / encoding failure (a `reqwest::Error`, a
    /// `url::ParseError`). Boxed so no foreign error type leaks into
    /// `cosmon-remote`'s public API (the semver surface stays closed).
    #[error("OIDC transport error: {source}")]
    Transport {
        /// The captured cause, type-erased to keep the semver surface closed.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl OidcError {
    /// Wrap a foreign transport / encoding error opaquely as
    /// [`OidcError::Transport`]. The concrete type is erased on the way in so it
    /// never appears in a public signature.
    pub fn transport(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Transport {
            source: Box::new(source),
        }
    }

    /// Whether this error is a `Server` error carrying the RFC 6749
    /// `invalid_grant` code — the signal that a code or refresh token was
    /// rejected. The refresh loop uses this to decide whether to re-read the
    /// store (race-loss) before declaring [`OidcError::RefreshExpired`].
    pub fn is_invalid_grant(&self) -> bool {
        matches!(self, Self::Server { error, .. } if error == "invalid_grant")
    }
}
