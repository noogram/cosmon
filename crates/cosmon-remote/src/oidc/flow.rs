// SPDX-License-Identifier: AGPL-3.0-only

//! The login state machine and the silent-refresh protocol
//! (delib-20260710-33b7 C2, C6, C7).
//!
//! This module orchestrates the pieces — [`super::discovery`],
//! [`super::pkce_s256`], [`super::loopback`], [`super::exchange`] — and the
//! credential-store primitives from [`crate::credential`] into three public
//! gestures:
//!
//! - [`login`] — the 7-step browser flow that mints and persists the first
//!   `{access, refresh}` pair.
//! - [`ensure_token`] / [`refresh_credential`] — the silent-refresh seam every
//!   command hits: **zero network when the access token is still valid**, and a
//!   single-writer refresh (never two in parallel per key) when it is not.
//! - [`force_refresh`] — the reactive path when the server returns `401` despite
//!   a locally-valid token (a clock-drift guard), and [`logout`].
//!
//! # The refresh protocol (C2), in one paragraph
//!
//! Forgejo rotates refresh tokens on every use (single-use). Two `cosmon-remote`
//! invocations that refresh in parallel would invalidate each other and force a
//! re-login. The guard is: an **advisory sidecar lock** per credential key + a
//! **double-check under the lock** (re-read the store; if a peer already rotated
//! to a fresh token, **adopt it with zero network** rather than refresh again) +
//! **persist-before-use** (write the new pair before returning the access token,
//! to shrink the crash-during-rotation window) + a **compare-and-swap fallback**
//! (on `invalid_grant`, re-read the store to tell a lost race from a genuinely
//! expired refresh token before declaring [`OidcError::RefreshExpired`]). The
//! invariant: *≤ 1 `grant=refresh_token` in flight per `(issuer, sub, aud)` per
//! machine.*

use std::time::Duration;

use chrono::{DateTime, Utc};

use super::discovery::{ClientRegistry, ProviderMetadata};
use super::error::OidcError;
use super::exchange;
use super::loopback::{self, LoopbackServer};
use super::pkce_s256::{CodeVerifier, Nonce};
use crate::credential::{
    BackendKind, CredentialKey, CredentialStore, SecretToken, StoreOutcome, StoredCredential,
};
use crate::error::{Error, Result};

/// Seconds of remaining access-token life below which we proactively refresh,
/// rather than send a request with a token about to lapse mid-flight. Also
/// absorbs modest client/server clock drift before it becomes a 401.
pub const REFRESH_LEEWAY_SECS: i64 = 60;

/// How long [`login`] waits for the browser redirect before giving up.
pub const LOGIN_TIMEOUT_SECS: u64 = 300;

/// The fully resolved inputs to a login: the provider endpoints, the
/// provisioned `client_id`, the exact `redirect_uri`, and the scopes to request.
/// Produced by [`discover`]; constructible directly (via [`OidcEndpoints::new`])
/// so tests can point it at a mock server.
#[derive(Debug, Clone)]
pub struct OidcEndpoints {
    /// The token's minting authority (`iss`) — the credential-key `issuer`.
    pub issuer: String,
    /// Where the browser obtains an authorization code.
    pub authorization_endpoint: String,
    /// Where codes and refresh tokens are exchanged for tokens.
    pub token_endpoint: String,
    /// The provisioned `client_id` (`== aud`).
    pub client_id: String,
    /// The exact `redirect_uri` (loopback), matched byte-for-byte by the server.
    pub redirect_uri: String,
    /// The scopes to request.
    pub scopes: Vec<String>,
}

impl OidcEndpoints {
    /// Assemble the endpoints directly (discovery seam for tests).
    pub fn new(
        issuer: impl Into<String>,
        authorization_endpoint: impl Into<String>,
        token_endpoint: impl Into<String>,
        client_id: impl Into<String>,
        redirect_uri: impl Into<String>,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            authorization_endpoint: authorization_endpoint.into(),
            token_endpoint: token_endpoint.into(),
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            scopes,
        }
    }

    /// The credential key `(issuer, sub, aud=client_id)` this login persists to.
    pub fn credential_key(&self, sub: &str) -> CredentialKey {
        CredentialKey::new(&self.issuer, sub, &self.client_id)
    }

    /// The lighter [`RefreshConfig`] carrying just what a refresh grant needs.
    /// Defaults to [`RefreshRotation::Rotating`] — the safe assumption for the
    /// module's stated target (Forgejo, `InvalidateRefreshTokens=true`).
    pub fn refresh_config(&self) -> RefreshConfig {
        RefreshConfig {
            token_endpoint: self.token_endpoint.clone(),
            client_id: self.client_id.clone(),
            rotation: RefreshRotation::default(),
        }
    }
}

/// Whether the provider rotates (single-uses) refresh tokens on every grant.
///
/// This governs the one ambiguous case in the refresh protocol: a token
/// response that **omits** the `refresh_token` (RFC 6749 §5.1 marks it
/// OPTIONAL). What "omitted" means depends entirely on the provider:
///
/// - On a [`Static`](RefreshRotation::Static) provider it means *"keep using the
///   one you already hold"* — safe to reuse the previous refresh token.
/// - On a [`Rotating`](RefreshRotation::Rotating) provider the presented refresh
///   token was **already invalidated by the very grant that returned empty**.
///   Reusing it would resurrect a dead token and the *next* refresh would fail
///   `invalid_grant`, forcing a spurious re-login. So an omitted refresh token
///   from a rotating provider must be treated as [`OidcError::RefreshExpired`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshRotation {
    /// Single-use refresh tokens: each grant invalidates the presented token and
    /// issues a new one (Forgejo `InvalidateRefreshTokens=true`). The safe
    /// default for this module's target — an omitted refresh token is fatal, not
    /// reusable.
    #[default]
    Rotating,
    /// The refresh token is stable across grants; an omitted `refresh_token` in
    /// the response means "reuse the current one" (RFC 6749 §6).
    Static,
}

/// The minimal inputs to a *refresh* grant — the token endpoint, the
/// `client_id`, and the provider's [`RefreshRotation`] policy. A credential can
/// be silently refreshed knowing only these, so the fast (no-refresh) path never
/// needs full discovery.
#[derive(Debug, Clone)]
pub struct RefreshConfig {
    /// Where the refresh grant is POSTed.
    pub token_endpoint: String,
    /// The `client_id` presented with the grant.
    pub client_id: String,
    /// Whether the provider single-uses refresh tokens (governs the
    /// empty-`refresh_token` fallback — see [`RefreshRotation`]).
    pub rotation: RefreshRotation,
}

/// What a valid-token request produced.
#[derive(Debug)]
pub enum TokenState {
    /// A usable access token (fresh from cache, adopted from a peer, or just
    /// refreshed). The caller presents it as `Authorization: Bearer`.
    Valid(SecretToken),
    /// No usable token and no refresh can recover one — the caller must run a
    /// full browser [`login`].
    NeedsLogin,
}

/// The cache read, before any network — the decision [`ensure_token`] makes.
#[derive(Debug)]
pub enum CacheState {
    /// The access token is valid now (or a static env bearer) — zero network.
    Fresh(SecretToken),
    /// A refresh token is present but the access token is expiring — a refresh
    /// grant is needed. Carries the current credential (for the CAS fallback).
    Stale(StoredCredential),
    /// Nothing is stored for this key — a full login is required.
    Cold,
}

/// The outcome of a successful [`login`], for the caller to report and to record
/// the resolved `(issuer, client_id)` into the profile.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    /// The key the credential was persisted under.
    pub key: CredentialKey,
    /// Which backend stored it (keyring / file).
    pub backend: BackendKind,
    /// When the freshly minted access token expires.
    pub expires_at: DateTime<Utc>,
}

/// Resolve the provider endpoints and the provisioned `client_id` for `audience`
/// (delib C8): standard OIDC discovery for the endpoints, the cosmon
/// reverse-discovery document for the `client_id`.
pub async fn discover(
    http: &reqwest::Client,
    issuer_base: &str,
    host_base: &str,
    audience: &str,
    fallback_scopes: Vec<String>,
) -> Result<OidcEndpoints> {
    let meta = ProviderMetadata::fetch(http, issuer_base).await?;
    let registry = ClientRegistry::fetch(http, host_base).await?;
    let client = registry
        .client_for(audience)
        .ok_or_else(|| OidcError::Discovery {
            reason: format!(
                "no OAuth client provisioned for audience {audience:?} at {host_base} \
             (the cosmon-oauth-clients document lists none for it)"
            ),
        })?;
    // A `redirect_uri` published by the (integrity-only) client registry is
    // never trusted verbatim: validate it is a loopback IP literal over http
    // before we bind a listener and advertise it to the authorization server.
    // The built-in default is constructed from the loopback IP literal, so it
    // is trusted without a re-parse. (Review task-20260710-a6ae F1, HIGH.)
    let redirect_uri = match client.redirect_uri.clone() {
        Some(uri) => {
            loopback::validate_loopback_redirect_uri(&uri)?;
            uri
        }
        None => loopback::redirect_uri(loopback::DEFAULT_REDIRECT_PORT),
    };
    let scopes = client.scopes.clone().unwrap_or(fallback_scopes);
    Ok(OidcEndpoints::new(
        meta.issuer,
        meta.authorization_endpoint,
        meta.token_endpoint,
        client.client_id.clone(),
        redirect_uri,
        scopes,
    ))
}

/// Build the `authorization_endpoint` URL carrying the PKCE `code_challenge`
/// and the CSRF `state`. Pure — no I/O — so the exact query shape is
/// unit-testable.
///
/// No OIDC `nonce` is emitted: this flow never validates an `id_token`, so a
/// minted nonce would be a control the code cannot enforce (see
/// [`super::pkce_s256`] header).
pub fn build_authorize_url(
    endpoints: &OidcEndpoints,
    state: &str,
    code_challenge: &str,
) -> Result<String> {
    let mut url =
        url::Url::parse(&endpoints.authorization_endpoint).map_err(OidcError::transport)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &endpoints.client_id)
        .append_pair("redirect_uri", &endpoints.redirect_uri)
        .append_pair("scope", &endpoints.scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

/// Run the OAuth2-PKCE authorization-code login and persist the resulting
/// credential (delib C7). The seven steps, in order:
///
/// 1. generate the PKCE `code_verifier` + S256 `code_challenge`, and the CSRF
///    `state` nonce;
/// 2. **bind the loopback listener before opening the browser** (fail fast if
///    the redirect port is taken);
/// 3. build the authorize URL and hand it to `open` (which opens the browser);
/// 4. `await` the redirect on the loopback listener (bounded by `timeout`);
/// 5. verify the echoed `state` (done inside [`LoopbackServer::accept`]);
/// 6. exchange the code + verifier for `{access, refresh, expires_in}`;
/// 7. persist the triple under `(issuer, sub, client_id)` and return the
///    outcome.
///
/// `open` receives the authorize URL. In production it opens the browser and
/// prints the URL as a fallback; tests inject a closure that drives the callback
/// directly, so the whole flow runs without a real browser.
pub async fn login(
    http: &reqwest::Client,
    store: &CredentialStore,
    endpoints: &OidcEndpoints,
    sub: &str,
    timeout: Duration,
    open: impl FnOnce(&str),
) -> Result<LoginOutcome> {
    // 0. Refuse to run the browser dance if the resolved backend cannot persist
    // the result: a read-only Env backend would silently discard the freshly
    // minted credential (F4 #1). Fail fast, before opening the browser, rather
    // than after a wasted round-trip.
    if store.backend_kind() == BackendKind::Env {
        return Err(OidcError::CredentialNotPersisted.into());
    }

    // 1. PKCE material + CSRF state nonce.
    let verifier = CodeVerifier::generate();
    let challenge = verifier.code_challenge();
    let state = Nonce::generate();

    // 2. Bind the loopback listener BEFORE opening the browser (C7 ordering).
    let port = redirect_port(&endpoints.redirect_uri);
    let server = LoopbackServer::bind(port).await?;

    // 3. Build the authorize URL and open the browser.
    let authorize_url = build_authorize_url(endpoints, state.as_str(), &challenge)?;
    open(&authorize_url);

    // 4 & 5. Await the redirect and verify `state`.
    let callback = server.accept(&state, timeout).await?;

    // 6. Exchange the code for tokens.
    let now = Utc::now();
    let tokens = exchange::exchange_code(
        http,
        &endpoints.token_endpoint,
        &endpoints.client_id,
        &callback.code,
        verifier.as_str(),
        &endpoints.redirect_uri,
    )
    .await?;

    // 7. Persist the triple.
    let key = endpoints.credential_key(sub);
    let cred = build_credential(
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.expires_in,
        now,
    );
    let expires_at = cred.expires_at();
    // The Env backend was rejected at step 0, so this always persists; the
    // outcome check keeps persist-before-use loud even if that gate ever moves.
    if store.store(&key, &cred)? == StoreOutcome::Discarded {
        return Err(OidcError::CredentialNotPersisted.into());
    }

    Ok(LoginOutcome {
        key,
        backend: store.backend_kind(),
        expires_at,
    })
}

/// Read the stored credential and decide, **without any network**, whether the
/// access token is usable now, needs a refresh, or is missing.
pub fn cached_access(
    store: &CredentialStore,
    key: &CredentialKey,
    now: DateTime<Utc>,
    leeway: chrono::Duration,
) -> Result<CacheState> {
    match store.load(key)? {
        None => Ok(CacheState::Cold),
        Some(cred) => {
            // A static (env) bearer has no refresh token and never expires: it
            // is always fresh and must never be sent to a refresh grant.
            if !cred.has_refresh() {
                return Ok(CacheState::Fresh(clone_access(&cred)));
            }
            if cred.is_expired_within(now, leeway) {
                Ok(CacheState::Stale(cred))
            } else {
                Ok(CacheState::Fresh(clone_access(&cred)))
            }
        }
    }
}

/// The full silent-refresh seam: return a valid access token, refreshing if the
/// cached one is expiring. Zero network on the fast path (fresh cache).
pub async fn ensure_token(
    http: &reqwest::Client,
    store: &CredentialStore,
    key: &CredentialKey,
    cfg: &RefreshConfig,
    now: DateTime<Utc>,
    leeway: chrono::Duration,
) -> Result<TokenState> {
    match cached_access(store, key, now, leeway)? {
        CacheState::Fresh(token) => Ok(TokenState::Valid(token)),
        CacheState::Cold => Ok(TokenState::NeedsLogin),
        CacheState::Stale(_) => refresh_credential(http, store, key, cfg, leeway).await,
    }
}

/// Perform the single-writer refresh (delib C2) for a credential known to be
/// expiring. Adopts a peer's freshly rotated token with zero network when
/// possible; otherwise performs exactly one refresh grant, persists it before
/// returning, and distinguishes a lost race from a dead refresh token.
pub async fn refresh_credential(
    http: &reqwest::Client,
    store: &CredentialStore,
    key: &CredentialKey,
    cfg: &RefreshConfig,
    leeway: chrono::Duration,
) -> Result<TokenState> {
    // Adopt a peer's token if the store now holds a non-expiring one — the peer
    // rotated while we were about to.
    rotate(http, store, key, cfg, move |cred, now| {
        !cred.is_expired_within(now, leeway)
    })
    .await
}

/// The reactive path (kahneman-F7): the server rejected a *locally-valid* token
/// with `401` (its clock is ahead of ours). Force a refresh regardless of local
/// expiry — but if a peer already rotated (the stored access differs from the
/// one we presented), adopt that instead of a redundant grant.
pub async fn force_refresh(
    http: &reqwest::Client,
    store: &CredentialStore,
    key: &CredentialKey,
    cfg: &RefreshConfig,
    presented_access: &str,
) -> Result<TokenState> {
    rotate(http, store, key, cfg, move |cred, now| {
        cred.access_token().expose() != presented_access && !cred.is_expired(now)
    })
    .await
}

/// Remove the stored credential for `key` (idempotent). The reverse of [`login`].
pub fn logout(store: &CredentialStore, key: &CredentialKey) -> Result<()> {
    store.delete(key)
}

// --- the shared rotation core -------------------------------------------

/// Cap on the lock-contention spin: `SPIN_ATTEMPTS × SPIN_INTERVAL` bounds how
/// long we wait for a peer's refresh before falling back to a fail-safe read.
const SPIN_ATTEMPTS: usize = 200;
const SPIN_INTERVAL: Duration = Duration::from_millis(50);

/// The single-writer rotation loop shared by [`refresh_credential`] and
/// [`force_refresh`]. `adopt_if` decides, on each store read, whether a peer's
/// stored credential is good enough to adopt without a network refresh — this is
/// the compare-and-swap that keeps refreshes single-flight even if the advisory
/// lock is a no-op (e.g. on NFS).
async fn rotate(
    http: &reqwest::Client,
    store: &CredentialStore,
    key: &CredentialKey,
    cfg: &RefreshConfig,
    adopt_if: impl Fn(&StoredCredential, DateTime<Utc>) -> bool,
) -> Result<TokenState> {
    for _ in 0..SPIN_ATTEMPTS {
        if let Some(_lock) = store.try_lock(key)? {
            // Critical section: we are the single writer for this key.
            let now = Utc::now();
            let Some(cred) = store.load(key)? else {
                return Ok(TokenState::NeedsLogin);
            };
            if adopt_if(&cred, now) {
                // A peer already rotated — adopt with zero network.
                return Ok(TokenState::Valid(clone_access(&cred)));
            }
            // Gate refresh on BOTH halves (F4): a static bearer (no refresh
            // token) cannot be refreshed, and a read-only Env backend cannot
            // persist a rotation (its `store` is a discard), so refreshing there
            // would hand out a token the store silently dropped. Either way, the
            // honest answer is "run a full login", not a doomed grant.
            if !cred.has_refresh() || store.backend_kind() == BackendKind::Env {
                return Ok(TokenState::NeedsLogin);
            }
            // Wrap the plaintext refresh-token copy in `Zeroizing` so the heap
            // buffer is wiped on drop. This is the flow-orchestration-side
            // sibling of the store-side F2 fix (task-20260710-a5a6): every other
            // `expose().to_owned()` in this file sinks straight into a
            // `SecretToken` (itself `Zeroizing`), but `presented` lives bare
            // across the `refresh_token` await and the CAS comparison below, so
            // it must carry the same wipe-on-drop guarantee itself.
            let presented = zeroize::Zeroizing::new(cred.refresh_token().expose().to_owned());
            match exchange::refresh_token(
                http,
                &cfg.token_endpoint,
                &cfg.client_id,
                presented.as_str(),
            )
            .await
            {
                Ok(tokens) => {
                    let refreshed = build_credential(
                        &tokens.access_token,
                        &tokens.refresh_token,
                        tokens.expires_in,
                        now,
                    );
                    let Some(fresh) = refreshed.reconcile_refresh(&cred, cfg.rotation) else {
                        // A rotating provider returned an empty refresh token: the
                        // token we just presented is already spent, so there is no
                        // live refresh token left to fall back on. Reusing the
                        // previous one would resurrect a dead token and fail the
                        // next refresh — surface a clean re-login instead.
                        return Err(OidcError::RefreshExpired.into());
                    };
                    // Persist-before-use: the store is updated before the access
                    // token is handed out. The Env gate above guarantees a
                    // writable backend here, so a `Discarded` outcome is a
                    // logic error, not a routine path — surface it loud rather
                    // than return an unpersisted token (F4 #1).
                    if store.store(key, &fresh)? == StoreOutcome::Discarded {
                        return Err(OidcError::CredentialNotPersisted.into());
                    }
                    return Ok(TokenState::Valid(clone_access(&fresh)));
                }
                Err(e) if is_invalid_grant(&e) => {
                    // CAS fallback: re-read to tell a lost race (a peer rotated
                    // our token out from under us) from a genuinely dead refresh
                    // token.
                    if let Some(after) = store.load(key)? {
                        if after.refresh_token().expose() != presented.as_str()
                            && !after.is_expired(Utc::now())
                        {
                            return Ok(TokenState::Valid(clone_access(&after)));
                        }
                    }
                    return Err(OidcError::RefreshExpired.into());
                }
                Err(e) => return Err(e),
            }
        }
        // A peer holds the lock (is refreshing). Wait, then adopt what they
        // persist rather than pile on a second grant. (The `if let` above always
        // returns, so reaching here means the lock was unavailable.)
        tokio::time::sleep(SPIN_INTERVAL).await;
        if let Some(cred) = store.load(key)? {
            if adopt_if(&cred, Utc::now()) {
                return Ok(TokenState::Valid(clone_access(&cred)));
            }
        }
    }
    // Contention never cleared. Fail safe: never POST a refresh unlocked — hand
    // back a still-usable token if one is on disk, else demand a login.
    match store.load(key)? {
        Some(cred) if !cred.is_expired(Utc::now()) => Ok(TokenState::Valid(clone_access(&cred))),
        _ => Err(OidcError::RefreshExpired.into()),
    }
}

// --- small helpers -------------------------------------------------------

/// The port the loopback listener must bind to match `redirect_uri` exactly.
/// Falls back to the default when the URI has no explicit port.
fn redirect_port(redirect_uri: &str) -> u16 {
    url::Url::parse(redirect_uri)
        .ok()
        .and_then(|u| u.port())
        .unwrap_or(loopback::DEFAULT_REDIRECT_PORT)
}

/// Copy the access token out of a credential for the one legitimate use — the
/// bearer header. This is the single deliberate exposure of the plaintext.
fn clone_access(cred: &StoredCredential) -> SecretToken {
    SecretToken::new(cred.access_token().expose().to_owned())
}

/// Whether `e` is an OIDC `invalid_grant` server error.
fn is_invalid_grant(e: &Error) -> bool {
    matches!(e, Error::Oidc(oe) if oe.is_invalid_grant())
}

/// Assemble a [`StoredCredential`] from a token response, converting the relative
/// `expires_in` to an absolute instant against `now`.
///
/// `expires_in` is **server-controlled and untrusted** (parse-don't-trust): a
/// hostile or buggy OIDC provider can return an arbitrary `i64`. The naïve
/// `now + chrono::Duration::seconds(expires_in)` panics on overflow — both
/// [`chrono::Duration::seconds`] (out-of-bounds `TimeDelta`) and the `DateTime`
/// addition (`DateTime + TimeDelta overflowed`) abort the process, which would
/// be reachable from the login exchange *and* the reactive-401 refresh grant.
/// That violates the delib-33b7 "no unwrap/expect in lib — `Result` partout"
/// discipline, so we clamp instead of trusting the wire value:
///
/// - `expires_in <= 0` → the token is already stale; expire it at `now` so the
///   very next [`StoredCredential::is_expired`] check (inclusive at `now`) forces
///   a refresh or re-login rather than handing out a dead bearer.
/// - out-of-range magnitude (either the [`chrono::Duration`] construction or the
///   `now + duration` sum overflows) → saturate to `now`, i.e. already-expiring.
///   A saturated far-future would be worse than useless: it would mask a broken
///   provider by pinning a token that can never be proactively refreshed.
fn build_credential(
    access_token: &str,
    refresh_token: &str,
    expires_in: i64,
    now: DateTime<Utc>,
) -> StoredCredential {
    // `try_seconds` returns `None` when `expires_in` is out of `TimeDelta`'s
    // representable range; `checked_add_signed` returns `None` when the sum
    // overflows `DateTime<Utc>`. Any failure saturates to `now` (already stale).
    let expires_at = if expires_in <= 0 {
        now
    } else {
        chrono::Duration::try_seconds(expires_in)
            .and_then(|d| now.checked_add_signed(d))
            .unwrap_or(now)
    };
    StoredCredential::new(
        SecretToken::new(access_token.to_owned()),
        SecretToken::new(refresh_token.to_owned()),
        expires_at,
    )
}

impl StoredCredential {
    /// Reconcile this freshly refreshed credential's refresh token against the
    /// provider's [`RefreshRotation`] policy.
    ///
    /// - A **non-empty** refresh token in the response is always kept as-is.
    /// - An **empty** (omitted) refresh token is ambiguous, and the provider
    ///   policy decides:
    ///   - [`RefreshRotation::Static`] → reuse `previous`'s refresh token (the
    ///     provider means "keep the one you hold"); returns `Some`.
    ///   - [`RefreshRotation::Rotating`] → the presented refresh token was
    ///     already invalidated by the grant that returned empty, so there is no
    ///     live token to reuse; returns `None`, and the caller surfaces
    ///     [`OidcError::RefreshExpired`] rather than resurrecting a dead token.
    fn reconcile_refresh(
        self,
        previous: &StoredCredential,
        rotation: RefreshRotation,
    ) -> Option<StoredCredential> {
        if !self.refresh_token().expose().is_empty() {
            return Some(self);
        }
        match rotation {
            RefreshRotation::Static => Some(StoredCredential::new(
                SecretToken::new(self.access_token().expose().to_owned()),
                SecretToken::new(previous.refresh_token().expose().to_owned()),
                self.expires_at(),
            )),
            RefreshRotation::Rotating => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn endpoints() -> OidcEndpoints {
        OidcEndpoints::new(
            "https://forge.example",
            "https://forge.example/login/oauth/authorize",
            "https://forge.example/login/oauth/access_token",
            "client-A",
            "http://127.0.0.1:7777/callback",
            vec!["openid".into(), "profile".into()],
        )
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().unwrap()
    }

    #[test]
    fn authorize_url_carries_pkce_and_state() {
        let url = build_authorize_url(&endpoints(), "the-state", "the-challenge").unwrap();
        assert!(url.starts_with("https://forge.example/login/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client-A"));
        assert!(url.contains("code_challenge=the-challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=the-state"));
        // The redirect_uri is URL-encoded but present.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A7777%2Fcallback"));
    }

    #[test]
    fn authorize_url_emits_no_oidc_nonce() {
        // Regression guard for task-20260710-05f7 (review a6ae F3): the OIDC
        // `nonce` is deliberately absent because no `id_token` is validated —
        // a minted-but-unchecked nonce is a fake oracle. Keep it out.
        let url = build_authorize_url(&endpoints(), "the-state", "the-challenge").unwrap();
        assert!(
            !url.contains("nonce="),
            "authorize URL must not carry an OIDC nonce: {url}"
        );
    }

    #[test]
    fn credential_key_uses_client_id_as_audience() {
        let key = endpoints().credential_key("operator");
        assert_eq!(key.issuer(), "https://forge.example");
        assert_eq!(key.sub(), "operator");
        assert_eq!(key.aud(), "client-A");
    }

    #[test]
    fn redirect_port_parses_or_defaults() {
        assert_eq!(redirect_port("http://127.0.0.1:7777/callback"), 7777);
        assert_eq!(redirect_port("http://127.0.0.1:9000/callback"), 9000);
        assert_eq!(redirect_port("not a url"), loopback::DEFAULT_REDIRECT_PORT);
    }

    #[test]
    fn cached_access_cold_when_nothing_stored() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let key = endpoints().credential_key("operator");
        let state = cached_access(&store, &key, Utc::now(), chrono::Duration::seconds(60)).unwrap();
        assert!(matches!(state, CacheState::Cold));
    }

    #[test]
    fn cached_access_fresh_then_stale_across_expiry() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let key = endpoints().credential_key("operator");
        let cred = StoredCredential::new(SecretToken::new("at"), SecretToken::new("rt"), ts(1_000));
        store.store(&key, &cred).unwrap();
        // Well before expiry → fresh.
        let leeway = chrono::Duration::seconds(60);
        assert!(matches!(
            cached_access(&store, &key, ts(800), leeway).unwrap(),
            CacheState::Fresh(_)
        ));
        // Inside the leeway window → stale.
        assert!(matches!(
            cached_access(&store, &key, ts(970), leeway).unwrap(),
            CacheState::Stale(_)
        ));
    }

    #[test]
    fn static_bearer_is_always_fresh() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let key = endpoints().credential_key("operator");
        // A credential with an empty refresh token = a static bearer.
        let cred = StoredCredential::static_bearer(SecretToken::new("env-bearer"));
        store.store(&key, &cred).unwrap();
        let state = cached_access(
            &store,
            &key,
            ts(4_000_000_000),
            chrono::Duration::seconds(60),
        )
        .unwrap();
        match state {
            CacheState::Fresh(t) => assert_eq!(t.expose(), "env-bearer"),
            other => panic!("expected Fresh, got {other:?}"),
        }
    }

    #[test]
    fn logout_removes_the_credential() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let key = endpoints().credential_key("operator");
        store
            .store(
                &key,
                &StoredCredential::new(SecretToken::new("a"), SecretToken::new("r"), ts(1)),
            )
            .unwrap();
        logout(&store, &key).unwrap();
        assert!(store.load(&key).unwrap().is_none());
        // Idempotent.
        logout(&store, &key).unwrap();
    }

    #[tokio::test]
    async fn force_refresh_adopts_a_peer_token_without_network() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let key = endpoints().credential_key("operator");
        // The store already holds a DIFFERENT, non-expired access token: a peer
        // rotated after we sent the bearer the server then rejected. The reactive
        // path (`force_refresh`) must adopt that peer token with ZERO network —
        // the unreachable token endpoint proves no refresh grant is POSTed.
        let peer = StoredCredential::new(
            SecretToken::new("peer-fresh-at"),
            SecretToken::new("rt-peer"),
            Utc::now() + chrono::Duration::hours(1),
        );
        store.store(&key, &peer).unwrap();

        let http = reqwest::Client::new();
        let cfg = RefreshConfig {
            token_endpoint: "http://127.0.0.1:1/token".into(), // unreachable on purpose
            client_id: "client-A".into(),
            rotation: RefreshRotation::Rotating,
        };
        let state = force_refresh(&http, &store, &key, &cfg, "stale-rejected-at")
            .await
            .unwrap();
        match state {
            TokenState::Valid(t) => assert_eq!(t.expose(), "peer-fresh-at"),
            TokenState::NeedsLogin => panic!("expected Valid(adopted peer), got NeedsLogin"),
        }
    }

    #[tokio::test]
    async fn force_refresh_is_noop_adopt_when_stored_matches_presented_and_valid() {
        // If the stored access token is the SAME one the server rejected, there is
        // no peer to adopt — the loop must fall through to an actual refresh grant.
        // With an unreachable endpoint that surfaces as a transport error, proving
        // the adopt predicate did NOT short-circuit on an identical token.
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let key = endpoints().credential_key("operator");
        store
            .store(
                &key,
                &StoredCredential::new(
                    SecretToken::new("same-at"),
                    SecretToken::new("rt"),
                    Utc::now() + chrono::Duration::hours(1),
                ),
            )
            .unwrap();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let cfg = RefreshConfig {
            token_endpoint: "http://127.0.0.1:1/token".into(),
            client_id: "client-A".into(),
            rotation: RefreshRotation::Rotating,
        };
        let err = force_refresh(&http, &store, &key, &cfg, "same-at")
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Oidc(_)),
            "expected an OIDC transport error from the refresh grant, got {err:?}"
        );
    }

    #[test]
    fn static_provider_reuses_refresh_when_response_omits_it() {
        let previous = StoredCredential::new(
            SecretToken::new("old-at"),
            SecretToken::new("keep-this-refresh"),
            ts(1),
        );
        let fresh = build_credential("new-at", "", 900, ts(1_000))
            .reconcile_refresh(&previous, RefreshRotation::Static)
            .expect("a static provider reuses the previous refresh token");
        assert_eq!(fresh.access_token().expose(), "new-at");
        assert_eq!(fresh.refresh_token().expose(), "keep-this-refresh");
        assert_eq!(fresh.expires_at(), ts(1_900));
    }

    #[test]
    fn rotating_provider_refuses_to_resurrect_a_spent_refresh_token() {
        // On a rotating provider (Forgejo InvalidateRefreshTokens=true), an empty
        // rotated refresh_token means the presented token is already invalidated.
        // Reusing it would fail the next refresh with invalid_grant, so the fix
        // refuses the reuse — the caller surfaces RefreshExpired instead.
        let previous = StoredCredential::new(
            SecretToken::new("old-at"),
            SecretToken::new("already-spent-refresh"),
            ts(1),
        );
        let reconciled = build_credential("new-at", "", 900, ts(1_000))
            .reconcile_refresh(&previous, RefreshRotation::Rotating);
        assert!(
            reconciled.is_none(),
            "an omitted refresh token on a rotating provider must not be reused"
        );
    }

    #[test]
    fn non_empty_refresh_token_is_kept_regardless_of_rotation() {
        let previous = StoredCredential::new(
            SecretToken::new("old-at"),
            SecretToken::new("old-refresh"),
            ts(1),
        );
        for rotation in [RefreshRotation::Rotating, RefreshRotation::Static] {
            let fresh = build_credential("new-at", "rotated-refresh", 900, ts(1_000))
                .reconcile_refresh(&previous, rotation)
                .expect("a present refresh token is always kept");
            assert_eq!(fresh.refresh_token().expose(), "rotated-refresh");
        }
    }

    #[test]
    fn refresh_config_defaults_to_rotating() {
        assert_eq!(
            endpoints().refresh_config().rotation,
            RefreshRotation::Rotating
        );
        assert_eq!(RefreshRotation::default(), RefreshRotation::Rotating);
    }

    #[test]
    fn build_credential_normal_expiry_is_offset_from_now() {
        // The happy path is unchanged: a sane `expires_in` becomes `now + secs`.
        let cred = build_credential("at", "rt", 900, ts(1_000));
        assert_eq!(cred.expires_at(), ts(1_900));
        assert!(!cred.is_expired(ts(1_000)));
    }

    #[test]
    fn build_credential_zero_expires_in_is_immediately_stale() {
        // `expires_in == 0` means the token is already dead on arrival.
        let cred = build_credential("at", "rt", 0, ts(1_000));
        assert_eq!(cred.expires_at(), ts(1_000));
        assert!(cred.is_expired(ts(1_000)), "expiry is inclusive at `now`");
    }

    #[test]
    fn build_credential_negative_expires_in_is_immediately_stale() {
        // A hostile/buggy provider returning a negative TTL must not underflow
        // into the past-but-still-usable window; treat it as already stale.
        let cred = build_credential("at", "rt", -1, ts(1_000));
        assert_eq!(cred.expires_at(), ts(1_000));
        assert!(cred.is_expired(ts(1_000)));
    }

    #[test]
    fn build_credential_overflowing_expires_in_saturates_to_stale_no_panic() {
        // These are the exact values from the adversarial repro (task-20260710-1f30
        // F1). Pre-fix, `now + chrono::Duration::seconds(expires_in)` panicked with
        // "DateTime + TimeDelta overflowed" / "TimeDelta::seconds out of bounds".
        // Post-fix, both saturate to `now` (already-expiring) with no panic.
        for hostile in [
            8_000_000_000_000_000_i64,
            9_223_372_036_854_775_i64 + 1,
            i64::MAX,
        ] {
            let cred = build_credential("at", "rt", hostile, ts(1_000));
            assert_eq!(
                cred.expires_at(),
                ts(1_000),
                "out-of-range expires_in={hostile} must saturate to `now`",
            );
            assert!(
                cred.is_expired(ts(1_000)),
                "a saturated credential must read as already expired",
            );
        }
    }

    /// Grep-gate: every plaintext-token copy in this file must sink into a
    /// zeroizing wrapper. A bare `expose().to_owned()` — a plaintext token
    /// materialized onto the heap with no wipe-on-drop guarantee — is the
    /// defect this molecule fixed at the silent-refresh site (`presented`,
    /// the flow-orchestration sibling of the store-side F2 fix
    /// task-20260710-a5a6). Any legitimate copy must land in a
    /// `SecretToken::new(...)` (itself `Zeroizing`) or a `Zeroizing::new(...)`
    /// **in the same statement**.
    ///
    /// Hardened against the security-review 5008 (finding A#2) bypass: the
    /// prior gate matched per physical line, so splitting the copy across two
    /// lines (`.expose()` then `.to_owned()`) slipped past it. This version
    /// strips comments, removes **all** whitespace, and checks the sink within
    /// the enclosing `;`-delimited statement — so a multi-line chain is judged
    /// exactly like a single-line one. Falsifier contract: dropping the
    /// `Zeroizing::new` wrapper from `presented` (on one line or split across
    /// several) reddens this test.
    #[test]
    fn every_plaintext_token_copy_sinks_into_a_zeroizing_wrapper() {
        let src = include_str!("flow.rs");
        // Strip line comments so this file's own prose (which names the pattern)
        // never trips the gate. `//` opens a comment only when it is not part of
        // a scheme like `https://` — guard on the preceding byte.
        let mut code = String::with_capacity(src.len());
        for line in src.lines() {
            let bytes = line.as_bytes();
            let mut cut = line.len();
            let mut i = 0;
            while i + 1 < bytes.len() {
                if bytes[i] == b'/' && bytes[i + 1] == b'/' {
                    let prev = if i == 0 { b' ' } else { bytes[i - 1] };
                    if prev != b':' {
                        cut = i;
                        break;
                    }
                }
                i += 1;
            }
            code.push_str(&line[..cut]);
            code.push('\n');
        }
        // Remove ALL whitespace so a copy split across physical lines
        // (`.expose()\n .to_owned()`, the delib-5008 multi-line bypass) reads as
        // one contiguous chain; the `;` split then scopes the sink check to the
        // enclosing statement.
        let flat: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        // Needle assembled from fragments so this test's body does not itself
        // contain the verbatim literal it hunts for.
        let needle = format!(".{}().{}()", "expose", "to_owned");
        let offenders: Vec<&str> = flat
            .split(';')
            .filter(|stmt| stmt.contains(&needle))
            .filter(|stmt| !(stmt.contains("SecretToken::new") || stmt.contains("Zeroizing::new")))
            .collect();
        assert!(
            offenders.is_empty(),
            "bare plaintext-token copies (no SecretToken/Zeroizing sink in the \
             same statement) found — each leaks an un-zeroized token onto the \
             heap: {offenders:?}",
        );
    }
}
