// SPDX-License-Identifier: AGPL-3.0-only

//! Error type for `cosmon-remote`.
//!
//! The CLI is a thin client: every non-trivial failure lives on the
//! wire. The `Error` enum distinguishes a small set of operator-visible
//! categories — `Http` (network/transport), `Api` (a structured 4xx/5xx
//! reply), `Config` (profile or config-file error), `Auth` (PKCE flow
//! state mismatch), `Io` (filesystem). The variants carry just enough
//! data to render a human-readable message; the raw `serde_json::Value`
//! is preserved on `Api` for `--json` callers.

use thiserror::Error;

/// Failures raised by the credential-store (delib-20260710-33b7 C4).
///
/// This is an **own** `#[non_exhaustive]` enum whose semver `cosmon-remote`
/// controls. Every *foreign* backend error (`keyring::Error`, an `io::Error`
/// from the file backend) is captured **opaquely** as
/// [`CredentialStoreError::Backend`] — it is never named in a public signature,
/// so a churn in the keyring crate cannot break our API. The recoverable
/// variants the caller (`oidc`) branches on are [`Self::Unavailable`] (degrade
/// to another backend) and the absent-credential case, which is **`Ok(None)`,
/// not a variant** (parse, don't validate).
///
/// New variants may be added in a minor release — downstream matches must carry
/// a `_` arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredentialStoreError {
    /// The selected backend cannot be reached (Secret Service absent on a
    /// headless Linux box), or an override is invalid or unsupported (such as
    /// `$COSMON_REMOTE_CRED_BACKEND=keyring` on Linux musl). Autodetection
    /// degrades to the 0600 file; an explicit unsupported override fails loud
    /// so a refreshable credential cannot silently evaporate.
    #[error("credential backend unavailable: {reason}")]
    Unavailable {
        /// Human-readable cause, safe to print (never carries secret material).
        reason: String,
    },

    /// A stored blob failed to parse, or declares a `schema_version` newer than
    /// this binary understands (fail-closed — we never guess at a future shape).
    #[error("stored credential is malformed: {reason}")]
    Malformed {
        /// Parse diagnostic (never echoes the token bytes).
        reason: String,
    },

    /// The 0600 credential file has group/other permission bits set, or is a
    /// symlink. The store refuses to read it rather than trust a widened file
    /// (turing-T6). Re-run `login` to rewrite it with tight permissions.
    #[error("insecure permissions on credential file: {path}")]
    InsecurePermissions {
        /// Offending path (rendered for the operator's `chmod`/`rm`).
        path: String,
    },

    /// An opaque error from the underlying backend (keyring, filesystem). Boxed
    /// so no foreign error type leaks into `cosmon-remote`'s public API.
    #[error("credential backend error: {source}")]
    Backend {
        /// The captured cause, type-erased to keep the semver surface closed.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The crate error.
///
/// Marked `#[non_exhaustive]` as of the release that introduced
/// [`CredentialStoreError`] (the "last free break", delib-20260710-33b7 C4):
/// downstream matches must carry a `_` arm so future variants (e.g. the
/// forthcoming `OidcError` fold from Child 2) do not break them.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error ({status}): {body}")]
    Api {
        status: u16,
        body: serde_json::Value,
    },

    #[error("config error: {0}")]
    Config(String),

    #[error("auth flow error: {0}")]
    Auth(String),

    /// `result` was asked for but the molecule has no deliverable yet.
    /// The server answered 200 with a `result_status`;
    /// the actionable next gesture is rendered at the call site, this
    /// variant only carries the verdict so the process exits non-zero
    /// — a script must be able to tell "no deliverable" from success.
    #[error("no deliverable yet (status: {status})")]
    NoDeliverable { status: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("TOML serialise error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),

    /// A credential-store failure, folded via one owned transparent `#[from]`
    /// (delib-20260710-33b7 C4). The inner enum owns the semver.
    #[error(transparent)]
    Credential(#[from] CredentialStoreError),

    /// An OAuth2-PKCE login / silent-refresh failure, folded via one owned
    /// transparent `#[from]` (delib-20260710-33b7 C4, Child 2). The inner enum
    /// owns the semver; [`crate::OidcError::RefreshExpired`] is the recoverable
    /// signal the caller acts on.
    #[error(transparent)]
    Oidc(#[from] crate::oidc::OidcError),
}

pub type Result<T> = std::result::Result<T, Error>;
