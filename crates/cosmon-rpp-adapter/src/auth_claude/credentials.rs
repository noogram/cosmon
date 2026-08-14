// SPDX-License-Identifier: AGPL-3.0-only

//! Write `~/.claude/.credentials.json` in the format `claude` CLI
//! expects, and read it back to say whether a worker could actually
//! use it. Schema observed by reverse-engineering the macOS Keychain
//! store (`Claude Code-credentials`, 2026-05-19). See ADR-0017 §6
//! (Q-impl-2 RESOLVED) for documentation.
//!
//! # Why this module also *reads*
//!
//! `GET /v1/auth/me` used to answer the worker-glasses question with
//! `path.exists()`. A file that exists but holds `{}`, or a token that
//! expired months ago, reported `claude_credentials_present: true`
//! while the worker refused to start — the signal confirmed the wrong
//! hypothesis at the exact moment someone was hunting the real cause
//! (issue #48). [`classify_credentials_file`] replaces the existence
//! check with the necessary conditions the worker itself needs, and
//! names which one failed.

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

/// What a worker would find if it tried to use the credentials file
/// right now.
///
/// The variants are the *decidable* preconditions — everything the
/// adapter can establish from the artifact alone, without spending an
/// Anthropic round-trip. They are necessary, not sufficient: a
/// [`CredentialsVerdict::Usable`] token can still be revoked upstream.
/// That residual uncertainty is why the wire field stays a
/// worker-glasses *hint* and never becomes an entitlement claim.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CredentialsVerdict {
    /// No file at the configured path — `auth login` was never
    /// completed (or the file was deleted).
    Absent,
    /// The path exists but could not be read (permissions, a
    /// directory, an I/O error). Existence alone told the old probe
    /// "present"; a worker gets nothing from it.
    Unreadable,
    /// The bytes are not JSON, or carry no `claudeAiOauth.accessToken`
    /// string, or that string is empty. The canonical `{}` placeholder
    /// lands here.
    Malformed,
    /// `accessToken` is past its `expiresAt` and no `refreshToken` is
    /// stored — the worker has nothing left to present or to renew
    /// with.
    Expired,
    /// `accessToken` is past its `expiresAt` but a `refreshToken` is
    /// stored, so the `claude` CLI renews it on first use. Reported as
    /// usable: refusing here would trade one false verdict for its
    /// mirror image.
    Refreshable,
    /// Every locally decidable precondition holds.
    Usable,
}

impl CredentialsVerdict {
    /// Whether a worker can start with these credentials, as far as
    /// the adapter can tell. This is what the `/v1/auth/me`
    /// worker-glasses boolean reports.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Refreshable | Self::Usable)
    }

    /// Stable wire label, published as `claude_credentials_status` so
    /// a client can say *which* precondition failed instead of
    /// guessing from a bare `false`. Snake-case, additive-only.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Unreadable => "unreadable",
            Self::Malformed => "malformed",
            Self::Expired => "expired",
            Self::Refreshable => "refreshable",
            Self::Usable => "usable",
        }
    }
}

/// Classify already-read credential bytes against a caller-supplied
/// clock (Unix epoch **milliseconds**, matching the file's `expiresAt`).
///
/// Split out from [`classify_credentials_file`] so the expiry branch is
/// testable without sleeping or mocking the process clock.
#[must_use]
pub fn classify_credentials_bytes(bytes: &[u8], now_ms: i64) -> CredentialsVerdict {
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return CredentialsVerdict::Malformed;
    };
    let Some(oauth) = parsed.get("claudeAiOauth") else {
        return CredentialsVerdict::Malformed;
    };
    let access_ok = oauth
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|t| !t.trim().is_empty());
    if !access_ok {
        return CredentialsVerdict::Malformed;
    }
    // A missing `expiresAt` is not treated as expired: some seeded
    // images write long-lived credentials without one, and the worker
    // starts fine with those. Only a value we can read *and* that is
    // in the past is evidence of expiry.
    let expired = oauth
        .get("expiresAt")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|exp| exp <= now_ms);
    if !expired {
        return CredentialsVerdict::Usable;
    }
    let refreshable = oauth
        .get("refreshToken")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|t| !t.trim().is_empty());
    if refreshable {
        CredentialsVerdict::Refreshable
    } else {
        CredentialsVerdict::Expired
    }
}

/// Classify the credentials file at `path` as of now.
///
/// Never returns an error: every failure mode *is* one of the
/// verdicts, because the caller's question ("could a worker start?")
/// has an answer in each case.
#[must_use]
pub fn classify_credentials_file(path: &Path) -> CredentialsVerdict {
    match std::fs::read(path) {
        Ok(bytes) => classify_credentials_bytes(&bytes, Utc::now().timestamp_millis()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CredentialsVerdict::Absent,
        Err(_) => CredentialsVerdict::Unreadable,
    }
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

    // ── Usability probe (issue #48) ───────────────────────────────
    //
    // The case that misled the issue's author: the file EXISTS, so the
    // old `path.exists()` probe said "present", while the worker
    // refused to start. Each test below is one shape of that lie.

    const NOW_MS: i64 = 1_779_451_200_000; // 2026-05-22T12:00:00Z

    fn creds_json(access: &str, expires_at: i64, refresh: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "claudeAiOauth": {
                "accessToken": access,
                "refreshToken": refresh,
                "expiresAt": expires_at,
                "scopes": ["user:inference"],
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_written_credentials_file_is_usable() {
        let bytes = render_credentials(&sample_response()).unwrap();
        assert_eq!(
            classify_credentials_bytes(&bytes, Utc::now().timestamp_millis()),
            CredentialsVerdict::Usable
        );
    }

    #[test]
    fn empty_object_is_malformed_not_present() {
        assert_eq!(
            classify_credentials_bytes(b"{}", NOW_MS),
            CredentialsVerdict::Malformed
        );
        assert!(!CredentialsVerdict::Malformed.is_usable());
    }

    #[test]
    fn truncated_or_blank_token_is_malformed() {
        assert_eq!(
            classify_credentials_bytes(b"{\"claudeAiOauth\": {", NOW_MS),
            CredentialsVerdict::Malformed
        );
        assert_eq!(
            classify_credentials_bytes(&creds_json("", NOW_MS + 1, "r"), NOW_MS),
            CredentialsVerdict::Malformed
        );
    }

    #[test]
    fn expired_without_refresh_token_is_expired() {
        let bytes = creds_json("sk-ant-oat01-old", NOW_MS - 1, "");
        assert_eq!(
            classify_credentials_bytes(&bytes, NOW_MS),
            CredentialsVerdict::Expired
        );
        assert!(!CredentialsVerdict::Expired.is_usable());
    }

    #[test]
    fn expired_with_refresh_token_stays_usable() {
        let bytes = creds_json("sk-ant-oat01-old", NOW_MS - 1, "sk-ant-ort01-new");
        assert_eq!(
            classify_credentials_bytes(&bytes, NOW_MS),
            CredentialsVerdict::Refreshable
        );
        assert!(CredentialsVerdict::Refreshable.is_usable());
    }

    #[test]
    fn missing_expiry_is_not_read_as_expired() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "claudeAiOauth": { "accessToken": "sk-ant-oat01-seeded" }
        }))
        .unwrap();
        assert_eq!(
            classify_credentials_bytes(&bytes, NOW_MS),
            CredentialsVerdict::Usable
        );
    }

    #[test]
    fn absent_file_is_absent_and_a_directory_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            classify_credentials_file(&dir.path().join("nope.json")),
            CredentialsVerdict::Absent
        );
        // A directory at the credentials path exists, so the old probe
        // reported `true`; reading it fails.
        assert_eq!(
            classify_credentials_file(dir.path()),
            CredentialsVerdict::Unreadable
        );
    }

    #[test]
    fn wire_labels_are_stable() {
        assert_eq!(CredentialsVerdict::Absent.as_wire(), "absent");
        assert_eq!(CredentialsVerdict::Unreadable.as_wire(), "unreadable");
        assert_eq!(CredentialsVerdict::Malformed.as_wire(), "malformed");
        assert_eq!(CredentialsVerdict::Expired.as_wire(), "expired");
        assert_eq!(CredentialsVerdict::Refreshable.as_wire(), "refreshable");
        assert_eq!(CredentialsVerdict::Usable.as_wire(), "usable");
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
