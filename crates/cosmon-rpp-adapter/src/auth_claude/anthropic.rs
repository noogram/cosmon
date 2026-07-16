// SPDX-License-Identifier: AGPL-3.0-only

//! Anthropic token exchange client. POSTs a **JSON** body
//! (`Content-Type: application/json`) to the token endpoint and
//! deserialises the JSON response.
//!
//! The wire shape mirrors the official `claude` CLI's
//! `exchangeCodeForTokens` byte-for-byte (claude-code source v2.1.88,
//! `src/services/oauth/client.ts:107-133`): the body is JSON — *not*
//! `application/x-www-form-urlencoded` — and it carries the CSRF `state`
//! echoed from the authorize step alongside the PKCE `code_verifier`.
//! See smithy ADR-0017 §13.

use serde::Deserialize;

use crate::auth_claude::config::AuthClaudeConfig;

/// Token exchange response. Anthropic returns the standard OAuth 2.0
/// success envelope plus optional `account.email_address` (the field
/// observed when reverse-engineering the macOS Keychain store; the
/// exact upstream key name is treated optimistically — fallback to
/// `account_email` if `account` is absent).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TokenResponse {
    /// Bearer access token (long-lived OAuth access token for Claude
    /// Code's user session).
    pub access_token: String,
    /// Refresh token (used by `claude` CLI to mint new access tokens
    /// when this one expires).
    pub refresh_token: String,
    /// Token lifetime in seconds. May be missing in some responses.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Optional space-separated scope echo.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional account block carrying the user email.
    #[serde(default)]
    pub account: Option<TokenAccount>,
    /// Subscription tier as observed in the Keychain (`max` / `pro`).
    /// May be absent depending on the token endpoint's response shape.
    #[serde(default)]
    pub subscription_type: Option<String>,
    /// Rate limit tier as observed in the Keychain. Optional.
    #[serde(default)]
    pub rate_limit_tier: Option<String>,
}

/// Inner account block of [`TokenResponse`].
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TokenAccount {
    /// User email (canonical Anthropic-side identity).
    #[serde(rename = "email_address", alias = "email")]
    pub email: Option<String>,
}

/// Anthropic-side error envelope (RFC 6749 §5.2).
#[derive(Debug, Deserialize)]
pub struct OAuthErrorEnvelope {
    /// Stable machine-readable error code (`invalid_grant`,
    /// `invalid_client`, etc).
    pub error: String,
    /// Human-readable description.
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Errors arising from a token-exchange attempt.
#[derive(Debug, thiserror::Error)]
pub enum TokenExchangeError {
    /// Anthropic returned a 4xx with an OAuth error envelope.
    /// Wire status code is preserved.
    #[error("anthropic refused: {error} (status {status})")]
    AnthropicRefused {
        /// HTTP status returned by Anthropic.
        status: u16,
        /// OAuth error code (`invalid_grant`, etc.).
        error: String,
        /// Optional human-readable description.
        description: Option<String>,
    },
    /// Network failure (DNS, TCP, TLS, timeout) reaching Anthropic.
    #[error("anthropic unreachable: {0}")]
    Unreachable(String),
    /// Response body did not match the expected JSON shape.
    #[error("malformed response: {0}")]
    MalformedResponse(String),
    /// Anthropic returned a 5xx — treat as retryable, but the adapter
    /// surfaces it the same as `Unreachable` for now.
    #[error("anthropic 5xx: status {status}")]
    Server {
        /// HTTP status returned by Anthropic.
        status: u16,
    },
}

/// Exchange a paste-back `authorization_code` for tokens at the
/// configured token URL. Synchronous from the caller's perspective; a
/// single HTTP round-trip.
///
/// The body is POSTed as **JSON** (`Content-Type: application/json`) and
/// includes the CSRF `state` echoed from the authorize step — matching the
/// official `claude` CLI's `exchangeCodeForTokens` exactly. A
/// form-encoded body, or one omitting `state`, is rejected by Anthropic.
/// See smithy ADR-0017 §13.
pub async fn exchange_code(
    http: &reqwest::Client,
    config: &AuthClaudeConfig,
    authorization_code: &str,
    state: &str,
    code_verifier: &str,
) -> Result<TokenResponse, TokenExchangeError> {
    let body = build_token_request_body(
        authorization_code,
        state,
        code_verifier,
        &config.redirect_uri,
        &config.client_id,
    );

    let resp = http
        .post(&config.token_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| TokenExchangeError::Unreachable(e.to_string()))?;

    let status = resp.status();
    if status.is_success() {
        let body: TokenResponse = resp
            .json()
            .await
            .map_err(|e| TokenExchangeError::MalformedResponse(e.to_string()))?;
        return Ok(body);
    }

    if status.is_server_error() {
        return Err(TokenExchangeError::Server {
            status: status.as_u16(),
        });
    }

    // 4xx — parse OAuth error envelope if present.
    let body_text = resp
        .text()
        .await
        .map_err(|e| TokenExchangeError::Unreachable(e.to_string()))?;
    match serde_json::from_str::<OAuthErrorEnvelope>(&body_text) {
        Ok(env) => Err(TokenExchangeError::AnthropicRefused {
            status: status.as_u16(),
            error: env.error,
            description: env.error_description,
        }),
        Err(_) => Err(TokenExchangeError::AnthropicRefused {
            status: status.as_u16(),
            error: "unknown_error".to_owned(),
            description: Some(format!("status {status}: {body_text}")),
        }),
    }
}

/// Build the JSON token-exchange request body. Field set and shape mirror
/// the official `claude` CLI's `exchangeCodeForTokens` (claude-code source
/// v2.1.88): `grant_type`, `code`, `redirect_uri`, `client_id`,
/// `code_verifier`, and the CSRF `state`. Extracted as a pure function so
/// the wire shape is unit-testable without an HTTP round-trip.
fn build_token_request_body(
    authorization_code: &str,
    state: &str,
    code_verifier: &str,
    redirect_uri: &str,
    client_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "grant_type": "authorization_code",
        "code": authorization_code,
        "redirect_uri": redirect_uri,
        "client_id": client_id,
        "code_verifier": code_verifier,
        "state": state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_response_deserialises_minimal_envelope() {
        let raw = r#"{
            "access_token": "sk-ant-oat01-…",
            "refresh_token": "sk-ant-ort01-…"
        }"#;
        let parsed: TokenResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.access_token, "sk-ant-oat01-…");
        assert!(parsed.expires_in.is_none());
        assert!(parsed.account.is_none());
    }

    #[test]
    fn token_response_deserialises_full_envelope() {
        let raw = r#"{
            "access_token": "a",
            "refresh_token": "b",
            "expires_in": 31536000,
            "scope": "user:profile user:inference",
            "account": { "email_address": "x@y" },
            "subscription_type": "max",
            "rate_limit_tier": "default_claude_max_20x"
        }"#;
        let parsed: TokenResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.expires_in, Some(31_536_000));
        assert_eq!(parsed.account.unwrap().email.as_deref(), Some("x@y"));
        assert_eq!(parsed.subscription_type.as_deref(), Some("max"));
    }

    #[test]
    fn oauth_error_envelope_parses() {
        let raw = r#"{"error":"invalid_grant","error_description":"authorization code expired"}"#;
        let parsed: OAuthErrorEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.error, "invalid_grant");
        assert_eq!(
            parsed.error_description.as_deref(),
            Some("authorization code expired")
        );
    }

    #[test]
    fn token_request_body_matches_official_cli_shape() {
        // Mirrors claude-code v2.1.88 `exchangeCodeForTokens`: a JSON
        // object carrying grant_type, code, redirect_uri, client_id,
        // code_verifier, AND the CSRF state. See ADR-0017 §13.
        let body = build_token_request_body(
            "AUTH_CODE",
            "STATE_NONCE",
            "VERIFIER",
            "https://platform.claude.com/oauth/code/callback",
            "client-xyz",
        );
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["code"], "AUTH_CODE");
        assert_eq!(body["state"], "STATE_NONCE", "state MUST be in the body");
        assert_eq!(body["code_verifier"], "VERIFIER");
        assert_eq!(body["client_id"], "client-xyz");
        assert_eq!(
            body["redirect_uri"],
            "https://platform.claude.com/oauth/code/callback"
        );
        // Exactly six fields — no extras, none missing.
        assert_eq!(
            body.as_object().expect("json object").len(),
            6,
            "token request body must have exactly 6 fields: {body}"
        );
    }
}
