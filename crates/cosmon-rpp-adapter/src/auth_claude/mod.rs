// SPDX-License-Identifier: AGPL-3.0-only

//! `auth_claude` — API-mediated Claude Code OAuth (PKCE Authorization Code
//! with manual paste-back). Honors the `no-direct-shell` invariant
//! introduced by [ADR-0017] (smithy galaxy).
//!
//! Five HTTP endpoints, six states, no polling. The operator drives the
//! flow from outside the container; the container holds the PKCE
//! `code_verifier` privately, builds an Anthropic authorize URL, waits
//! for the operator to paste back the `authorization_code` retrieved
//! from Anthropic's manual-redirect page, then exchanges the code for
//! a `(access_token, refresh_token)` pair which is written to
//! `~/.claude/.credentials.json` (chmod 0600).
//!
//! Surface (mounted by [`crate::router`] when `AppState::auth_claude`
//! is `Some`):
//!
//! - `POST /v1/auth/claude/start` — create session
//! - `POST /v1/auth/claude/email` — submit email, build authorize URL
//! - `GET /v1/auth/claude/{session_id}` — read session view
//! - `POST /v1/auth/claude/confirm` — paste authorization_code, exchange
//! - `DELETE /v1/auth/claude/{session_id}` — cancel + cleanup
//!
//! Protocol spec: `smithy/docs/specs/auth-claude-api-protocol-v1.md`
//! (v1.1, PKCE pivot). OpenAPI: `auth-claude-api.openapi.yaml`.
//!
//! [ADR-0017]: ../../../../../galaxies/smithy/docs/adr/0017-api-mediated-claude-auth-device-code-flow.md

#![allow(clippy::missing_errors_doc)]
// Several module-doc paragraphs reference upstream OAuth/PKCE
// identifiers (e.g. `code_verifier`, `code_challenge`, `client_id`)
// that are RFC-canonical and not Rust items. Backticking them all
// would clutter the prose; suppress the lint module-wide.
#![allow(clippy::doc_markdown)]
// `AuthClaudeState::new` documents the lazy `reqwest::Client` build
// whose `.expect` cannot fire with default config — the surface is
// noisier than the warning is worth.
#![allow(clippy::missing_panics_doc)]
// `routes::confirm` is the canonical token-exchange handler — its
// linear narrative is intentionally long (load, TTL, state guard,
// exchange, write credentials, transition, persist).
#![allow(clippy::too_many_lines)]

pub mod anthropic;
pub mod config;
pub mod credentials;
pub mod pkce;
pub mod routes;
pub mod state;
pub mod store;

use std::sync::Arc;

pub use config::AuthClaudeConfig;
pub use state::{AuthSession, AuthState as SessionState};
pub use store::{FilesystemSessionStore, SessionStore};

/// Shared per-adapter state for the auth-claude surface. Held inside
/// [`crate::AppState::auth_claude`] as an `Option` — when `None`, the
/// 5 endpoints return `503 service_unavailable`.
#[derive(Debug, Clone)]
pub struct AuthClaudeState {
    /// Resolved upstream Anthropic + local config (URLs, client_id,
    /// scopes, TTL, credentials path).
    pub config: Arc<AuthClaudeConfig>,
    /// Disk-backed session store under
    /// `<state_dir>/auth-sessions/<session_id>.json`.
    pub store: Arc<dyn SessionStore>,
    /// Outbound HTTP client used to exchange `authorization_code` for
    /// tokens at the Anthropic token endpoint. Stored on the adapter
    /// (not per-request) so connection pooling is preserved.
    pub http: reqwest::Client,
}

impl AuthClaudeState {
    /// Build the auth-claude adapter state. `config` and `store` are
    /// pre-built by the caller (the binary's `main` constructs them
    /// from the operator config; tests can substitute an in-memory
    /// store).
    #[must_use]
    pub fn new(config: AuthClaudeConfig, store: Arc<dyn SessionStore>) -> Self {
        Self {
            config: Arc::new(config),
            store,
            http: reqwest::Client::builder()
                .user_agent("cosmon-rpp-adapter/auth-claude")
                .build()
                .expect("reqwest client build cannot fail with default config"),
        }
    }

    /// Variant of [`Self::new`] that accepts an externally-built HTTP
    /// client — used by integration tests to point the client at a
    /// mock Anthropic server.
    #[must_use]
    pub fn with_http(
        config: AuthClaudeConfig,
        store: Arc<dyn SessionStore>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            config: Arc::new(config),
            store,
            http,
        }
    }
}
