// SPDX-License-Identifier: AGPL-3.0-only

//! In-memory mock OIDC `IdP` for §8j HTTPS+OIDC adapter tests.
//!
//! [`OidcMock`] spins up a tokio task hosting an axum app on a random
//! local port. The app exposes one route — `GET /jwks` — returning the
//! public side of the embedded test key in RFC 7517 JWKS shape. The
//! same key is used by [`OidcMock::issue_jwt`] to sign JWTs the way a
//! real `IdP` would.
//!
//! The mock is intentionally minimal:
//!
//! - No discovery document — adapters today do not fetch
//!   `/.well-known/openid-configuration`. They consult JWKS by file or
//!   by URL once at boot.
//! - No client registration / token endpoint — the test caller decides
//!   `(sub, aud, scopes, lifetime)` and signs the result directly.
//! - No `Audience` enforcement on `/jwks` — JWKS is public-by-design.
//!
//! When the real adapter pulls JWKS over HTTP (V1+ feature, currently
//! disk-pinned), [`OidcMock::jwks_url`] is the URL it should target.
//! Until then, [`OidcMock::write_jwks_file`] projects the same key
//! material into the on-disk format the adapter loads at boot.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

use crate::{TEST_RSA_E_B64URL, TEST_RSA_N_B64URL, TEST_RSA_PRIVATE_PEM};

/// Default issuer used by [`OidcMock::start`].
pub const DEFAULT_ISSUER: &str = "https://idp.test.cosmon-oidc-testkit";

/// Default audience used by [`OidcMock::start`].
pub const DEFAULT_AUDIENCE: &str = "cosmon-rpp-test";

/// Default `kid` used by [`OidcMock::start`].
pub const DEFAULT_KID: &str = "test-kid-1";

/// Configuration for [`OidcMock::start_with`]. Each field has a
/// reasonable default so callers can override one parameter at a time
/// without restating the others.
#[derive(Clone, Debug)]
pub struct OidcMockConfig {
    /// `iss` claim emitted in every issued JWT and pinned in the
    /// JWKS file produced by [`OidcMock::write_jwks_file`].
    pub issuer: String,
    /// Audiences accepted by the adapter. The first entry is the one
    /// [`OidcMock::issue_jwt`] embeds by default.
    pub audiences: Vec<String>,
    /// `kid` used in JWT headers and JWKS records.
    pub kid: String,
    /// Default lifetime applied by [`OidcMock::issue_jwt`] (seconds).
    pub default_lifetime_secs: u64,
}

impl Default for OidcMockConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_owned(),
            audiences: vec![DEFAULT_AUDIENCE.to_owned()],
            kid: DEFAULT_KID.to_owned(),
            default_lifetime_secs: 600,
        }
    }
}

/// In-memory mock `IdP` — see module documentation.
#[derive(Debug)]
pub struct OidcMock {
    config: OidcMockConfig,
    jwks_url: String,
    /// Background task hosting the JWKS endpoint. Dropping the
    /// [`OidcMock`] aborts the task and releases the port.
    server: tokio::task::JoinHandle<()>,
}

impl OidcMock {
    /// Start a mock with [`OidcMockConfig::default`]. Convenience
    /// shortcut for the most common case.
    pub async fn start() -> Self {
        Self::start_with(OidcMockConfig::default()).await
    }

    /// Start a mock with the supplied configuration. Binds to
    /// `127.0.0.1:0` (a random free port) and serves the JWKS at
    /// `GET /jwks`.
    ///
    /// # Panics
    ///
    /// Panics if `127.0.0.1:0` cannot be bound (extremely unusual:
    /// would indicate the loopback interface is unavailable).
    pub async fn start_with(config: OidcMockConfig) -> Self {
        let jwks_body = jwks_body(&config.kid);
        let app = Router::new().route(
            "/jwks",
            get({
                let body = Arc::new(jwks_body);
                move || {
                    let body = Arc::clone(&body);
                    async move { Json((*body).clone()) }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to loopback for OidcMock");
        let addr = listener
            .local_addr()
            .expect("local_addr after bind succeeded");
        let server = tokio::spawn(async move {
            // Errors during shutdown (caller dropped the mock) are
            // expected; downgrade to a trace.
            if let Err(err) = axum::serve(listener, app).await {
                tracing::trace!(?err, "OidcMock axum::serve exited");
            }
        });
        let jwks_url = format!("http://{addr}/jwks");
        Self {
            config,
            jwks_url,
            server,
        }
    }

    /// Issue a JWT for `subject` with the mock's default audience and
    /// lifetime, optionally embedding a `scopes` claim. Returns the
    /// raw `header.payload.signature` token.
    #[must_use]
    pub fn issue_jwt(&self, subject: &str, scopes: &[&str]) -> String {
        self.issue(&IssueJwt {
            subject,
            audience: None,
            scopes,
            lifetime_secs: None,
            jti: None,
        })
    }

    /// Issue a JWT with full control over every claim. Useful for
    /// expiry edge-cases and cross-tenant pivot scenarios where the
    /// audience must override the mock default.
    #[must_use]
    pub fn issue(&self, opts: &IssueJwt<'_>) -> String {
        let now_s = chrono::Utc::now().timestamp();
        let lifetime = opts
            .lifetime_secs
            .unwrap_or(self.config.default_lifetime_secs);
        let aud = opts.audience.unwrap_or_else(|| {
            self.config
                .audiences
                .first()
                .map_or(DEFAULT_AUDIENCE, String::as_str)
        });
        let jti = opts
            .jti
            .map_or_else(|| format!("jti-{now_s}-{}", opts.subject), str::to_owned);
        let claims = json!({
            "iss": self.config.issuer,
            "sub": opts.subject,
            "aud": aud,
            "iat": now_s,
            "exp": now_s + i64::try_from(lifetime).unwrap_or(0),
            "jti": jti,
            "scopes": opts.scopes,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.config.kid.clone());
        let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())
            .expect("embedded test key is well-formed RSA PEM");
        encode(&header, &claims, &key).expect("encode JWT with test key")
    }

    /// Public URL of the JWKS endpoint. Format:
    /// `http://127.0.0.1:<port>/jwks`.
    #[must_use]
    pub fn jwks_url(&self) -> &str {
        &self.jwks_url
    }

    /// Configured issuer (the `iss` claim emitted in every JWT).
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.config.issuer
    }

    /// Configured key id (matches the JWKS record).
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.config.kid
    }

    /// Pinned audiences — copy returned so callers can pass them to a
    /// `JwksStore` builder without re-allocating from the iterator.
    #[must_use]
    pub fn audiences(&self) -> Vec<String> {
        self.config.audiences.clone()
    }

    /// Default audience (first entry in [`OidcMockConfig::audiences`]).
    #[must_use]
    pub fn default_audience(&self) -> &str {
        self.config
            .audiences
            .first()
            .map_or(DEFAULT_AUDIENCE, String::as_str)
    }

    /// Pre-computed JWK record (no allocation per call beyond
    /// `serde_json::Value` cloning). Suitable for embedding in custom
    /// JWKS responses or for round-trip serialisation tests.
    #[must_use]
    pub fn jwks_value(&self) -> Value {
        jwks_body(&self.config.kid)
    }

    /// Project the JWKS into the on-disk format
    /// `cosmon-rpp-adapter::JwksStore::load` expects.
    ///
    /// The file is written to
    /// `<state_dir>/security/jwks/<sanitised_iss>.json`. The directory
    /// tree is created if needed. Returns the absolute path of the
    /// file produced.
    pub fn write_jwks_file(&self, state_dir: &Path) -> std::io::Result<PathBuf> {
        let dir = state_dir.join("security").join("jwks");
        std::fs::create_dir_all(&dir)?;
        let stem = sanitise_for_filename(&self.config.issuer);
        let path = dir.join(format!("{stem}.json"));
        let body = json!({
            "iss": self.config.issuer,
            "audiences": self.config.audiences,
            "keys": [
                {
                    "kid": self.config.kid,
                    "alg": "RS256",
                    "kty": "rsa",
                    "n": TEST_RSA_N_B64URL,
                    "e": TEST_RSA_E_B64URL,
                }
            ],
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&body)?)?;
        Ok(path)
    }
}

impl Drop for OidcMock {
    fn drop(&mut self) {
        // Abort the JWKS server so the loopback port is released
        // immediately. The tokio runtime will surface the cancel.
        self.server.abort();
    }
}

/// Builder-style options for [`OidcMock::issue`]. Fields are borrowed
/// where it is cheap; the resulting JWT does not retain references.
#[derive(Clone, Debug)]
pub struct IssueJwt<'a> {
    /// `sub` claim — the principal identifier signed into the token.
    pub subject: &'a str,
    /// Override the audience claim. `None` uses
    /// [`OidcMock::default_audience`].
    pub audience: Option<&'a str>,
    /// Optional `scopes` array (mirrors the `OAuth2` convention).
    pub scopes: &'a [&'a str],
    /// Override the token lifetime. `None` uses
    /// [`OidcMockConfig::default_lifetime_secs`].
    pub lifetime_secs: Option<u64>,
    /// Override the `jti`. `None` derives one from the subject and
    /// the current timestamp.
    pub jti: Option<&'a str>,
}

fn jwks_body(kid: &str) -> Value {
    json!({
        "keys": [
            {
                "kid": kid,
                "alg": "RS256",
                "kty": "RSA",
                "use": "sig",
                "n": TEST_RSA_N_B64URL,
                "e": TEST_RSA_E_B64URL,
            }
        ]
    })
}

/// Make an issuer URL safe to use as a filename stem (no `/`, no `:`).
fn sanitise_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | ':' | '\\' | '?' | '*' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

/// Absolute path to the `fake-cs` binary built alongside this crate
/// by `build.rs`. Use it as the `cs_path` for
/// `cosmon-rpp-adapter::AppState` in integration tests that exercise
/// the subprocess envelope without rebuilding the real `cs` binary.
#[must_use]
pub fn fake_cs_path() -> PathBuf {
    PathBuf::from(env!("COSMON_OIDC_TESTKIT_FAKE_CS"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
    use std::sync::OnceLock;

    fn rt() -> &'static tokio::runtime::Runtime {
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
        })
    }

    #[test]
    fn issue_jwt_uses_default_audience_and_kid() {
        let mock = rt().block_on(OidcMock::start());
        let token = mock.issue_jwt("sub-x", &["cosmon:molecule:read"]);
        let header = decode_header(&token).unwrap();
        assert_eq!(header.kid.as_deref(), Some(DEFAULT_KID));
        assert_eq!(header.alg, Algorithm::RS256);

        let key = DecodingKey::from_rsa_pem(crate::TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[DEFAULT_AUDIENCE]);
        validation.set_issuer(&[DEFAULT_ISSUER]);
        let data = decode::<serde_json::Value>(&token, &key, &validation).unwrap();
        assert_eq!(data.claims["sub"], "sub-x");
        assert_eq!(data.claims["aud"], DEFAULT_AUDIENCE);
        assert_eq!(data.claims["scopes"][0], "cosmon:molecule:read");
    }

    #[test]
    fn issue_can_override_audience_and_jti() {
        let mock = rt().block_on(OidcMock::start_with(OidcMockConfig {
            audiences: vec!["aud-a".into(), "aud-b".into()],
            ..OidcMockConfig::default()
        }));
        let token = mock.issue(&IssueJwt {
            subject: "sub-y",
            audience: Some("aud-b"),
            scopes: &[],
            lifetime_secs: Some(60),
            jti: Some("custom-jti"),
        });
        let key = DecodingKey::from_rsa_pem(crate::TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["aud-b"]);
        validation.set_issuer(&[DEFAULT_ISSUER]);
        let data = decode::<serde_json::Value>(&token, &key, &validation).unwrap();
        assert_eq!(data.claims["jti"], "custom-jti");
        assert_eq!(data.claims["aud"], "aud-b");
    }

    #[test]
    fn jwks_url_serves_keys_endpoint() {
        let mock = rt().block_on(OidcMock::start());
        let url = mock.jwks_url().to_owned();
        assert!(url.starts_with("http://127.0.0.1:"));
        let body =
            rt().block_on(async move { reqwest::get(&url).await.unwrap().json::<Value>().await });
        let body = body.unwrap();
        assert_eq!(body["keys"][0]["kid"], DEFAULT_KID);
        assert_eq!(body["keys"][0]["kty"], "RSA");
        assert_eq!(body["keys"][0]["alg"], "RS256");
    }

    #[test]
    fn write_jwks_file_emits_loadable_format() {
        let td = tempfile::tempdir().unwrap();
        let mock = rt().block_on(OidcMock::start());
        let path = mock.write_jwks_file(td.path()).unwrap();
        assert!(path.exists());
        let parent = path.parent().unwrap();
        assert!(parent.ends_with("security/jwks"));
        let text = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["iss"], DEFAULT_ISSUER);
        assert_eq!(v["audiences"][0], DEFAULT_AUDIENCE);
        assert_eq!(v["keys"][0]["kid"], DEFAULT_KID);
        assert_eq!(v["keys"][0]["alg"], "RS256");
        assert_eq!(v["keys"][0]["kty"], "rsa");
    }
}
