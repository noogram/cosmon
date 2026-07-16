// SPDX-License-Identifier: AGPL-3.0-only

//! Host-sealed operator credential for the admin provisioning surface
//! (B2 — impl of the B1 design
//! `docs/admin-provisioning-design.md` §2.2/§4.2).
//!
//! The admin surface (`/v1/admin/*`) writes the §8j root-of-trust — the
//! `(iss, sub) → noyau` binding. Its auth therefore **cannot** derive
//! from the tenant OIDC chain that binding exists to backstop (the B1
//! "posture (a) by the back door" trap, design §2.1). Instead the seal
//! is a **host-side operator credential**, disjoint from any JWT:
//!
//! - **NEVER** an OIDC JWT, **NEVER** mintable by the `IdP`, **NEVER**
//!   carried in a tenant token.
//! - Provisioned at container boot from `COSMON_ADMIN_TOKEN` (env) or a
//!   mounted secret file (`COSMON_ADMIN_TOKEN_FILE`). Lives in the same
//!   trust domain as the host-side `.toml` the route replaces: whoever
//!   can set the boot secret is whoever could already SSH-write the
//!   binding. No new authority is minted — only the *channel* changes
//!   (SSH → HTTP-with-seal).
//! - **Fail-closed**: no seal configured ⇒ the admin surface is CLOSED
//!   (`403 admin_disabled`). A deployment that never sets the secret is
//!   non-regressive — the routes simply stay shut.
//!
//! The seal stores the BLAKE3 *hash* of the configured token, never the
//! token itself. Verification hashes the presented token and compares
//! two fixed-size digests — `blake3::Hash`'s `PartialEq` is
//! constant-time, so there is no length- or content-dependent
//! short-circuit to time.

use axum::http::{HeaderMap, StatusCode};

use crate::error::ApiError;

/// Header the operator presents the admin token in. Lower-case because
/// `http::HeaderMap` lookups are case-insensitive but we compare the
/// canonical form.
pub const ADMIN_TOKEN_HEADER: &str = "x-cosmon-admin-token";

/// Env var carrying the admin token directly (highest precedence).
pub const ADMIN_TOKEN_ENV: &str = "COSMON_ADMIN_TOKEN";

/// Env var naming a file whose (trimmed) contents are the admin token —
/// the mounted-secret path (docker/k8s secret), used when the token
/// must not appear in the process environment listing.
pub const ADMIN_TOKEN_FILE_ENV: &str = "COSMON_ADMIN_TOKEN_FILE";

/// Sealed operator credential guarding `/v1/admin/*`.
///
/// `expected == None` is the fail-closed state: the admin surface is
/// disabled and every request to it is rejected `403 admin_disabled`.
#[derive(Clone)]
pub struct AdminSeal {
    /// BLAKE3 of the configured admin token. `None` ⇒ surface disabled.
    expected: Option<blake3::Hash>,
}

// Hand-rolled Debug so a misplaced `{:?}` never prints the sealed hash
// (defence-in-depth — the hash is not the secret, but it is operator
// material and has no business in a log line).
impl std::fmt::Debug for AdminSeal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminSeal")
            .field("enabled", &self.expected.is_some())
            .finish()
    }
}

impl AdminSeal {
    /// Build the seal from the boot environment. Precedence:
    /// `COSMON_ADMIN_TOKEN` (env) → `COSMON_ADMIN_TOKEN_FILE` (mounted
    /// secret) → disabled. An empty / whitespace-only value is treated
    /// as absent (fail-closed) rather than sealing the empty string.
    #[must_use]
    pub fn from_env() -> Self {
        if let Some(token) = std::env::var(ADMIN_TOKEN_ENV)
            .ok()
            .filter(|t| !t.trim().is_empty())
        {
            return Self::from_token(token.trim());
        }
        if let Some(path) = std::env::var_os(ADMIN_TOKEN_FILE_ENV) {
            match std::fs::read_to_string(&path) {
                Ok(contents) if !contents.trim().is_empty() => {
                    return Self::from_token(contents.trim());
                }
                Ok(_) => {
                    tracing::warn!(
                        event = "boot.admin_seal",
                        "COSMON_ADMIN_TOKEN_FILE is empty — admin surface stays closed",
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        event = "boot.admin_seal",
                        error = %e,
                        "COSMON_ADMIN_TOKEN_FILE unreadable — admin surface stays closed",
                    );
                }
            }
        }
        Self { expected: None }
    }

    /// Seal a concrete token (used by [`Self::from_env`] and tests).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        Self {
            expected: Some(blake3::hash(token.as_bytes())),
        }
    }

    /// The disabled (fail-closed) seal — no token configured.
    #[must_use]
    pub fn disabled() -> Self {
        Self { expected: None }
    }

    /// Whether the admin surface is open (a seal was configured at boot).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.expected.is_some()
    }

    /// Guard the admin surface: extract `X-Cosmon-Admin-Token` and
    /// compare it constant-time against the sealed hash.
    ///
    /// # Errors
    ///
    /// - `403 admin_disabled` — no seal configured at boot (fail-closed).
    /// - `401 admin_token_missing` — header absent or not valid UTF-8.
    /// - `401 admin_token_invalid` — token present but does not match.
    ///
    /// A valid tenant JWT in `Authorization: Bearer …` does **not** open
    /// this door — the route never reads it. Only the dedicated seal
    /// header is consulted (design E2/E4).
    pub fn require(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let Some(expected) = self.expected.as_ref() else {
            return Err(ApiError::with_status(
                StatusCode::FORBIDDEN,
                "admin_disabled",
            ));
        };
        let presented = headers
            .get(ADMIN_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                ApiError::with_status(StatusCode::UNAUTHORIZED, "admin_token_missing")
            })?;
        // Constant-time: hash the presented token, compare two
        // fixed-size BLAKE3 digests (no length short-circuit).
        if blake3::hash(presented.as_bytes()) == *expected {
            Ok(())
        } else {
            Err(ApiError::with_status(
                StatusCode::UNAUTHORIZED,
                "admin_token_invalid",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ADMIN_TOKEN_HEADER, HeaderValue::from_str(token).unwrap());
        h
    }

    #[test]
    fn disabled_seal_rejects_with_403() {
        let seal = AdminSeal::disabled();
        assert!(!seal.is_enabled());
        let err = seal.require(&headers_with("anything")).unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.label, "admin_disabled");
    }

    #[test]
    fn correct_token_passes() {
        let seal = AdminSeal::from_token("s3cret-operator-token");
        assert!(seal.is_enabled());
        assert!(seal.require(&headers_with("s3cret-operator-token")).is_ok());
    }

    #[test]
    fn wrong_token_rejects_with_401_invalid() {
        let seal = AdminSeal::from_token("s3cret-operator-token");
        let err = seal.require(&headers_with("wrong")).unwrap_err();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.label, "admin_token_invalid");
    }

    #[test]
    fn missing_header_rejects_with_401_missing() {
        let seal = AdminSeal::from_token("s3cret-operator-token");
        let err = seal.require(&HeaderMap::new()).unwrap_err();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.label, "admin_token_missing");
    }

    #[test]
    fn debug_never_prints_the_hash() {
        let seal = AdminSeal::from_token("s3cret");
        let dbg = format!("{seal:?}");
        assert!(dbg.contains("enabled"));
        assert!(!dbg.contains("s3cret"));
        // The hex digest of the token must not leak either.
        let hex = blake3::hash(b"s3cret").to_hex().to_string();
        assert!(!dbg.contains(&hex));
    }
}
