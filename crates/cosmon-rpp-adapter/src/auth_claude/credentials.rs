// SPDX-License-Identifier: AGPL-3.0-only

//! Write `~/.claude/.credentials.json` in the format `claude` CLI
//! expects. Schema observed by reverse-engineering the macOS Keychain
//! store (`Claude Code-credentials`, 2026-05-19). See ADR-0017 §6
//! (Q-impl-2 RESOLVED) for documentation.

use std::path::Path;

use chrono::Utc;
use serde::Serialize;

use crate::auth_claude::anthropic::TokenResponse;

/// Top-level shape of `.credentials.json` — a single `claudeAiOauth`
/// object. The exact key (camelCase, leading lowercase) is preserved
/// to match `claude` CLI's deserialiser.
#[derive(Debug, Serialize)]
pub struct CredentialsFile<'a> {
    /// Inner OAuth payload.
    #[serde(rename = "claudeAiOauth")]
    pub claude_ai_oauth: ClaudeAiOauth<'a>,
}

/// Inner payload — mirrors the Keychain-stored object.
#[derive(Debug, Serialize)]
pub struct ClaudeAiOauth<'a> {
    /// Bearer access token.
    #[serde(rename = "accessToken")]
    pub access_token: &'a str,
    /// Refresh token.
    #[serde(rename = "refreshToken")]
    pub refresh_token: &'a str,
    /// Absolute expiry — Unix epoch milliseconds.
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
    /// Scopes granted, as a JSON array of strings.
    pub scopes: Vec<&'a str>,
    /// Subscription tier (`max`, `pro`, …).
    #[serde(rename = "subscriptionType", skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<&'a str>,
    /// Rate-limit tier (Anthropic-internal label).
    #[serde(rename = "rateLimitTier", skip_serializing_if = "Option::is_none")]
    pub rate_limit_tier: Option<&'a str>,
}

/// Errors writing the credentials file.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Underlying I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialisation error (should not happen with our types).
    #[error("serde_json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Compute `expiresAt` as Unix epoch ms — sum of current time and the
/// `expires_in` seconds field from the token response. Anthropic
/// returns ~31_536_000 (1 year) for `claude` CLI tokens at the time of
/// writing (2026-05).
fn compute_expires_at_ms(expires_in_s: Option<u64>) -> i64 {
    let now_ms = Utc::now().timestamp_millis();
    let extra_ms = expires_in_s
        .and_then(|s| i64::try_from(s).ok())
        .map_or(0, |s| s.saturating_mul(1000));
    now_ms.saturating_add(extra_ms)
}

/// Default scope set persisted when Anthropic does not echo back
/// scopes. Mirrors the constant in
/// [`crate::auth_claude::config::DEFAULT_SCOPES`] but expanded into
/// the array form `.credentials.json` expects.
const FALLBACK_SCOPES: &[&str] = &[
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];

/// Build the `.credentials.json` bytes for a successful token response.
/// Exposed for testing; production callers want [`write_credentials_file`].
pub fn render_credentials(resp: &TokenResponse) -> Result<Vec<u8>, WriteError> {
    let expires_at = compute_expires_at_ms(resp.expires_in);
    let scopes: Vec<&str> = resp
        .scope
        .as_deref()
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| FALLBACK_SCOPES.to_vec());
    let file = CredentialsFile {
        claude_ai_oauth: ClaudeAiOauth {
            access_token: &resp.access_token,
            refresh_token: &resp.refresh_token,
            expires_at,
            scopes,
            subscription_type: resp.subscription_type.as_deref(),
            rate_limit_tier: resp.rate_limit_tier.as_deref(),
        },
    };
    Ok(serde_json::to_vec(&file)?)
}

/// Write `path` with the rendered credentials, applying `chmod 0600`
/// on Unix to honour CI8 (no clear-text secrets readable outside the
/// owner). Parent directories are created if absent.
pub fn write_credentials_file(path: &Path, resp: &TokenResponse) -> Result<(), WriteError> {
    let bytes = render_credentials(resp)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    // Best-effort 0600 — chmod is Unix-only but the container is Linux.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_response() -> TokenResponse {
        TokenResponse {
            access_token: "sk-ant-oat01-abc".to_owned(),
            refresh_token: "sk-ant-ort01-def".to_owned(),
            expires_in: Some(31_536_000),
            scope: Some("user:profile user:inference".to_owned()),
            account: None,
            subscription_type: Some("max".to_owned()),
            rate_limit_tier: Some("default_claude_max_20x".to_owned()),
        }
    }

    #[test]
    fn render_has_correct_shape() {
        let bytes = render_credentials(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let oauth = parsed.get("claudeAiOauth").expect("claudeAiOauth field");
        assert_eq!(oauth["accessToken"], "sk-ant-oat01-abc");
        assert_eq!(oauth["refreshToken"], "sk-ant-ort01-def");
        assert_eq!(oauth["subscriptionType"], "max");
        assert_eq!(oauth["rateLimitTier"], "default_claude_max_20x");
        assert!(oauth["expiresAt"].is_i64());
        let scopes = oauth["scopes"].as_array().unwrap();
        assert!(scopes.iter().any(|v| v.as_str() == Some("user:profile")));
    }

    #[test]
    fn render_uses_fallback_scopes_when_anthropic_omits() {
        let mut r = sample_response();
        r.scope = None;
        let bytes = render_credentials(&r).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let scopes = parsed["claudeAiOauth"]["scopes"].as_array().unwrap();
        assert!(!scopes.is_empty());
        assert!(scopes
            .iter()
            .any(|v| v.as_str() == Some("user:sessions:claude_code")));
    }

    #[test]
    fn write_creates_file_with_0600_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/.claude/.credentials.json");
        write_credentials_file(&path, &sample_response()).unwrap();
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "credentials file must be chmod 0600");
        }
    }
}
