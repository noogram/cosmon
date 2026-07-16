// SPDX-License-Identifier: AGPL-3.0-only

//! Endpoint + client_id discovery (delib-20260710-33b7 C8).
//!
//! Two documents, one standard and one cosmon-namespaced, because Forgejo
//! answers half the question and not the other half:
//!
//! - **`GET <issuer>/.well-known/openid-configuration`** — the RFC 8414 / OIDC
//!   Discovery document. Standard, so we use it: it yields the `issuer`, the
//!   `authorization_endpoint`, and the `token_endpoint`. [`ProviderMetadata`].
//! - **`GET <host>/.well-known/cosmon-oauth-clients`** — a cosmon-namespaced
//!   reverse-discovery document. Forgejo has **no** Dynamic Client Registration
//!   (do not invent RFC 7591), so the `client_id` provisioned per audience must
//!   reach the client some other way. cosmon-server publishes it here, keyed by
//!   audience, covering **both** the CLI app (audience A) and the MCP connector
//!   app (audience B) in one document. [`ClientRegistry`].
//!
//! The split is deliberate: we speak the standard where a standard exists
//! (endpoints), and reserve the cosmon namespace only for the one thing the
//! standard cannot give us (the pre-provisioned `client_id`).
//!
//! # Integrity, not confidentiality
//!
//! `client_id` is public — but swapping it swaps the *audience* the token is
//! minted for, so the document needs **integrity** (fetched over the deployment's
//! TLS/install channel), and the resource server independently validates `aud`
//! against a closed allowlist of provisioned client_ids (that RS-side guard is
//! Child 3's, `task-20260710-909a`). This module's job is only to *fetch and
//! parse* honestly: the DTOs are **deserialize-only**, `#[non_exhaustive]`, and
//! unknown-field-tolerant, and the `schema_version` gate fails **closed** on a
//! version newer than this binary understands.

use serde::Deserialize;

use super::error::OidcError;
use crate::error::Result;

/// The `schema_version` of the `cosmon-oauth-clients` document this binary
/// understands. A document declaring a higher version is rejected
/// ([`OidcError::Discovery`]) rather than parsed on a guess — fail-closed.
pub const CLIENT_REGISTRY_SCHEMA: u32 = 1;

/// The subset of the OIDC Discovery document (`.well-known/openid-configuration`)
/// the login flow needs. **Deserialize-only** and unknown-field-tolerant: a
/// Forgejo upgrade that adds fields must not break an older client.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ProviderMetadata {
    /// The token's minting authority — the `iss` claim every token from this
    /// provider carries. This becomes the `issuer` component of the
    /// [`crate::credential::CredentialKey`], so it is *what the token is*, never
    /// *where it is spent*.
    pub issuer: String,
    /// Where the browser is sent to obtain an authorization code.
    pub authorization_endpoint: String,
    /// Where the authorization code (and later the refresh token) is exchanged
    /// for tokens.
    pub token_endpoint: String,
}

impl ProviderMetadata {
    /// The well-known path appended to the issuer base to fetch this document.
    pub const WELL_KNOWN_PATH: &'static str = "/.well-known/openid-configuration";

    /// Fetch and parse the provider metadata from `issuer_base` (a URL like
    /// `https://forge.example`, trailing slash tolerated). Network failures and
    /// malformed bodies both surface as [`OidcError::Discovery`].
    pub async fn fetch(http: &reqwest::Client, issuer_base: &str) -> Result<Self> {
        let url = format!(
            "{}{}",
            issuer_base.trim_end_matches('/'),
            Self::WELL_KNOWN_PATH
        );
        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| OidcError::Discovery {
                reason: format!("GET {url}: {e}"),
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(OidcError::Discovery {
                reason: format!("GET {url} returned HTTP {}", status.as_u16()),
            }
            .into());
        }
        resp.json::<Self>()
            .await
            .map_err(|e| OidcError::Discovery {
                reason: format!("parsing {url}: {e}"),
            })
            .map_err(Into::into)
    }
}

/// One provisioned OAuth client, keyed by its logical audience label. **This is
/// the `(audience → client_id)` binding** the resource server hardcodes
/// (`aud == client_id`).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct OAuthClient {
    /// The logical audience label (`cs-rpp-adapter` for the CLI, `claude-web`
    /// for the MCP connector). Matched against [`crate::config::Profile::aud`].
    pub audience: String,
    /// The Forgejo-generated `client_id`. Because `aud == client_id`, this is
    /// also the `aud` component of the credential key — the isolation slot.
    pub client_id: String,
    /// The redirect URI registered for this client, if the server chooses to
    /// publish it. When absent the client uses the loopback default
    /// (`http://127.0.0.1:7777/callback`).
    #[serde(default)]
    pub redirect_uri: Option<String>,
    /// The scopes the server recommends for this client, if published. When
    /// absent the client falls back to the profile's scopes.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

/// The cosmon reverse-discovery document (`.well-known/cosmon-oauth-clients`).
///
/// **Deserialize-only**, `#[non_exhaustive]`, unknown-field-tolerant. The
/// `schema_version` is the fail-closed guard: [`Self::require_supported`] rejects
/// a document from the future rather than guessing at its shape.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ClientRegistry {
    /// The document schema version. A value greater than
    /// [`CLIENT_REGISTRY_SCHEMA`] is refused.
    pub schema_version: u32,
    /// The provisioned clients, one per audience (covers both A and B).
    #[serde(default)]
    pub clients: Vec<OAuthClient>,
}

impl ClientRegistry {
    /// The well-known path appended to the host base to fetch this document.
    pub const WELL_KNOWN_PATH: &'static str = "/.well-known/cosmon-oauth-clients";

    /// Fetch, parse, and version-gate the registry from `host_base` (the
    /// rpp-adapter host, e.g. `https://cosmon.example.ts.net`).
    pub async fn fetch(http: &reqwest::Client, host_base: &str) -> Result<Self> {
        let url = format!(
            "{}{}",
            host_base.trim_end_matches('/'),
            Self::WELL_KNOWN_PATH
        );
        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| OidcError::Discovery {
                reason: format!("GET {url}: {e}"),
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(OidcError::Discovery {
                reason: format!("GET {url} returned HTTP {}", status.as_u16()),
            }
            .into());
        }
        let registry = resp
            .json::<Self>()
            .await
            .map_err(|e| OidcError::Discovery {
                reason: format!("parsing {url}: {e}"),
            })?;
        registry.require_supported()?;
        Ok(registry)
    }

    /// Reject a document whose `schema_version` exceeds what this binary
    /// understands (fail-closed). A parse from a newer, unknown layout could
    /// silently mis-bind an audience to the wrong client_id.
    pub fn require_supported(&self) -> Result<()> {
        if self.schema_version > CLIENT_REGISTRY_SCHEMA {
            return Err(OidcError::Discovery {
                reason: format!(
                    "cosmon-oauth-clients schema_version {} is newer than this binary understands \
                     (≤ {CLIENT_REGISTRY_SCHEMA}); upgrade cosmon-remote",
                    self.schema_version
                ),
            }
            .into());
        }
        Ok(())
    }

    /// The provisioned client for `audience`, or `None` if the deployment does
    /// not publish one (the caller then surfaces a precise "audience not
    /// provisioned" error rather than guessing a client_id).
    pub fn client_for(&self, audience: &str) -> Option<&OAuthClient> {
        self.clients.iter().find(|c| c.audience == audience)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_metadata_parses_and_ignores_unknown_fields() {
        let json = r#"{
            "issuer": "https://forge.example",
            "authorization_endpoint": "https://forge.example/login/oauth/authorize",
            "token_endpoint": "https://forge.example/login/oauth/access_token",
            "userinfo_endpoint": "https://forge.example/login/oauth/userinfo",
            "response_types_supported": ["code"]
        }"#;
        let meta: ProviderMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.issuer, "https://forge.example");
        assert_eq!(
            meta.authorization_endpoint,
            "https://forge.example/login/oauth/authorize"
        );
        assert_eq!(
            meta.token_endpoint,
            "https://forge.example/login/oauth/access_token"
        );
    }

    #[test]
    fn registry_selects_client_by_audience() {
        let json = r#"{
            "schema_version": 1,
            "clients": [
                {"audience": "cs-rpp-adapter", "client_id": "aaa-111"},
                {"audience": "claude-web", "client_id": "bbb-222", "redirect_uri": "http://127.0.0.1:7777/callback"}
            ]
        }"#;
        let reg: ClientRegistry = serde_json::from_str(json).unwrap();
        reg.require_supported().unwrap();
        assert_eq!(
            reg.client_for("cs-rpp-adapter").unwrap().client_id,
            "aaa-111"
        );
        assert_eq!(reg.client_for("claude-web").unwrap().client_id, "bbb-222");
        assert!(reg.client_for("unknown-audience").is_none());
    }

    #[test]
    fn registry_fails_closed_on_future_schema() {
        let json = r#"{"schema_version": 999, "clients": []}"#;
        let reg: ClientRegistry = serde_json::from_str(json).unwrap();
        let err = reg.require_supported().unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn registry_tolerates_unknown_top_level_and_client_fields() {
        // A forward-compatible field must not hard-fail an older binary — only
        // the schema_version gate is authoritative.
        let json = r#"{
            "schema_version": 1,
            "generated_at": "2026-07-10T00:00:00Z",
            "clients": [
                {"audience": "cs-rpp-adapter", "client_id": "aaa", "grant_types": ["authorization_code"]}
            ]
        }"#;
        let reg: ClientRegistry = serde_json::from_str(json).unwrap();
        reg.require_supported().unwrap();
        assert_eq!(reg.client_for("cs-rpp-adapter").unwrap().client_id, "aaa");
    }
}
