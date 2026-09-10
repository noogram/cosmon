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
//! the verifier, derives the S256 challenge, runs a callback redirect catcher,
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
//! - [`callback`] — the bind-before-browser redirect catcher (C7).
//! - [`exchange`] — the code and refresh token grants (C2).
//! - [`flow`] — [`login`] / [`ensure_token`] / [`refresh_credential`] /
//!   [`force_refresh`] / [`logout`] (C2, C6, C7).
//!
//! # Opening the authorize URL
//!
//! [`login`] takes the opener as a parameter, so *what* opens the URL is not
//! this module's decision. [`Opener`] resolves that decision once, from the
//! environment: the system browser by default, or the command named by
//! `$COSMON_REMOTE_BROWSER`. That second form is what makes the flow
//! scriptable — a headless smoke drives it with `curl -L` against an
//! auto-approving IdP and never needs a browser at all.

pub mod callback;
pub mod discovery;
pub mod error;
pub mod exchange;
pub mod flow;
pub mod pkce_s256;

pub use callback::{
    parse_callback_target, redirect_uri, CallbackParams, CallbackServer, CALLBACK_PATH,
    DEFAULT_REDIRECT_PORT, LOOPBACK_IP,
};
pub use discovery::{ClientRegistry, OAuthClient, ProviderMetadata, CLIENT_REGISTRY_SCHEMA};
pub use error::OidcError;
pub use exchange::TokenResponse;
pub use flow::{
    bearer_identity, build_authorize_url, cached_access, discover, ensure_token, force_refresh,
    login, logout, refresh_credential, BearerIdentity, CacheState, LoginOutcome, OidcEndpoints,
    RefreshConfig, RefreshRotation, TokenState, LOGIN_TIMEOUT_SECS, REFRESH_LEEWAY_SECS,
};
pub use pkce_s256::{CodeVerifier, Nonce};

/// The environment variable naming an external command to open the authorize
/// URL, in place of the system browser.
///
/// The value is a command line: the first whitespace-separated token is the
/// program, the rest are leading arguments, and the authorize URL is appended
/// as the final argument. It is **not** a shell line — there is no quoting, no
/// globbing, and no `$VAR` expansion, because there is no shell.
///
/// This is the whole non-browser seam. A container smoke drives the login with
/// `COSMON_REMOTE_BROWSER='curl -sS -L -o /dev/null'`: `curl` follows the
/// auto-approving IdP's 302 into the loopback callback this process is already
/// listening on, and the flow completes with no display, no browser, and no
/// second CLI surface to keep in step.
pub const BROWSER_COMMAND_ENV: &str = "COSMON_REMOTE_BROWSER";

/// How this invocation opens the authorize URL.
///
/// Resolved **once, before `login` runs**, so a malformed
/// `$COSMON_REMOTE_BROWSER` is refused up front rather than at the moment the
/// browser should have opened — by then the listener is bound, the operator is
/// staring at nothing, and the only remaining event is the five-minute timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opener {
    /// The default: hand the URL to the platform's default browser
    /// ([`open_browser`]).
    SystemBrowser,
    /// Spawn this argv, with the authorize URL appended as the last argument.
    /// Non-empty by construction — [`Opener::from_raw`] refuses an empty one.
    Spawn(Vec<String>),
}

impl Opener {
    /// Resolve the opener from the process environment
    /// ([`BROWSER_COMMAND_ENV`]).
    ///
    /// # Errors
    ///
    /// [`OidcError::BrowserCommand`] when the variable is set but names no
    /// command.
    pub fn from_env() -> crate::error::Result<Self> {
        Self::from_raw(std::env::var(BROWSER_COMMAND_ENV).ok().as_deref())
    }

    /// The pure half of [`Opener::from_env`]: decide from a raw variable value
    /// rather than from the live environment, so the decision is unit-testable
    /// without mutating a process-global.
    ///
    /// `None` (unset) is the system browser. A set-but-blank value is an
    /// **error**, not a silent fallback: an operator who exported the variable
    /// asked for a command, and quietly opening a browser instead would be a
    /// different login than the one they requested.
    ///
    /// # Errors
    ///
    /// [`OidcError::BrowserCommand`] when `raw` is `Some` but contains no
    /// non-whitespace token.
    pub fn from_raw(raw: Option<&str>) -> crate::error::Result<Self> {
        let Some(command_line) = raw else {
            return Ok(Self::SystemBrowser);
        };
        let argv: Vec<String> = command_line.split_whitespace().map(str::to_owned).collect();
        if argv.is_empty() {
            return Err(OidcError::BrowserCommand {
                var: BROWSER_COMMAND_ENV,
            }
            .into());
        }
        Ok(Self::Spawn(argv))
    }

    /// Open `url`, by whichever route this opener names.
    ///
    /// The URL is always printed to stderr first — it is the safety net when
    /// the browser does not appear, and the thing a script's log needs when the
    /// spawned command misbehaves. The URL carries the `state` and the
    /// `code_challenge`, both public by design (RFC 7636 §4.2: the challenge is
    /// a digest, and the verifier it protects never leaves this process).
    ///
    /// Never fails and never blocks: the spawned command is **not** waited on.
    /// It cannot be — a `curl` that follows the redirect only returns once this
    /// process has answered the loopback callback, which happens after `open`
    /// returns. Waiting here would deadlock the login it is trying to drive.
    pub fn open(&self, url: &str) {
        match self {
            Self::SystemBrowser => open_browser(url),
            Self::Spawn(argv) => spawn_opener(argv, url),
        }
    }
}

/// Spawn `argv` with `url` appended, reporting a failure on stderr rather than
/// swallowing it. `argv` is non-empty by [`Opener::from_raw`]'s construction;
/// an empty one would be a caller-built value, and is reported the same way
/// instead of panicking.
fn spawn_opener(argv: &[String], url: &str) {
    eprintln!("\n  Opening the sign-in URL with `{}`:\n", argv.join(" "));
    eprintln!("    {url}\n");
    let Some((program, leading_args)) = argv.split_first() else {
        eprintln!("  (${BROWSER_COMMAND_ENV} named no command — open the URL above)\n");
        return;
    };
    let spawned = std::process::Command::new(program)
        .args(leading_args)
        .arg(url)
        .spawn();
    if let Err(err) = spawned {
        eprintln!("  (could not run `{program}`: {err} — open the URL above)\n");
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn an_unset_variable_means_the_system_browser() {
        assert_eq!(Opener::from_raw(None).unwrap(), Opener::SystemBrowser);
    }

    #[test]
    fn a_command_line_splits_into_program_and_leading_args() {
        // The URL is appended by `open`, never baked into the argv here: a
        // value that already contained the URL would open the wrong login.
        assert_eq!(
            Opener::from_raw(Some("curl -sS -L -o /dev/null")).unwrap(),
            Opener::Spawn(
                ["curl", "-sS", "-L", "-o", "/dev/null"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            )
        );
    }

    #[test]
    fn a_set_but_blank_variable_is_refused_rather_than_silently_ignored() {
        // The failure mode this guards: falling back to the browser would make
        // `login` hang on the callback for five minutes with nothing said.
        for raw in ["", "   ", "\t\n"] {
            let err = Opener::from_raw(Some(raw)).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::Oidc(OidcError::BrowserCommand {
                        var: BROWSER_COMMAND_ENV
                    })
                ),
                "{raw:?}: expected BrowserCommand, got {err:?}"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_does_not_produce_an_empty_argument() {
        assert_eq!(
            Opener::from_raw(Some("  xdg-open  ")).unwrap(),
            Opener::Spawn(vec!["xdg-open".to_owned()])
        );
    }
}
