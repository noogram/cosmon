// SPDX-License-Identifier: AGPL-3.0-only

//! JWKS provisioning over HTTP (OIDC standard) — replaces the v2.4
//! file-stage + `SIGHUP` delivery of signing keys.
//!
//! # What this module owns
//!
//! The **delivery** of authentication keys, nothing more. The validator
//! ([`crate::jwt::JwtVerifier`]), the authorization pin
//! ([`crate::nucleon_map`]), the deny-by-default posture, the
//! `RS256`/`ES256` whitelist, and the `jku`/`x5u` refusal are unchanged.
//! This module only changes *how the keys reach the live
//! [`SharedJwksStore`]*: instead of an operator dropping a JSON blob and
//! sending `kill -HUP`, the adapter **fetches** each trusted issuer's
//! JWKS from its `jwks_uri` and refreshes it on a timer + on demand.
//!
//! # The host-side allowlist is the trust boundary
//!
//! The adapter fetches **only** the issuers declared in
//! `<state_dir>/security/trusted-issuers.toml` — an arbitrary endpoint
//! is never contacted nor trusted. Each entry carries **two distinct
//! fields** (smithy spec §2.1/§3.1, the load-bearing nuance):
//!
//! - `iss` — the **external** issuer URL the `IdP` burns into the token
//!   (`http://<host>/git`). Matches the token claim and the authz pin
//!   byte-for-byte. **Never** used as a fetch target.
//! - `jwks_uri` — the **internal** container address the keys are
//!   actually fetched from (`http://forgejo:3000/login/oauth/keys`).
//!
//! In split-DNS (the Tenant-Demo compose: `iss` is ingress-routed, the fetch
//! is container-to-container) these *must* differ, which is why the
//! config carries both and the discovery `.well-known` lookup is
//! short-circuited when `jwks_uri` is explicit. Discovery is the default
//! only for the mono-DNS case (e.g. a direct provider like Google whose
//! external issuer URL is itself the fetch host).
//!
//! # Refresh — two complementary triggers
//!
//! 1. **TTL pull** ([`JwksProvider::run`]) — a background task re-fetches
//!    every issuer every [`DEFAULT_REFRESH_TTL`] (1 h, configurable). The
//!    net of last resort: purges a key retired upstream that no longer
//!    mints tokens, and bounds freshness even with no traffic.
//! 2. **On-demand cache-miss** ([`JwksProvider::ensure_kid`]) — when a
//!    request arrives with a `kid` absent from the store for a *trusted*
//!    issuer (the typical signature of an upstream key rotation between
//!    TTL ticks), a targeted refetch of that one `jwks_uri` is triggered,
//!    with anti-stampede (single-flight per issuer + cooldown). This is
//!    what makes rotation "free" without waiting for the next tick.
//!
//! # Fail-closed
//!
//! An unreachable issuer at boot leaves its keys **empty** → every token
//! for it is denied ([`crate::jwt::JwtVerifier`] returns
//! `IssuerNotPinned`) until a fetch succeeds; a background retry/backoff
//! keeps trying. A *transient* failure for an already-loaded issuer
//! **keeps the live keys** — a network blip never regresses a working
//! store to empty (the same discipline as the file path's
//! `reload_jwks_read_error_keeps_live_store`). A network failure closes
//! the door, it never opens it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::jwt::SharedJwksStore;

/// Default JWKS refresh interval — 1 h, validated by the OIDC review
/// (smithy spec §2.3). The TTL is only the background net; urgent
/// rotation is covered instantly by the on-demand cache-miss, so 1 h
/// suffices and is ~12× quieter on the wire than a 5 min poll.
pub const DEFAULT_REFRESH_TTL: Duration = Duration::from_secs(3600);

/// Default cooldown between two on-demand refetches of the **same**
/// issuer. Bounds the cache-miss path as an amplification channel: an
/// attacker spraying tokens with random `kid`s cannot force more than
/// one fetch per issuer per window.
pub const DEFAULT_CACHE_MISS_COOLDOWN: Duration = Duration::from_secs(30);

/// Initial backoff between boot-time retry passes while an issuer is
/// still unreachable (no keys loaded yet). Doubles up to
/// [`MAX_BOOT_BACKOFF`], then the loop settles into the steady TTL
/// cadence once any key is present.
const INITIAL_BOOT_BACKOFF: Duration = Duration::from_secs(1);

/// Cap on the boot retry backoff.
const MAX_BOOT_BACKOFF: Duration = Duration::from_secs(60);

/// Per-issuer HTTP timeout for both the discovery and the JWKS fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// One trusted issuer, host-side. The two-field shape is load-bearing:
/// `iss` matches the token, `jwks_uri` targets the fetch (see module
/// docs and smithy spec §2.1).
#[derive(Clone, Debug, Deserialize)]
pub struct TrustedIssuer {
    /// External issuer URL — matches the token `iss` claim and the
    /// authz pin byte-for-byte. Never a fetch target.
    pub iss: String,
    /// Internal JWKS endpoint to fetch from. When omitted, the
    /// `jwks_uri` is discovered via `<iss>/.well-known/openid-configuration`
    /// (mono-DNS providers only — in split-DNS the external `.well-known`
    /// would advertise an external `jwks_uri`, so it must be set
    /// explicitly).
    #[serde(default)]
    pub jwks_uri: Option<String>,
    /// Audiences pinned for this issuer (the `aud` claim must match one).
    /// On the file path these lived inside the JWKS file; the wire JWKS
    /// carries no `aud`, so they move here.
    #[serde(default)]
    pub audiences: Vec<String>,
}

/// The host-side allowlist of trusted issuers, parsed from
/// `<state_dir>/security/trusted-issuers.toml`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TrustedIssuers {
    /// The `[[issuer]]` array. Empty (or a missing file) means **no
    /// issuer is trusted** — deny-all, fail-closed.
    #[serde(default, rename = "issuer")]
    pub issuers: Vec<TrustedIssuer>,
}

impl TrustedIssuers {
    /// Load the allowlist from `<state_dir>/security/trusted-issuers.toml`.
    /// A missing file resolves to an empty allowlist (deny-all) — the
    /// adapter then relies on the file-stage fallback, or denies every
    /// token until issuers are configured.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the file exists but cannot be read, or an
    /// `InvalidData` error wrapping the TOML parse failure.
    pub fn load(state_dir: &Path) -> std::io::Result<Self> {
        let path = state_dir.join("security/trusted-issuers.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        toml::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// `true` when no issuer is configured (deny-all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.issuers.is_empty()
    }

    /// Number of configured issuers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.issuers.len()
    }
}

/// Errors raised while fetching a JWKS over HTTP.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The HTTP request (discovery or JWKS) failed or returned a
    /// non-success status.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

/// The OIDC discovery document — only the one field we consume
/// (`jwks_uri`). Used solely for the mono-DNS case where `jwks_uri` is
/// not configured explicitly.
#[derive(Debug, Deserialize)]
struct OidcDiscovery {
    jwks_uri: String,
}

/// The HTTP client that fetches JWKS documents. Stateless beyond the
/// reused connection pool — the trust decisions live in
/// [`TrustedIssuers`] (which issuers) and [`crate::jwt::JwksStore`]
/// (which keys are well-formed and whitelisted).
#[derive(Clone, Debug)]
pub struct JwksFetcher {
    client: reqwest::Client,
}

impl JwksFetcher {
    /// Build a fetcher with a bounded per-request timeout so the refresh
    /// loop can never hang on a stalled endpoint.
    ///
    /// # Errors
    ///
    /// Returns the `reqwest` error if the TLS backend cannot be
    /// initialised.
    pub fn new() -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build()?;
        Ok(Self { client })
    }

    /// Resolve the JWKS URL for an issuer: the explicit `jwks_uri` if
    /// present, otherwise the `.well-known/openid-configuration`
    /// discovery on the external issuer URL (mono-DNS only).
    ///
    /// The resolved URL **never** comes from a token claim — only from
    /// the host-side config or that config's issuer. This preserves the
    /// turing G7/G8 `jku`/`x5u` refusal: the token can never choose where
    /// its validating key is fetched from.
    ///
    /// # Errors
    ///
    /// Returns [`FetchError`] if discovery is required but the request
    /// fails or the document is malformed.
    pub async fn resolve_jwks_uri(&self, issuer: &TrustedIssuer) -> Result<String, FetchError> {
        if let Some(uri) = &issuer.jwks_uri {
            return Ok(uri.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.iss.trim_end_matches('/')
        );
        let disc: OidcDiscovery = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(disc.jwks_uri)
    }

    /// Fetch the raw JWKS document for an issuer, resolving its
    /// `jwks_uri` first. Returns the body verbatim; parsing + the
    /// algorithm whitelist happen in
    /// [`crate::jwt::JwksStore::replace_remote_jwks`].
    ///
    /// # Errors
    ///
    /// Returns [`FetchError`] on any transport failure or non-success
    /// status (discovery or JWKS).
    pub async fn fetch_issuer(&self, issuer: &TrustedIssuer) -> Result<String, FetchError> {
        let uri = self.resolve_jwks_uri(issuer).await?;
        let body = self
            .client
            .get(&uri)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(body)
    }
}

/// Per-issuer runtime state for the anti-stampede guard: the
/// single-flight lock (held across the fetch `await`) and the timestamp
/// of the last fetch attempt (for the cooldown).
#[derive(Default)]
struct IssuerRuntime {
    fetch_lock: Arc<tokio::sync::Mutex<()>>,
    last_fetch: Option<Instant>,
}

/// Summary of one [`JwksProvider::refresh_all`] pass, for ops logging.
#[derive(Debug, Default)]
pub struct JwksRefreshReport {
    /// Issuers in the allowlist.
    pub issuers_total: usize,
    /// Issuers whose fetch + parse succeeded this pass.
    pub issuers_ok: usize,
    /// Total keys pinned across all issuers after the pass.
    pub keys_total: usize,
}

impl JwksRefreshReport {
    /// `true` when every configured issuer refreshed successfully.
    #[must_use]
    pub fn all_ok(&self) -> bool {
        self.issuers_ok == self.issuers_total
    }

    /// Emit the structured `tracing` surface for this pass.
    pub fn log(&self) {
        tracing::info!(
            event = "jwks.refresh",
            issuers_total = self.issuers_total,
            issuers_ok = self.issuers_ok,
            keys_total = self.keys_total,
            "jwks http-fetch refresh pass",
        );
    }
}

/// The live JWKS provider: owns the trusted-issuer allowlist, the HTTP
/// fetcher, and a handle to the [`SharedJwksStore`] the validator reads.
///
/// Construct it at boot, run [`Self::refresh_all`] once for the initial
/// load, then `tokio::spawn` [`Self::run`] for the TTL + boot-backoff
/// loop. The on-demand cache-miss path is [`Self::ensure_kid`].
///
/// The store handle is the **same** `ArcSwap` the validator reads via
/// `state.jwks.load()`: a refresh is a single atomic pointer store, so
/// in-flight requests keep their snapshot and the next request sees the
/// new keys — no reader blocking, no reboot, no dropped worker.
#[derive(Clone)]
pub struct JwksProvider {
    shared: SharedJwksStore,
    fetcher: JwksFetcher,
    issuers: Vec<TrustedIssuer>,
    cooldown: Duration,
    runtime: Arc<Mutex<HashMap<String, IssuerRuntime>>>,
}

impl JwksProvider {
    /// Build a provider over the given store, allowlist, and HTTP
    /// fetcher. Uses [`DEFAULT_CACHE_MISS_COOLDOWN`].
    #[must_use]
    pub fn new(shared: SharedJwksStore, issuers: Vec<TrustedIssuer>, fetcher: JwksFetcher) -> Self {
        Self {
            shared,
            fetcher,
            issuers,
            cooldown: DEFAULT_CACHE_MISS_COOLDOWN,
            runtime: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Override the cache-miss cooldown (tests use a short value to
    /// exercise back-to-back refetches deterministically).
    #[must_use]
    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// The shared store handle the validator reads. Cheap clone.
    #[must_use]
    pub fn shared(&self) -> SharedJwksStore {
        self.shared.clone()
    }

    /// `true` when at least one key is pinned across all issuers — the
    /// boot-vs-steady cadence switch in [`Self::run`].
    #[must_use]
    fn has_any_keys(&self) -> bool {
        self.shared
            .load()
            .key_counts_by_issuer()
            .iter()
            .any(|(_, n)| *n > 0)
    }

    /// Re-fetch **every** trusted issuer and publish the merged result.
    ///
    /// Fail-closed and non-regressing: the new store starts as a clone of
    /// the live one, each issuer that fetches + parses cleanly **replaces**
    /// its keys, and an issuer that fails **keeps its previous keys**. A
    /// transient outage therefore never empties a working store; a boot
    /// with everything unreachable leaves an empty store (deny-all).
    pub async fn refresh_all(&self) -> JwksRefreshReport {
        let mut store = (**self.shared.load()).clone();
        let mut issuers_ok = 0;
        for issuer in &self.issuers {
            match self.fetcher.fetch_issuer(issuer).await {
                Ok(json) => {
                    match store.replace_remote_jwks(&issuer.iss, issuer.audiences.clone(), &json) {
                        Ok(n) => {
                            issuers_ok += 1;
                            tracing::info!(
                                event = "jwks.fetch.issuer",
                                iss = %issuer.iss,
                                keys = n,
                                "fetched JWKS for issuer",
                            );
                        }
                        Err(e) => tracing::warn!(
                            event = "jwks.fetch.issuer",
                            iss = %issuer.iss,
                            error = %e,
                            "malformed JWKS document — keeping prior keys for issuer",
                        ),
                    }
                }
                Err(e) => tracing::warn!(
                    event = "jwks.fetch.issuer",
                    iss = %issuer.iss,
                    error = %e,
                    "JWKS fetch failed — keeping prior keys for issuer (fail-closed)",
                ),
            }
        }
        let keys_total = store.key_counts_by_issuer().iter().map(|(_, n)| *n).sum();
        self.shared.store(store);
        JwksRefreshReport {
            issuers_total: self.issuers.len(),
            issuers_ok,
            keys_total,
        }
    }

    /// Targeted on-demand refetch of a single issuer, with anti-stampede.
    ///
    /// Returns `false` immediately (no network) when `iss` is **not** in
    /// the host-side allowlist — an unknown issuer is never contacted.
    /// Otherwise a single-flight lock serialises concurrent refetches of
    /// the same issuer, and a cooldown caps the fetch rate so the
    /// cache-miss cannot be used as an amplification channel. Returns
    /// `true` only when this call performed a fetch that updated the
    /// store.
    pub async fn refresh_issuer(&self, iss: &str) -> bool {
        let Some(issuer) = self.issuers.iter().find(|i| i.iss == iss).cloned() else {
            return false;
        };
        let lock = self.issuer_lock(iss);
        let _guard = lock.lock().await;
        // Cooldown check (under the single-flight lock so a queued caller
        // observes the just-updated timestamp and skips a redundant fetch).
        if let Some(last) = self.last_fetch(iss) {
            if last.elapsed() < self.cooldown {
                return false;
            }
        }
        self.mark_fetched(iss);
        match self.fetcher.fetch_issuer(&issuer).await {
            Ok(json) => {
                let mut store = (**self.shared.load()).clone();
                match store.replace_remote_jwks(iss, issuer.audiences.clone(), &json) {
                    Ok(_) => {
                        self.shared.store(store);
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            event = "jwks.cache_miss",
                            iss = %iss,
                            error = %e,
                            "on-demand refetch returned a malformed document",
                        );
                        false
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    event = "jwks.cache_miss",
                    iss = %iss,
                    error = %e,
                    "on-demand refetch failed — keeping live store (fail-closed)",
                );
                false
            }
        }
    }

    /// The cache-miss entry point for the admission hot path: ensure a
    /// key for `(iss, kid)` is pinned, fetching on demand if absent.
    ///
    /// Returns `true` when the key is present (already, or after a
    /// successful refetch). A token whose `kid` is already known costs no
    /// network round-trip; an unknown `kid` for a trusted issuer triggers
    /// one anti-stampede-guarded refetch, then re-reads the store. This
    /// is what makes an upstream key rotation take effect "in the second
    /// the new token arrives" rather than at the next TTL tick.
    pub async fn ensure_kid(&self, iss: &str, kid: &str) -> bool {
        if self.shared.load().contains_kid(iss, kid) {
            return true;
        }
        self.refresh_issuer(iss).await;
        self.shared.load().contains_kid(iss, kid)
    }

    /// The TTL + boot-backoff refresh loop. `tokio::spawn` this after the
    /// initial [`Self::refresh_all`]. While no key is loaded (boot,
    /// issuer still unreachable) it retries on a bounded exponential
    /// backoff; once any key is present it settles into the steady `ttl`
    /// cadence.
    pub async fn run(self, ttl: Duration) {
        let mut backoff = INITIAL_BOOT_BACKOFF;
        loop {
            let next = if self.has_any_keys() {
                backoff = INITIAL_BOOT_BACKOFF;
                ttl
            } else {
                let b = backoff;
                backoff = (backoff * 2).min(MAX_BOOT_BACKOFF);
                b
            };
            tokio::time::sleep(next).await;
            self.refresh_all().await.log();
        }
    }

    /// Vend (or create) the single-flight lock for an issuer. The std
    /// `Mutex` guards only the registry map (a short, await-free critical
    /// section); the per-issuer `tokio::Mutex` is what is held across the
    /// fetch await. Poisoning is recovered rather than panicked
    /// (`unwrap`-free library discipline).
    fn issuer_lock(&self, iss: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(iss.to_owned()).or_default().fetch_lock.clone()
    }

    /// Read the last-fetch timestamp for an issuer.
    fn last_fetch(&self, iss: &str) -> Option<Instant> {
        let map = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(iss).and_then(|r| r.last_fetch)
    }

    /// Stamp the current instant as this issuer's last fetch attempt.
    fn mark_fetched(&self, iss: &str) {
        let mut map = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(iss.to_owned()).or_default().last_fetch = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jwt::{JwksStore, JwtVerifier};
    use crate::Posture;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TEST_JWKS: &str = include_str!("../tests/fixtures/test_rsa_jwks.json");
    const TEST_JWKS_ROTATED: &str = include_str!("../tests/fixtures/test_rsa_jwks_rotated.json");

    /// A minimal mock JWKS endpoint over a raw TCP socket: it serves a
    /// body that a closure picks per request (so a test can "rotate" the
    /// served document) and counts the JWKS hits. Discovery is served at
    /// `/.well-known/openid-configuration` pointing back at `/keys`.
    struct MockIdp {
        addr: std::net::SocketAddr,
        jwks_hits: Arc<AtomicUsize>,
        _shutdown: tokio::sync::oneshot::Sender<()>,
    }

    impl MockIdp {
        async fn start(body: Arc<Mutex<String>>) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let jwks_hits = Arc::new(AtomicUsize::new(0));
            let hits = jwks_hits.clone();
            let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
            let base = format!("http://{addr}");
            tokio::spawn(async move {
                loop {
                    let accept = tokio::select! {
                        a = listener.accept() => a,
                        _ = &mut rx => return,
                    };
                    let Ok((mut sock, _)) = accept else { continue };
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.split_whitespace().nth(1).unwrap_or("/");
                    let payload = if path.contains("well-known") {
                        format!("{{\"issuer\":\"{base}\",\"jwks_uri\":\"{base}/keys\"}}")
                    } else {
                        hits.fetch_add(1, Ordering::SeqCst);
                        body.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                }
            });
            Self {
                addr,
                jwks_hits,
                _shutdown: tx,
            }
        }

        fn base(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    fn provider_for(issuers: Vec<TrustedIssuer>) -> JwksProvider {
        let shared = SharedJwksStore::new(JwksStore::default());
        JwksProvider::new(shared, issuers, JwksFetcher::new().unwrap())
            .with_cooldown(Duration::from_millis(0))
    }

    #[tokio::test]
    async fn fetch_loads_keys_from_mock() {
        let mock = MockIdp::start(Arc::new(Mutex::new(TEST_JWKS.to_owned()))).await;
        let provider = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some(format!("{}/keys", mock.base())),
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        let report = provider.refresh_all().await;
        assert_eq!(report.issuers_ok, 1);
        assert_eq!(report.keys_total, 1);
        assert!(provider
            .shared()
            .load()
            .contains_kid("https://idp.test", "kid-1"));
    }

    #[tokio::test]
    async fn iss_differs_from_jwks_uri() {
        // The load-bearing nuance (spec §2.1): the token's `iss` is the
        // EXTERNAL url; the fetch targets the INTERNAL jwks_uri. The store
        // must be keyed by the external iss, not the fetch host.
        let mock = MockIdp::start(Arc::new(Mutex::new(TEST_JWKS.to_owned()))).await;
        let external_iss = "http://aws-tenant-demo-ephemeral/git";
        let provider = provider_for(vec![TrustedIssuer {
            iss: external_iss.to_owned(),
            jwks_uri: Some(format!("{}/keys", mock.base())), // internal
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        provider.refresh_all().await;
        let store = provider.shared();
        // keyed by the external iss…
        assert!(store.load().contains_kid(external_iss, "kid-1"));
        // …NOT by the internal fetch host.
        assert!(!store.load().contains_kid(&mock.base(), "kid-1"));
        let counts = store.load().key_counts_by_issuer();
        assert_eq!(counts, vec![(external_iss.to_owned(), 1)]);
    }

    #[tokio::test]
    async fn discovery_resolves_jwks_uri_when_absent() {
        // Mono-DNS case: no explicit jwks_uri → discover via the issuer's
        // .well-known. The mock's iss is its own base url.
        let mock = MockIdp::start(Arc::new(Mutex::new(TEST_JWKS.to_owned()))).await;
        let provider = provider_for(vec![TrustedIssuer {
            iss: mock.base(),
            jwks_uri: None, // force discovery
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        let report = provider.refresh_all().await;
        assert_eq!(report.issuers_ok, 1);
        assert!(provider.shared().load().contains_kid(&mock.base(), "kid-1"));
    }

    #[tokio::test]
    async fn fail_closed_when_issuer_unreachable() {
        // jwks_uri points at a dead port → fetch fails → store stays
        // empty (deny-all). Never fail-open.
        let provider = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some("http://127.0.0.1:1/keys".to_owned()),
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        let report = provider.refresh_all().await;
        assert_eq!(report.issuers_ok, 0);
        assert_eq!(report.keys_total, 0);
        assert!(!provider
            .shared()
            .load()
            .contains_kid("https://idp.test", "kid-1"));
    }

    #[tokio::test]
    async fn transient_failure_does_not_regress_live_keys() {
        // A live key must survive a later fetch failure (no regression to
        // empty — the wire mirror of reload_jwks_read_error_keeps_live_store).
        let body = Arc::new(Mutex::new(TEST_JWKS.to_owned()));
        let mock = MockIdp::start(body.clone()).await;
        let good_uri = format!("{}/keys", mock.base());
        let provider = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some(good_uri),
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        provider.refresh_all().await;
        assert!(provider
            .shared()
            .load()
            .contains_kid("https://idp.test", "kid-1"));

        // Now make the same issuer unreachable and refresh again.
        let dead = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some("http://127.0.0.1:1/keys".to_owned()),
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        // Seed the dead provider's store with the live key, then fail.
        dead.shared.store((**provider.shared().load()).clone());
        dead.refresh_all().await;
        assert!(
            dead.shared()
                .load()
                .contains_kid("https://idp.test", "kid-1"),
            "a transient fetch failure must keep the prior key",
        );
    }

    #[tokio::test]
    async fn cache_miss_refetch_brings_rotated_key() {
        // Store starts with kid-1 only. Upstream rotates to kid-2. A
        // request for kid-2 (cache-miss) triggers a targeted refetch that
        // brings the new key — without waiting for the TTL tick.
        let body = Arc::new(Mutex::new(TEST_JWKS.to_owned()));
        let mock = MockIdp::start(body.clone()).await;
        let provider = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some(format!("{}/keys", mock.base())),
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        provider.refresh_all().await;
        assert!(provider
            .shared()
            .load()
            .contains_kid("https://idp.test", "kid-1"));
        assert!(!provider
            .shared()
            .load()
            .contains_kid("https://idp.test", "kid-2"));

        // Rotate upstream.
        *body.lock().unwrap() = TEST_JWKS_ROTATED.to_owned();

        // Cache-miss on kid-2 → refetch → present.
        let ok = provider.ensure_kid("https://idp.test", "kid-2").await;
        assert!(ok, "cache-miss refetch should bring the rotated key");
        assert!(provider
            .shared()
            .load()
            .contains_kid("https://idp.test", "kid-2"));
    }

    #[tokio::test]
    async fn cache_miss_never_contacts_untrusted_issuer() {
        // ensure_kid for an issuer absent from the allowlist must NOT
        // fetch and must report the key as missing (deny).
        let provider = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some("http://127.0.0.1:1/keys".to_owned()),
            audiences: vec![],
        }]);
        let ok = provider.ensure_kid("https://evil.test", "kid-x").await;
        assert!(!ok);
        assert!(!provider.refresh_issuer("https://evil.test").await);
    }

    #[tokio::test]
    async fn cache_miss_cooldown_caps_refetch_rate() {
        // With a non-zero cooldown, a second cache-miss for the same
        // issuer inside the window must NOT hit the endpoint again.
        let body = Arc::new(Mutex::new(TEST_JWKS.to_owned()));
        let mock = MockIdp::start(body.clone()).await;
        let provider = JwksProvider::new(
            SharedJwksStore::new(JwksStore::default()),
            vec![TrustedIssuer {
                iss: "https://idp.test".to_owned(),
                jwks_uri: Some(format!("{}/keys", mock.base())),
                audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
            }],
            JwksFetcher::new().unwrap(),
        )
        .with_cooldown(Duration::from_secs(60));
        // First miss (kid-9 absent) → one fetch.
        provider.ensure_kid("https://idp.test", "kid-9").await;
        let after_first = mock.jwks_hits.load(Ordering::SeqCst);
        // Second miss inside the cooldown → no extra fetch.
        provider.ensure_kid("https://idp.test", "kid-9").await;
        let after_second = mock.jwks_hits.load(Ordering::SeqCst);
        assert_eq!(
            after_first, after_second,
            "cooldown must suppress a back-to-back refetch (anti-amplification)",
        );
    }

    #[tokio::test]
    async fn fetched_key_validates_a_real_token() {
        // End-to-end: a token signed by the matching private key validates
        // against the HTTP-fetched JWKS — proving the fetch seam loads
        // crypto-usable keys, not just counts.
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let priv_pem = include_str!("../tests/fixtures/test_rsa_private.pem");
        let mock = MockIdp::start(Arc::new(Mutex::new(TEST_JWKS.to_owned()))).await;
        let provider = provider_for(vec![TrustedIssuer {
            iss: "https://idp.test".to_owned(),
            jwks_uri: Some(format!("{}/keys", mock.base())),
            audiences: vec!["cosmon-rpp-tenant-demo".to_owned()],
        }]);
        provider.refresh_all().await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = serde_json::json!({
            "iss": "https://idp.test",
            "sub": "sub-1",
            "aud": "cosmon-rpp-tenant-demo",
            "iat": now,
            "exp": now + 60,
            "jti": "tok-1",
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("kid-1".into());
        let key = EncodingKey::from_rsa_pem(priv_pem.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        let v = JwtVerifier::validate(&provider.shared().load(), &token, Posture::Prepared)
            .expect("fetched key should validate the token");
        assert_eq!(v.sub, "sub-1");
    }

    #[test]
    fn trusted_issuers_load_missing_is_empty() {
        let td = tempfile::tempdir().unwrap();
        let ti = TrustedIssuers::load(td.path()).unwrap();
        assert!(ti.is_empty());
    }

    #[test]
    fn trusted_issuers_parse_two_field_shape() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("security");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("trusted-issuers.toml"),
            r#"
[[issuer]]
iss = "http://host/git"
jwks_uri = "http://forgejo:3000/login/oauth/keys"
audiences = ["cosmon-rpp-tenant-demo"]

[[issuer]]
iss = "https://accounts.google.com"
audiences = ["cosmon-rpp-speck"]
"#,
        )
        .unwrap();
        let ti = TrustedIssuers::load(td.path()).unwrap();
        assert_eq!(ti.len(), 2);
        assert_eq!(ti.issuers[0].iss, "http://host/git");
        assert_eq!(
            ti.issuers[0].jwks_uri.as_deref(),
            Some("http://forgejo:3000/login/oauth/keys")
        );
        // Google entry: discovery (no explicit jwks_uri).
        assert_eq!(ti.issuers[1].jwks_uri, None);
    }
}
