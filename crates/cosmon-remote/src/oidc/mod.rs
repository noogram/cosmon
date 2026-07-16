// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

//! The user↔cosmon OAuth2-PKCE login and silent-refresh flow
//! (delib-20260710-33b7, Child 2).
//!
//! # Why this exists (and why it is NOT [`crate::pkce`])
//!
//! `cosmon-remote` obtains a user↔cosmon bearer JWT from Forgejo (the IdP).
//! Forgejo issues **15-minute** access tokens and **single-use, ~30-day** refresh
//! tokens. Without persistence that would mean a full browser flow every quarter
//! hour; with `{access, refresh}` persisted (via [`crate::credential`]) it is a
//! monthly browser login and a silent refresh every 15 minutes.
//!
//! This is a **different flow** from [`crate::pkce`], which is the
//! Claude/Anthropic manual-paste device flow (`/v1/auth/claude/*`) where the
//! PKCE crypto lives on the server. Here the CLI *is* the OAuth client: it mints
//! the verifier, derives the S256 challenge, runs a loopback redirect catcher,
//! and exchanges the code itself. Keeping the two apart — distinct module,
//! distinct error type ([`OidcError`], never `Error::Auth`) — is a load-bearing
//! part of the contract: the brief calls out by name the confusion of reusing
//! the Claude flow's helpers.
//!
//! # The seven-step login and the refresh protocol
//!
//! [`login`] runs discovery → PKCE-gen → **bind-before-browser** → browser →
//! catch-code-and-verify-state → exchange → persist. [`ensure_token`] is the
//! silent-refresh seam every authenticated command hits: zero network when the
//! access token is valid, and a single-writer refresh (advisory lock +
//! compare-and-swap + adopt-winner + persist-before-use) when it is not. See
//! [`flow`] for the full protocol write-up.
//!
//! # Module map
//!
//! - [`error`] — [`OidcError`] (C4).
//! - [`pkce_s256`] — the verifier / S256 challenge / CSRF nonces (C7).
//! - [`discovery`] — OIDC metadata + the cosmon `client_id` registry (C8).
//! - [`loopback`] — the bind-before-browser redirect catcher (C7).
//! - [`exchange`] — the code and refresh token grants (C2).
//! - [`flow`] — [`login`] / [`ensure_token`] / [`refresh_credential`] /
//!   [`force_refresh`] / [`logout`] (C2, C6, C7).

pub mod discovery;
pub mod error;
pub mod exchange;
pub mod flow;
pub mod loopback;
pub mod pkce_s256;

pub use discovery::{ClientRegistry, OAuthClient, ProviderMetadata, CLIENT_REGISTRY_SCHEMA};
pub use error::OidcError;
pub use exchange::TokenResponse;
pub use flow::{
    build_authorize_url, cached_access, discover, ensure_token, force_refresh, login, logout,
    refresh_credential, CacheState, LoginOutcome, OidcEndpoints, RefreshConfig, RefreshRotation,
    TokenState, LOGIN_TIMEOUT_SECS, REFRESH_LEEWAY_SECS,
};
pub use loopback::{
    parse_callback_target, redirect_uri, validate_loopback_redirect_uri, CallbackParams,
    LoopbackServer, CALLBACK_PATH, DEFAULT_REDIRECT_PORT, LOOPBACK_IP,
};
pub use pkce_s256::{CodeVerifier, Nonce};

/// Best-effort open the operator's default browser at `url`, and always print
/// the URL to stderr as a fallback (a headless box, an SSH session, or a
/// spawn failure). Never fails the flow — the printed URL is the safety net.
///
/// This is the production `open` closure handed to [`login`]; tests inject their
/// own to drive the callback without a browser.
pub fn open_browser(url: &str) {
    eprintln!("\n  Opening your browser to sign in. If it does not open, visit:\n");
    eprintln!("    {url}\n");
    let spawned = browser_command(url).map(|mut c| c.spawn());
    if !matches!(spawned, Some(Ok(_))) {
        eprintln!("  (could not launch a browser automatically — open the URL above)\n");
    }
}

/// The platform command that opens a URL in the default browser, or `None` on an
/// unsupported target (the caller then relies on the printed URL). The `Option`
/// is meaningful only on non-unix / non-windows targets; clippy sees a single
/// active `cfg` and reads it as always-`Some`, hence the allow.
#[allow(clippy::unnecessary_wraps)]
fn browser_command(url: &str) -> Option<std::process::Command> {
    #[cfg(target_os = "macos")]
    {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        Some(c)
    }
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        Some(c)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        Some(c)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = url;
        None
    }
}
