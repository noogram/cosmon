// SPDX-License-Identifier: AGPL-3.0-only

//! The loopback redirect catcher (delib-20260710-33b7 C7).
//!
//! The authorization-code flow needs a `redirect_uri` the browser can reach
//! after the operator signs in. For a native CLI that is a loopback listener:
//! the client binds `127.0.0.1:<port>`, the authorize URL names
//! `http://127.0.0.1:<port>/callback` as its `redirect_uri`, and after consent
//! the browser lands there with `?code=…&state=…`.
//!
//! Two invariants the contract makes load-bearing:
//!
//! - **Bind before browser.** [`LoopbackServer::bind`] must succeed *before* the
//!   authorize URL is opened. If the port is taken we fail fast with a precise
//!   error, rather than opening a browser that will redirect into the void.
//! - **The IP literal, not `localhost`.** We bind and advertise `127.0.0.1`
//!   verbatim. `localhost` can resolve to `::1` (or a hosts-file surprise); the
//!   `redirect_uri` registered with Forgejo is the literal, so binding anything
//!   else silently mismatches.
//! - **Accept in a loop, terminate only on proof of `state`.** A single
//!   `accept()` is a trap: a browser preconnect, a speculative `GET /favicon.ico`,
//!   or a cross-origin `fetch("http://127.0.0.1:7777/callback?error=x")` from a
//!   page open during login would each consume the one accept slot — parking or
//!   failing the flow while the genuine redirect lands on a second, never-serviced
//!   socket (login hang / spurious failure / on-demand DoS; review
//!   `task-20260710-a6ae` F2). [`LoopbackServer::accept`] therefore loops,
//!   answering and discarding every non-matching request, and returns **only** on
//!   a `GET /callback` whose `state` verifies against the per-flow [`Nonce`] (or
//!   on the overall timeout). Because only a request that echoes the high-entropy
//!   `state` can end the wait, an origin that does not know it cannot preempt the
//!   catcher. Each connection is also read under a short per-connection cap so a
//!   silent preconnect cannot wedge the loop.
//!
//! The impure I/O ([`LoopbackServer`]) is kept thin; the parsing of the HTTP
//! request target into a [`CallbackParams`] is a **pure function**
//! ([`parse_callback_target`]) so it can be unit- and property-tested without a
//! socket. The `/callback` path and `GET` method are asserted before a request
//! is treated as a callback.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use super::error::OidcError;
use super::pkce_s256::Nonce;
use crate::error::Result;

/// The loopback IP the client binds and advertises. The literal `127.0.0.1`,
/// never `localhost` — the `redirect_uri` registered with the provider is the
/// literal, and `localhost` may resolve to `::1`.
pub const LOOPBACK_IP: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// The default redirect port. The `cs-rpp-adapter` OAuth app is provisioned with
/// `http://127.0.0.1:7777/callback` as an allowed `redirect_uri` (a signal the
/// loopback flow was intended).
pub const DEFAULT_REDIRECT_PORT: u16 = 7777;

/// The path component of the redirect URI.
pub const CALLBACK_PATH: &str = "/callback";

/// The only HTTP method a genuine browser redirect uses. Anything else
/// (`HEAD`/`POST`/preconnect probes) is not the callback and is ignored.
pub const CALLBACK_METHOD: &str = "GET";

/// Upper bound on how long a *single* accepted connection may take to deliver
/// its request line before the loop abandons it and returns to `accept()`. A
/// browser preconnect opens a socket and may send nothing; without this cap it
/// would park the loop while the real redirect waits on the next socket. Kept
/// well below any realistic overall login timeout so it only ever trims dead
/// sockets, never a live-but-slow redirect.
const PER_CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Build the exact `redirect_uri` string for `port`. This must match, byte for
/// byte, what is registered with the provider (exact-match redirect).
pub fn redirect_uri(port: u16) -> String {
    format!("http://{LOOPBACK_IP}:{port}{CALLBACK_PATH}")
}

/// Validate that a `redirect_uri` discovered from the client registry is a
/// **loopback** redirect the client may safely bind and advertise: the scheme
/// must be `http` and the host must be an IP *loopback literal* (`127.0.0.0/8`
/// or `::1`).
///
/// This is the guard against a spoofed or compromised
/// `.well-known/cosmon-oauth-clients` document (review `task-20260710-a6ae` F1).
/// Without it, [`crate::oidc::discover`] would advertise an attacker-controlled
/// `redirect_uri` to the authorization server while binding the loopback
/// listener locally — steering the authorization code toward a host the operator
/// never intended (`https://evil.example/callback`), an authorization-code
/// exfiltration path. RFC 8252 §7.3 (OAuth 2.0 for Native Apps) restricts the
/// loopback redirection to exactly these IP literals.
///
/// `localhost` is deliberately **rejected**, not just `http`-required: it can
/// resolve to a hosts-file surprise or `::1`, and the whole loopback contract is
/// built on the IP literal (see the module header). A registry that wants the
/// loopback default should simply omit `redirect_uri`.
pub fn validate_loopback_redirect_uri(redirect_uri: &str) -> Result<()> {
    let url = url::Url::parse(redirect_uri).map_err(|e| OidcError::Discovery {
        reason: format!("redirect_uri {redirect_uri:?} is not a valid URL: {e}"),
    })?;
    if url.scheme() != "http" {
        return Err(OidcError::Discovery {
            reason: format!(
                "redirect_uri {redirect_uri:?} must use the http scheme for a loopback \
                 redirect (got scheme {:?}) — refusing a non-loopback redirect from the \
                 client registry",
                url.scheme()
            ),
        }
        .into());
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => Ok(()),
        Some(url::Host::Ipv6(ip)) if ip.is_loopback() => Ok(()),
        other => Err(OidcError::Discovery {
            reason: format!(
                "redirect_uri {redirect_uri:?} must target a loopback IP literal \
                 (127.0.0.1 or [::1]), got host {other:?} — refusing a non-loopback \
                 redirect from the client registry"
            ),
        }
        .into()),
    }
}

/// What the browser handed back on the loopback callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackParams {
    /// The authorization `code` to exchange for tokens.
    pub code: String,
    /// The `state` value the server echoed back — to be compared against the
    /// per-flow [`Nonce`] before the code is trusted.
    pub state: String,
}

/// The callback query parameters we care about, extracted from a request target
/// whose path has already been asserted to be [`CALLBACK_PATH`].
#[derive(Debug, Default)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Parse a request target (`/callback?…`) into its callback query parameters,
/// **asserting the path is [`CALLBACK_PATH`]** first. A target on any other path
/// (`/favicon.ico`, `/`, …) is not a callback and yields [`OidcError::Callback`].
fn read_callback_query(target: &str) -> Result<CallbackQuery> {
    // Parse against a dummy base so a relative target (`/callback?…`) yields a
    // path + query we can inspect. The base host is irrelevant — only the path
    // and query are read.
    let url = url::Url::parse("http://127.0.0.1")
        .and_then(|base| base.join(target))
        .map_err(|e| OidcError::Callback {
            reason: format!("unparseable callback target {target:?}: {e}"),
        })?;

    if url.path() != CALLBACK_PATH {
        return Err(OidcError::Callback {
            reason: format!(
                "callback arrived on unexpected path {:?} (expected {CALLBACK_PATH})",
                url.path()
            ),
        }
        .into());
    }

    let mut q = CallbackQuery::default();
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => q.code = Some(v.into_owned()),
            "state" => q.state = Some(v.into_owned()),
            "error" => q.error = Some(v.into_owned()),
            "error_description" => q.error_description = Some(v.into_owned()),
            _ => {}
        }
    }
    Ok(q)
}

/// Parse the target of the callback HTTP request line (e.g.
/// `/callback?code=abc&state=xyz`) into a [`CallbackParams`].
///
/// This is the **pure** heart of the catcher — no socket, no runtime. It
/// asserts the request path is [`CALLBACK_PATH`], and maps an OAuth error
/// redirect (`?error=access_denied&error_description=…`) to [`OidcError::Server`],
/// so a refused consent surfaces as the right error rather than a bare
/// "missing code".
///
/// ```
/// use cosmon_remote::oidc::parse_callback_target;
/// let p = parse_callback_target("/callback?code=abc&state=xyz").unwrap();
/// assert_eq!(p.code, "abc");
/// assert_eq!(p.state, "xyz");
/// ```
pub fn parse_callback_target(target: &str) -> Result<CallbackParams> {
    let q = read_callback_query(target)?;

    if let Some(error) = q.error {
        return Err(OidcError::Server {
            error,
            description: q.error_description,
        }
        .into());
    }

    match (q.code, q.state) {
        (Some(code), Some(state)) => Ok(CallbackParams { code, state }),
        (None, _) => Err(OidcError::Callback {
            reason: "callback carried no `code` parameter".to_owned(),
        }
        .into()),
        (_, None) => Err(OidcError::Callback {
            reason: "callback carried no `state` parameter".to_owned(),
        }
        .into()),
    }
}

/// Split an HTTP request line (`GET /callback?code=… HTTP/1.1`) into its method
/// and request target (the first two whitespace-separated tokens).
fn parse_request_line(request_line: &str) -> Result<(&str, &str)> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or_else(|| OidcError::Callback {
        reason: format!("malformed HTTP request line: {request_line:?}"),
    })?;
    let target = parts.next().ok_or_else(|| OidcError::Callback {
        reason: format!("malformed HTTP request line: {request_line:?}"),
    })?;
    Ok((method, target))
}

/// Classification of one accepted request inside the [`LoopbackServer::accept`]
/// loop. Only the two terminating variants end the wait; `Ignore` means answer
/// politely and keep listening for the genuine redirect.
enum Accepted {
    /// A `GET /callback` with a `code` and a `state` that verifies — the flow
    /// completes with these params.
    Callback(CallbackParams),
    /// A `GET /callback` OAuth error redirect (`?error=…`) whose `state` verifies
    /// — a genuine authorization-server refusal, surfaced as a terminal error.
    OauthError(crate::Error),
    /// Anything else — a preconnect, a favicon probe, a wrong method/path, or a
    /// request whose `state` does not verify (a cross-origin/attacker preempt).
    /// Not allowed to end the flow.
    Ignore,
}

/// Decide what an accepted request means, **verifying `state` before any
/// request is allowed to terminate the flow**. Because only a request that
/// echoes the high-entropy `state` can produce `Callback` or `OauthError`, an
/// origin that does not know `state` (a preconnect, a favicon fetch, a
/// cross-origin `fetch`) can never preempt the catcher — it always maps to
/// [`Accepted::Ignore`].
fn classify_request(request_line: &str, expected_state: &Nonce) -> Accepted {
    let Ok((method, target)) = parse_request_line(request_line) else {
        return Accepted::Ignore;
    };
    if method != CALLBACK_METHOD {
        return Accepted::Ignore;
    }
    // Wrong path (favicon, `/`, …) → not a callback.
    let Ok(q) = read_callback_query(target) else {
        return Accepted::Ignore;
    };

    // An OAuth error redirect only counts if it proves knowledge of `state`;
    // otherwise it is a cross-origin `?error=…` preempt attempt.
    if let Some(error) = q.error {
        return match q.state {
            Some(state) if expected_state.verify(&state) => Accepted::OauthError(
                OidcError::Server {
                    error,
                    description: q.error_description,
                }
                .into(),
            ),
            _ => Accepted::Ignore,
        };
    }

    match (q.code, q.state) {
        (Some(code), Some(state)) if expected_state.verify(&state) => {
            Accepted::Callback(CallbackParams { code, state })
        }
        // Missing code/state, or a state that does not verify: never terminates.
        _ => Accepted::Ignore,
    }
}

/// A bound loopback listener waiting for exactly one OAuth callback.
///
/// Construct with [`LoopbackServer::bind`] **before** opening the browser, then
/// `await` [`LoopbackServer::accept`] to capture the redirect.
pub struct LoopbackServer {
    listener: TcpListener,
    port: u16,
}

impl LoopbackServer {
    /// Bind `127.0.0.1:<port>`. Fails fast (a precise [`OidcError::Callback`]) if
    /// the port is already taken — this is the "bind before browser" gate. Pass
    /// port `0` to let the OS pick an ephemeral port (used by tests); read it
    /// back with [`LoopbackServer::port`].
    pub async fn bind(port: u16) -> Result<Self> {
        let addr = SocketAddr::new(IpAddr::V4(LOOPBACK_IP), port);
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| OidcError::Callback {
                reason: format!("could not bind {addr} for the OAuth redirect: {e}"),
            })?;
        let bound = listener.local_addr().map_err(|e| OidcError::Callback {
            reason: format!("could not read the bound loopback address: {e}"),
        })?;
        Ok(Self {
            listener,
            port: bound.port(),
        })
    }

    /// The port actually bound (meaningful after `bind(0)`).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The exact `redirect_uri` this server listens on.
    pub fn redirect_uri(&self) -> String {
        redirect_uri(self.port)
    }

    /// Accept connections **in a loop** until one is a `GET /callback` whose
    /// `state` verifies against `expected_state`, then return its params. Every
    /// other request — a browser preconnect, a `GET /favicon.ico`, a wrong
    /// method/path, or a cross-origin `?error=…` that cannot prove knowledge of
    /// `state` — is answered with a small HTML page and **discarded**, so it can
    /// never consume the flow's one chance (review `task-20260710-a6ae` F2).
    ///
    /// The whole loop is bounded by `timeout` (a browser that never returns does
    /// not hang the CLI forever), and each individual connection is read under
    /// `PER_CONNECTION_READ_TIMEOUT` so a silent preconnect cannot wedge it.
    ///
    /// A genuine authorization-server error redirect (`?error=…&state=<valid>`)
    /// terminates fast with [`OidcError::Server`]; a state that does not verify
    /// is treated as a preempt attempt and ignored (the flow keeps waiting for
    /// the real redirect rather than failing on a forged one).
    pub async fn accept(self, expected_state: &Nonce, timeout: Duration) -> Result<CallbackParams> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(no_redirect_error(timeout));
            }

            let Ok(accepted) = tokio::time::timeout(remaining, self.listener.accept()).await else {
                return Err(no_redirect_error(timeout));
            };
            let (mut stream, _peer) = accepted.map_err(|e| OidcError::Callback {
                reason: format!("accept on the loopback listener failed: {e}"),
            })?;

            // Bound each connection by whichever is smaller: the per-connection
            // cap (trims silent preconnects) or the flow's remaining budget.
            let per_conn = remaining.min(PER_CONNECTION_READ_TIMEOUT);
            let Some(request_line) = read_request_line(&mut stream, per_conn).await else {
                // No usable request line (silent preconnect, EOF, read timeout).
                // Drop the socket and go back to accepting the genuine redirect.
                continue;
            };

            match classify_request(&request_line, expected_state) {
                Accepted::Callback(params) => {
                    let _ = write_http_response(&mut stream, "200 OK", &success_page()).await;
                    return Ok(params);
                }
                Accepted::OauthError(err) => {
                    let _ = write_http_response(
                        &mut stream,
                        "200 OK",
                        &error_page("authorization failed"),
                    )
                    .await;
                    return Err(err);
                }
                Accepted::Ignore => {
                    // Not the redirect. Answer 404 and keep waiting; do NOT let
                    // this preempt the single flow (loop continues).
                    let _ =
                        write_http_response(&mut stream, "404 Not Found", &ignored_page()).await;
                }
            }
        }
    }
}

/// The timeout error surfaced when no genuine redirect arrives in time.
fn no_redirect_error(timeout: Duration) -> crate::Error {
    OidcError::Callback {
        reason: format!(
            "no OAuth redirect arrived within {}s — did the browser open?",
            timeout.as_secs()
        ),
    }
    .into()
}

/// Read one connection up to the end of its request headers (or a bounded cap),
/// under `per_conn`. Returns the HTTP request line (first line), or `None` if
/// the socket delivered nothing usable before EOF, error, or the cap — the
/// signature of a browser preconnect that must be ignored rather than treated
/// as a callback.
async fn read_request_line(stream: &mut TcpStream, per_conn: Duration) -> Option<String> {
    let deadline = Instant::now() + per_conn;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            // A read of >0 bytes extends the buffer; EOF (0), a read error, or
            // the per-connection cap all end this connection's read.
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => break,
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    match text.lines().next() {
        Some(line) if !line.is_empty() => Some(line.to_owned()),
        _ => None,
    }
}

/// Write a minimal HTTP/1.1 response carrying `body` as `text/html` with the
/// given `status` (e.g. `"200 OK"`, `"404 Not Found"`).
async fn write_http_response(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| OidcError::Callback {
            reason: format!("writing the callback response failed: {e}"),
        })?;
    let _ = stream.flush().await;
    Ok(())
}

/// The body returned to a non-callback request (preconnect, favicon,
/// cross-origin probe) that the loop ignores.
fn ignored_page() -> String {
    "<!doctype html><meta charset=utf-8><title>cosmon-remote</title>\
     <body>Not the OAuth callback.</body>"
        .to_owned()
}

fn success_page() -> String {
    "<!doctype html><meta charset=utf-8><title>cosmon-remote</title>\
     <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;text-align:center\">\
     <h1>✓ Signed in</h1><p>You can close this tab and return to the terminal.</p></body>"
        .to_owned()
}

fn error_page(reason: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>cosmon-remote</title>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;text-align:center\">\
         <h1>✗ Login failed</h1><p>{reason}. Return to the terminal for details.</p></body>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn redirect_uri_binds_the_ip_literal() {
        assert_eq!(redirect_uri(7777), "http://127.0.0.1:7777/callback");
        assert!(!redirect_uri(7777).contains("localhost"));
    }

    #[test]
    fn validate_accepts_loopback_ipv4_literal() {
        validate_loopback_redirect_uri("http://127.0.0.1:7777/callback").unwrap();
        // The built-in default must always pass its own guard.
        validate_loopback_redirect_uri(&redirect_uri(DEFAULT_REDIRECT_PORT)).unwrap();
        // Any port on the loopback interface is fine.
        validate_loopback_redirect_uri("http://127.0.0.1:9000/callback").unwrap();
        // The whole 127.0.0.0/8 block is loopback (RFC 8252 §7.3 lists 127.0.0.1,
        // but the block is reserved for loopback and is safe to bind).
        validate_loopback_redirect_uri("http://127.1.2.3:8080/callback").unwrap();
    }

    #[test]
    fn validate_accepts_loopback_ipv6_literal() {
        validate_loopback_redirect_uri("http://[::1]:7777/callback").unwrap();
    }

    #[test]
    fn validate_rejects_arbitrary_host() {
        // The core exploit: a spoofed registry naming an attacker host.
        let err = validate_loopback_redirect_uri("https://evil.example/callback").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn validate_rejects_public_ip_over_http() {
        // Non-TLS but a routable, non-loopback IP — still an exfiltration path.
        let err = validate_loopback_redirect_uri("http://8.8.8.8:7777/callback").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn validate_rejects_localhost_hostname() {
        // `localhost` is a name, not an IP literal — it can resolve to a
        // hosts-file surprise, so the contract advertises the literal only.
        let err = validate_loopback_redirect_uri("http://localhost:7777/callback").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn validate_rejects_non_http_scheme_on_loopback() {
        // Even pointed at loopback, a non-http scheme is not the loopback flow.
        let err = validate_loopback_redirect_uri("https://127.0.0.1:7777/callback").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn validate_rejects_unparseable_uri() {
        let err = validate_loopback_redirect_uri("not a url").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn validate_rejects_credentialed_loopback_authority() {
        // A userinfo@ prefix is a phishing/confusion vector even when the host
        // resolves to loopback; url parses the host as 127.0.0.1 here, so this
        // documents that the *host* is what we gate on, not the raw string.
        // `evil.example` as host (with 127.0.0.1 as userinfo) must be rejected.
        let err =
            validate_loopback_redirect_uri("http://127.0.0.1@evil.example/callback").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Discovery { .. })
        ));
    }

    #[test]
    fn parses_code_and_state() {
        let p = parse_callback_target("/callback?code=the-code&state=the-state").unwrap();
        assert_eq!(p.code, "the-code");
        assert_eq!(p.state, "the-state");
    }

    #[test]
    fn parses_url_encoded_values() {
        let p = parse_callback_target("/callback?code=a%2Fb%2Bc&state=x%3Dy").unwrap();
        assert_eq!(p.code, "a/b+c");
        assert_eq!(p.state, "x=y");
    }

    #[test]
    fn missing_code_is_a_callback_error() {
        let err = parse_callback_target("/callback?state=only").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Callback { .. })
        ));
    }

    #[test]
    fn missing_state_is_a_callback_error() {
        let err = parse_callback_target("/callback?code=only").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Callback { .. })
        ));
    }

    #[test]
    fn oauth_error_redirect_maps_to_server_error() {
        let err = parse_callback_target(
            "/callback?error=access_denied&error_description=The%20user%20refused",
        )
        .unwrap_err();
        match err {
            crate::Error::Oidc(OidcError::Server { error, description }) => {
                assert_eq!(error, "access_denied");
                assert_eq!(description.as_deref(), Some("The user refused"));
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }

    #[test]
    fn parse_request_line_extracts_method_and_target() {
        assert_eq!(
            parse_request_line("GET /callback?code=x&state=y HTTP/1.1").unwrap(),
            ("GET", "/callback?code=x&state=y")
        );
    }

    #[test]
    fn parse_request_line_rejects_empty_line() {
        assert!(parse_request_line("").is_err());
    }

    #[test]
    fn parse_callback_target_rejects_non_callback_path() {
        // A favicon probe carrying query params must NOT be read as a callback.
        let err = parse_callback_target("/favicon.ico?code=x&state=y").unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Callback { .. })
        ));
    }

    #[test]
    fn classify_accepts_get_callback_with_matching_state() {
        let state = Nonce::from_string("the-state");
        let line = "GET /callback?code=the-code&state=the-state HTTP/1.1";
        match classify_request(line, &state) {
            Accepted::Callback(p) => {
                assert_eq!(p.code, "the-code");
                assert_eq!(p.state, "the-state");
            }
            _ => panic!("expected Callback"),
        }
    }

    #[test]
    fn classify_ignores_wrong_method() {
        let state = Nonce::from_string("the-state");
        let line = "POST /callback?code=the-code&state=the-state HTTP/1.1";
        assert!(matches!(classify_request(line, &state), Accepted::Ignore));
    }

    #[test]
    fn classify_ignores_favicon_probe() {
        let state = Nonce::from_string("the-state");
        assert!(matches!(
            classify_request("GET /favicon.ico HTTP/1.1", &state),
            Accepted::Ignore
        ));
    }

    #[test]
    fn classify_ignores_state_mismatch() {
        // An attacker who does not know `state` cannot terminate the flow, even
        // with a well-formed code — it is ignored, not a hard error.
        let state = Nonce::from_string("the-real-state");
        let line = "GET /callback?code=x&state=the-wrong-state HTTP/1.1";
        assert!(matches!(classify_request(line, &state), Accepted::Ignore));
    }

    #[test]
    fn classify_ignores_cross_origin_error_without_state() {
        // A page open in the browser doing fetch(".../callback?error=x") cannot
        // preempt the catcher: no verifying state → ignored.
        let state = Nonce::from_string("the-state");
        assert!(matches!(
            classify_request("GET /callback?error=access_denied HTTP/1.1", &state),
            Accepted::Ignore
        ));
    }

    #[test]
    fn classify_surfaces_genuine_oauth_error_with_matching_state() {
        // The authorization server echoes `state` even on a refusal, so a genuine
        // consent-denied redirect terminates fast.
        let state = Nonce::from_string("the-state");
        let line = "GET /callback?error=access_denied&state=the-state HTTP/1.1";
        match classify_request(line, &state) {
            Accepted::OauthError(crate::Error::Oidc(OidcError::Server { error, .. })) => {
                assert_eq!(error, "access_denied");
            }
            _ => panic!("expected OauthError(Server)"),
        }
    }

    /// Send one HTTP request to the loopback port and drain the response.
    async fn send_request(port: u16, req: &str) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        let _ = s.read_to_end(&mut resp).await;
    }

    #[tokio::test]
    async fn accept_ignores_preconnect_and_favicon_then_returns_real_redirect() {
        // The F2 regression: a silent preconnect and a favicon probe must NOT
        // consume the single accept slot; the genuine redirect on a later socket
        // still wins.
        let server = LoopbackServer::bind(0).await.unwrap();
        let port = server.port();
        let state = Nonce::from_string("real-state");

        let client = tokio::spawn(async move {
            // 1) Silent preconnect: open a socket, send nothing, close it.
            let pre = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            drop(pre);
            // 2) Favicon probe — answered 404 and ignored.
            send_request(port, "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n").await;
            // 3) The genuine redirect.
            send_request(
                port,
                "GET /callback?code=the-code&state=real-state HTTP/1.1\r\nHost: x\r\n\r\n",
            )
            .await;
        });

        let params = server
            .accept(&state, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(params.code, "the-code");
        assert_eq!(params.state, "real-state");
        client.await.unwrap();
    }

    #[tokio::test]
    async fn accept_times_out_when_only_noise_arrives() {
        // A cross-origin `?error=…` that cannot prove `state` must not terminate
        // the flow; with no genuine redirect, accept eventually times out.
        let server = LoopbackServer::bind(0).await.unwrap();
        let port = server.port();
        let state = Nonce::from_string("real-state");

        let client = tokio::spawn(async move {
            send_request(
                port,
                "GET /callback?error=access_denied HTTP/1.1\r\nHost: x\r\n\r\n",
            )
            .await;
        });

        let err = server
            .accept(&state, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Oidc(OidcError::Callback { .. })
        ));
        client.await.unwrap();
    }

    proptest! {
        /// Any code/state pair from the URL-safe alphabet roundtrips through the
        /// callback target parser.
        #[test]
        fn prop_code_state_roundtrip(
            code in "[A-Za-z0-9_.~-]{1,64}",
            state in "[A-Za-z0-9_.~-]{1,64}",
        ) {
            let target = format!("/callback?code={code}&state={state}");
            let p = parse_callback_target(&target).unwrap();
            prop_assert_eq!(p.code, code);
            prop_assert_eq!(p.state, state);
        }
    }
}
