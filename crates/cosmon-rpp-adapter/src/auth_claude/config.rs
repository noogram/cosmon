// SPDX-License-Identifier: AGPL-3.0-only

//! Resolved configuration for the auth-claude surface. Carries the
//! upstream Anthropic OAuth endpoints, the `client_id` discovered by
//! reverse-engineering the `claude` CLI, the scope set, the session
//! TTL, and the local path where `~/.claude/.credentials.json` should
//! be written.

use std::path::PathBuf;
use std::time::Duration;

/// Discovered OAuth client_id for the `claude` CLI (constant string
/// embedded in the binary, v2.1.144, 2026-05-19). Documented in
/// `smithy/docs/adr/0017-...` §6 (Q-impl-1 RESOLVED).
pub const DEFAULT_CLAUDE_CLI_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Default Anthropic authorize URL (Claude.ai SSO path).
pub const DEFAULT_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";

/// Default Anthropic token exchange endpoint.
pub const DEFAULT_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// Default redirect URI — Anthropic's manual-redirect page that shows
/// the `authorization_code` for paste-back in headless mode.
pub const DEFAULT_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";

/// Default scope set (space-separated, ordered as observed in the CLI).
pub const DEFAULT_SCOPES: &str = "org:create_api_key user:profile user:inference \
                                   user:sessions:claude_code user:mcp_servers user:file_upload";

/// Default session TTL — spec §4.3 (15 minutes).
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(15 * 60);

/// Default credentials write path inside the container. The path is
/// resolved relative to `$HOME` of the user running the adapter; in a
/// guest-slice container that user is `cosmon` (uid 1000).
pub const DEFAULT_CREDENTIALS_RELATIVE_PATH: &str = ".claude/.credentials.json";

/// Resolved auth-claude config. All fields are mandatory at runtime;
/// builder constants above provide defaults.
#[derive(Debug, Clone)]
pub struct AuthClaudeConfig {
    /// OAuth `client_id` used in the authorize + token requests.
    pub client_id: String,
    /// Anthropic authorize URL (base, sans query parameters).
    pub authorize_url: String,
    /// Anthropic token exchange URL.
    pub token_url: String,
    /// Registered redirect URI (must exactly match what Anthropic
    /// expects for this `client_id`).
    pub redirect_uri: String,
    /// Space-separated scope list (per RFC 6749 §3.3).
    pub scopes: String,
    /// Session TTL — bounds `code_verifier` lifetime.
    pub session_ttl: Duration,
    /// Absolute filesystem path where `.credentials.json` is written
    /// on successful token exchange.
    pub credentials_path: PathBuf,
}

impl AuthClaudeConfig {
    /// Construct a config from the default constants, with the
    /// credentials path resolved against the supplied home directory.
    /// This is the path used by the binary's `main` (where `home` =
    /// `$HOME`) and by tests (where `home` is a temp dir).
    #[must_use]
    pub fn defaults_with_home(home: &std::path::Path) -> Self {
        Self {
            client_id: DEFAULT_CLAUDE_CLI_CLIENT_ID.to_owned(),
            authorize_url: DEFAULT_AUTHORIZE_URL.to_owned(),
            token_url: DEFAULT_TOKEN_URL.to_owned(),
            redirect_uri: DEFAULT_REDIRECT_URI.to_owned(),
            scopes: DEFAULT_SCOPES.to_owned(),
            session_ttl: DEFAULT_SESSION_TTL,
            credentials_path: home.join(DEFAULT_CREDENTIALS_RELATIVE_PATH),
        }
    }

    /// Override the token URL (test hook — points at a mock server).
    #[must_use]
    pub fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = url.into();
        self
    }

    /// Override the authorize URL (test hook).
    #[must_use]
    pub fn with_authorize_url(mut self, url: impl Into<String>) -> Self {
        self.authorize_url = url.into();
        self
    }
}
