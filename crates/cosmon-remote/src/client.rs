// SPDX-License-Identifier: AGPL-3.0-only

//! HTTP client for the cosmon-rpp v1 surface.
//!
//! One method per consumed route. The routes themselves are NOT listed
//! here (a prose list would be a third hand-maintained copy of the
//! surface): every `/v1/` route this client dials is a
//! generated const from [`crate::canon`], projected at build time from
//! the §8p surface canon. [`crate::canon::ROUTES_USED`] is the
//! authoritative enumeration; `tests/surface_bijection.rs` gates it.
//! The only literal paths left are `/healthz` (operational endpoint,
//! deliberately outside the `/v1/` canon) and the OIDC `/issue` helper
//! (issuer-side, not an adapter route).
//!
//! ## OIDC mint
//!
//! The client also exposes a minimal `mint_jwt` helper that talks to a
//! `cs-oidc-mock`-style issuer at `<profile.oidc_url>/issue?sub=…&aud=…&scopes=…`.
//! In production deployments where the issuer is a real OIDC IdP, the
//! caller passes the token directly via `--token` or `$COSMON_REMOTE_TOKEN`
//! and `mint_jwt` is not used. The CLI never persists minted tokens to
//! disk — they live in process memory only.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cosmon_core::kind::MoleculeKind;
use reqwest::{header, Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::canon::{self, CanonRoute};
use crate::config::Profile;
use crate::credential::{CredentialKey, CredentialStore};
use crate::error::{Error, Result};
use crate::oidc::{self, OidcError, RefreshConfig, RefreshRotation, TokenState};

/// A molecule `kind` as it arrives on the wire.
///
/// The bug this
/// type fixes was re-stating the closed kind set as a bare `String` with a
/// `#[serde(default)]` silencer: a missing/garbled `kind` quietly became
/// `""`, a lie deep in the call graph. The cure the panel converged on —
/// *delete the copy, share the type* — lands here:
///
/// - **Developer-side drift** (a second hand-typed enum that can fall out of
///   sync with [`cosmon_core::kind::MoleculeKind`]) is made *impossible*: the
///   known set IS the shared enum. Add a kind in `cosmon-core` and this
///   binary learns it for free; there is no second list to update.
/// - **Runtime version skew** is the one genuine boundary this crate crosses
///   (`cosmon-remote` is a separately-versioned, downloaded binary talking to
///   a remote server — gödel class (b)). An old client meeting a server that
///   emits a *newer* kind degrades to [`MoleculeKindWire::Unknown`] carrying
///   the raw token verbatim, never a crash and never a lost value.
///
/// A *missing* `kind` field is not represented here at all — it surfaces as
/// `None` on [`MoleculeView::kind`], which is honest about absence (the
/// domain's `MoleculeData.kind` is itself `Option`), rather than papered over
/// with a fabricated `""`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MoleculeKindWire {
    /// A kind this binary knows — the shared closed enum from `cosmon-core`.
    Known(MoleculeKind),
    /// A kind value newer than this binary (runtime version skew). Carried
    /// verbatim so `--json` round-trips and the operator still sees the
    /// real token, never silently dropped.
    Unknown(String),
}

impl fmt::Display for MoleculeKindWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(k) => write!(f, "{k}"),
            Self::Unknown(s) => f.write_str(s),
        }
    }
}

/// One molecule projection, mirrors `MoleculeView` in the OpenAPI.
///
/// `additionalProperties: true` on the wire — we capture the typed
/// fields and stash the rest under `extra` so a future adapter that
/// emits new fields does not force a CLI bump. `#[non_exhaustive]` so a
/// future typed field promoted out of `extra` is a minor semver bump for
/// downstream library consumers, not a breaking change.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MoleculeView {
    pub id: String,
    /// Molecule kind, decoded into the shared [`MoleculeKind`] enum (B1
    /// collapse). `None` when the wire carried no `kind` — never a fake
    /// `""`. No `#[serde(default)]`: an `Option` field is already absent-as-
    /// `None` by serde's own rule, so there is no silencer to hide a typed
    /// drift. A *present but malformed* `kind` is now a loud deserialize
    /// error, exactly as the panel demanded. See [`MoleculeKindWire`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<MoleculeKindWire>,
    pub status: String,
    #[serde(default)]
    pub last_5_events: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl MoleculeView {
    /// Human-readable kind label for table output — the kind string when
    /// the wire carried one, `"—"` when it did not. Keeps the absent case
    /// visibly empty rather than masquerading as a real (empty) kind.
    #[must_use]
    pub fn kind_label(&self) -> String {
        self.kind
            .as_ref()
            .map_or_else(|| "—".to_owned(), ToString::to_string)
    }
}

/// Wraps every single-molecule response (`{request_id, molecule}`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MoleculeEnvelope {
    pub request_id: String,
    pub molecule: MoleculeView,
}

/// Wraps the ensemble list response (`{request_id, ensemble: {molecules: […]}}`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnsembleEnvelope {
    pub request_id: String,
    pub ensemble: serde_json::Value,
}

impl EnsembleEnvelope {
    /// Strict accessor for the `molecules` array. Returns an empty slice
    /// if the field is absent (the adapter omits it when there are zero
    /// molecules in some code paths).
    pub fn molecules(&self) -> Vec<MoleculeView> {
        self.ensemble
            .get("molecules")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value::<MoleculeView>(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Wire shape returned by `GET /v1/quota`.
///
/// Mirrors the server-side `QuotaResponse` — kept as a stand-alone
/// deserialise target so the CLI never needs to crack open the JSON
/// map by hand.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuotaResponse {
    /// Audit-correlation id for the inbox-materialised admission row
    /// of this `/v1/quota` call.
    pub request_id: String,
    /// Bucket configuration block (capacity + leak rate).
    pub limits: QuotaLimits,
    /// Tenant-specific snapshot block (current level).
    pub current: QuotaCurrent,
    /// `floor(burst_capacity − bucket_level)`. Equal to the value
    /// carried by the `X-RateLimit-Remaining` header on the same
    /// response.
    pub remaining: i64,
    /// ISO-8601 wall-clock at which the bucket will be fully drained
    /// back to zero. Equal to the `X-RateLimit-Reset` header.
    pub reset_at: String,
}

/// Bucket configuration block of [`QuotaResponse`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuotaLimits {
    /// Maximum tokens the bucket can hold (V0: 30).
    pub burst_capacity: i64,
    /// Tokens drained per minute (V0: 10).
    pub leak_per_minute: f64,
    /// Tokens drained per hour (V0: 600).
    pub leak_per_hour: f64,
}

/// Tenant-specific snapshot block of [`QuotaResponse`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuotaCurrent {
    /// Live bucket level, drained to the request's wall-clock.
    pub bucket_level: f64,
    /// Floor of `bucket_level`, useful for tables.
    pub bucket_level_floor: i64,
}

/// Wire shape returned by `GET /v1/workers`.
///
/// Mirrors the server-side `WorkersResponse` — kept as a stand-alone
/// deserialise target so the CLI never needs to crack open the JSON
/// map by hand.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkersResponse {
    /// Audit-correlation id for the inbox-materialised admission row
    /// of this `/v1/workers` call.
    pub request_id: String,
    /// Active workers in the caller's noyau, sorted by `started_at`
    /// ascending.
    pub workers: Vec<WorkerEntry>,
    /// `workers.len()` — pre-computed server-side.
    pub count: usize,
}

/// One worker entry inside [`WorkersResponse`]. Stable additive-only
/// wire shape: adding a field is allowed; renaming or removing one is
/// a §8p break (the same contract as every other v1 response).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkerEntry {
    /// Molecule the worker is bound to.
    pub molecule_id: String,
    /// Worker identity stamped at `cs tackle` time.
    pub session_name: String,
    /// ISO-8601 instant when the worker process record was created.
    pub started_at: String,
    /// Operating-system PID, when the transport backend surfaced one.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Tmux session owning the worker process.
    pub tmux_session: String,
}

/// Body for `POST /v1/molecules` (nucleate).
#[derive(Debug, Clone, Default, Serialize)]
pub struct NucleateRequest {
    pub formula: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Body for `POST /v1/molecules/{id}/tags`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TagRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

/// Body for `POST /v1/molecules/{id}/collapse`.
#[derive(Debug, Clone, Serialize)]
pub struct CollapseRequest {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

/// Body for `POST /v1/molecules/{id}/freeze`, `…/thaw`, `…/stuck`.
#[derive(Debug, Clone, Serialize)]
pub struct ReasonRequest {
    pub reason: String,
}

/// Response of `POST /v1/molecules/{id}/tackle` (T9 remote-tackle V2 envelope).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TackleEnvelope {
    pub request_id: String,
    pub tackle: TackleBody,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TackleBody {
    pub molecule_id: String,
    pub worker_session: Option<String>,
    pub spawned_at: Option<String>,
}

/// Response of `POST /v1/molecules/{id}/run` (bounded drain,
/// ADR-124). 202-shaped: the drain was spawned,
/// not completed — progress arrives on `GET /v1/events`
/// (`drain.started` / `drain.terminated`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunEnvelope {
    pub request_id: String,
    pub drain: DrainStarted,
}

/// The `drain` body of [`RunEnvelope`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DrainStarted {
    /// Root molecule id the resident loop drains from.
    pub root: String,
    /// Always `started` today (202 contract).
    pub status: String,
    /// Server-resolved B1/B2/B3 bounds the loop runs under — read
    /// face only; the request never carries a bound.
    pub bounds: DrainBounds,
    /// Wall-clock deadline handed to the loop (`--timeout`, named
    /// exit `timeout` on expiry — I4).
    pub timeout_secs: u64,
    /// RFC3339 spawn timestamp.
    pub started_at: String,
}

/// The B1/B2/B3 bound triple echoed by `run` and `GET /v1/quota`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DrainBounds {
    /// B3 — action budget (well-founded measure; obligatory).
    pub budget: u64,
    /// B1 — max DAG depth.
    pub max_depth: u64,
    /// B2 — max molecules drained.
    pub max_molecules: u64,
}

/// One artifact entry in the manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArtifactEntry {
    pub name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub integrity: IntegrityHash,
    pub created_at: String,
    pub token: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IntegrityHash {
    pub algo: String,
    pub hex: String,
}

/// Manifest returned by `GET /v1/molecules/{id}/artifacts`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArtifactManifest {
    pub request_id: String,
    pub molecule_id: String,
    pub artifacts: Vec<ArtifactEntry>,
}

/// Envelope returned by `PUT /v1/molecules/{id}/artifacts/{name}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArtifactPushedEnvelope {
    pub request_id: String,
    pub artifact: ArtifactEntry,
}

/// The canonical deliverable returned by `GET /v1/molecules/{id}/result`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MoleculeResult {
    /// Source label — `result.md`, `synthesis.md`, or `artifact:<name>`.
    pub source: String,
    /// MIME type detected from the deliverable's filename.
    pub content_type: String,
    /// `utf8` (inline text) or `base64` (binary).
    pub encoding: String,
    /// The deliverable body — verbatim text for `utf8`, base64 for binary.
    pub content: String,
    /// Size of the deliverable on disk, in bytes.
    pub size_bytes: u64,
    /// Integrity hash of the bytes.
    pub integrity: IntegrityHash,
}

/// Liveness block — the raw signals the server reports alongside a
/// result, so the client can re-decide for itself instead of trusting
/// an opaque verdict. All fields tolerate absence: an older
/// server, or a molecule never tackled, simply omits them.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Liveness {
    /// Raw worker-process record, or `null` when no process exists.
    #[serde(default)]
    pub process: Option<serde_json::Value>,
    /// Freshness mark (today proxied by `last_progress_at`; C2 will make
    /// it a true worker heartbeat under this same stable name).
    #[serde(default)]
    pub heartbeat_at: Option<String>,
    /// When the molecule was tackled, or `null` if never.
    #[serde(default)]
    pub tackled_at: Option<String>,
    /// The stall decree in force, in seconds.
    #[serde(default)]
    pub stale_after_s: Option<u64>,
}

/// Envelope returned by `GET /v1/molecules/{id}/result`.
///
/// The route returns 200 for *any*
/// molecule that exists: `result` is `null` when no deliverable is
/// resolved, and `result_status` carries the derived six-state verdict
/// (`pending` · `running` · `ready` · `done-no-deliverable` · `stalled`
/// · `failed`). The bare 404 only survives for an absent molecule.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResultEnvelope {
    pub request_id: String,
    pub molecule_id: String,
    /// Molecule lifecycle status at read time (e.g. `completed`,
    /// `running`) — raw, unchanged, kept for back-compat.
    pub status: String,
    /// Derived result verdict (C1). `None` when talking to a pre-C1
    /// server that did not emit the field.
    #[serde(default)]
    pub result_status: Option<String>,
    /// Raw liveness signals (C1). `None` from a pre-C1 server.
    #[serde(default)]
    pub liveness: Option<Liveness>,
    /// The canonical deliverable, or `None` when none is resolved.
    #[serde(default)]
    pub result: Option<MoleculeResult>,
}

/// PKCE auth-claude session response shape.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthStartResponse {
    pub session_id: String,
    pub state: String,
    /// Session deadline. The `/start` route names this `ttl_at` on the
    /// wire (`expires_at` only appears once a PKCE deadline exists,
    /// from `/email` onward).
    pub ttl_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthEmailResponse {
    pub verification_url: String,
    pub state: String,
    pub expires_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthSessionStatus {
    pub state: String,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `GET /v1/noyaux` discovery envelope.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoyauxResponse {
    /// One entry per noyau visible to the JWT's `sub`. Empty when the
    /// principal carries a valid JWT but has no nucleon binding.
    pub noyaux: Vec<NoyauEntry>,
    /// Future-proof — server may add top-level fields without an API
    /// bump (additive evolution).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Single row of [`NoyauxResponse::noyaux`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoyauEntry {
    /// Noyau identifier (e.g. `"tenant-demo-sandbox"`).
    pub id: String,
    /// Number of `(iss, sub) → noyau` bindings backing this noyau
    /// for the JWT's `sub`. ≥ 1 by construction.
    pub binding_count: u64,
    /// Absolute path of the noyau's galaxy tree on the adapter host
    /// (`<galaxies_root>/<id>`).
    pub galaxies_root: String,
    /// Future-proof — server may add per-row fields without an API
    /// bump.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `GET /v1/auth/me` whoami envelope.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthMeResponse {
    pub sub: String,
    #[serde(default)]
    pub aud: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub noyau: Option<String>,
    pub expires_at: String,
    pub issuer: String,
    /// Worker-glasses signal: whether the container's
    /// Claude credentials file exists. `None` both when the server
    /// predates the field and when the deployment has no auth-claude
    /// surface — `doctor` renders that honestly as "unknown", never as
    /// green or red.
    #[serde(default)]
    pub claude_credentials_present: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `cs-oidc-mock` `/issue` response — `{access_token, ...}`.
#[derive(Debug, Clone, Deserialize)]
pub struct MintedJwt {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// The thin HTTP client. Reads from a [`Profile`]; carries a single
/// bearer token (minted or operator-supplied); never persists to disk.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    oidc_base: String,
    sub: String,
    aud: String,
    token: Option<String>,
    /// Spool dir for the passive opt-out remontée. `None` disables it
    /// (profile gesture `config set phone-home off`, or no config dir).
    phone_home_dir: Option<std::path::PathBuf>,
    /// The optional reactive-refresh binding (delib-20260710-33b7 C2,
    /// kahneman-F7). Present only for real-OIDC profiles: it lets an
    /// authenticated request that the server rejects with `401` — despite a
    /// locally-valid bearer whose clock was ahead of ours past the proactive
    /// leeway — force one silent refresh and retry. `None` for env / mock /
    /// operator-supplied-token profiles, where a `401` is surfaced verbatim.
    /// Shared behind an `Arc` so [`Client`] stays cheaply [`Clone`].
    reauth: Option<Arc<ReactiveRefresh>>,
}

/// The binding that lets a [`Client`] recover from a reactive `401` by forcing a
/// silent OAuth refresh (delib-20260710-33b7 C2, kahneman-F7).
///
/// The *proactive* refresh (on the 15-minute expiry boundary) is the dominant
/// path and lives in [`crate::oidc::ensure_token`]; this binding is the
/// belt-and-suspenders guard for the residual case the local wall-clock cannot
/// see — the server's clock is ahead of ours by more than
/// [`crate::oidc::REFRESH_LEEWAY_SECS`], so a token we still believe fresh comes
/// back `401`. It holds everything [`crate::oidc::force_refresh`] needs, but
/// resolves the token endpoint **lazily** (only on an actual `401`) so the
/// zero-network fast path is never burdened with a discovery round-trip.
pub struct ReactiveRefresh {
    http: reqwest::Client,
    store: CredentialStore,
    key: CredentialKey,
    /// The OIDC issuer base, from which the token endpoint is discovered on
    /// demand — kept instead of a pre-resolved [`RefreshConfig`] so building the
    /// binding costs no network on the fast path.
    oidc_url: String,
    client_id: String,
}

impl fmt::Debug for ReactiveRefresh {
    /// Redacted: the binding is adjacent to secret material (it can mint bearers)
    /// so it prints only the non-sensitive routing fields, never the store's
    /// contents or a token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReactiveRefresh")
            .field("oidc_url", &self.oidc_url)
            .field("client_id", &self.client_id)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl ReactiveRefresh {
    /// Assemble a reactive-refresh binding for a real-OIDC profile. `http` is
    /// reused for the refresh grant; `store`/`key` locate the persisted
    /// credential; `oidc_url`/`client_id` resolve the token endpoint lazily.
    pub fn new(
        http: reqwest::Client,
        store: CredentialStore,
        key: CredentialKey,
        oidc_url: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Self {
        Self {
            http,
            store,
            key,
            oidc_url: oidc_url.into(),
            client_id: client_id.into(),
        }
    }

    /// Force one refresh after a server `401`, returning the fresh access token
    /// to retry with — or `None` when no token can be recovered (the store is
    /// cold, or the refresh token is spent), in which case the caller surfaces
    /// the original `401` and the operator re-runs `login`. A peer's
    /// freshly-rotated token already on disk is adopted with zero network
    /// (via [`crate::oidc::force_refresh`]'s compare-and-swap).
    async fn refreshed_bearer(&self, presented_access: &str) -> Result<Option<String>> {
        // Resolve the token endpoint only now (a real 401), never on the fast path.
        let cfg = RefreshConfig {
            token_endpoint: oidc::ProviderMetadata::fetch(&self.http, &self.oidc_url)
                .await?
                .token_endpoint,
            client_id: self.client_id.clone(),
            // Same Forgejo target as the proactive path: single-use rotation.
            rotation: RefreshRotation::Rotating,
        };
        match oidc::force_refresh(&self.http, &self.store, &self.key, &cfg, presented_access).await
        {
            Ok(TokenState::Valid(token)) => Ok(Some(token.expose().to_owned())),
            // Nothing to retry with — the store is cold or the refresh token is
            // spent — so the caller surfaces the original 401 and the operator
            // re-runs `login`.
            Ok(TokenState::NeedsLogin) | Err(Error::Oidc(OidcError::RefreshExpired)) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Client {
    /// Build a client from a ready profile. `token` is the bearer JWT;
    /// passing `None` leaves the client unauthenticated (useful for
    /// `/healthz` and `mint_jwt`).
    pub fn new(profile: &Profile, token: Option<String>) -> Result<Self> {
        profile.check_ready()?;
        Self::new_unchecked(profile, token)
    }

    /// Variant that skips `check_ready` — used by `mint_jwt`-only
    /// flows where the operator may legitimately not have set every
    /// field yet.
    pub fn new_unchecked(profile: &Profile, token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(profile.timeout_secs))
            .build()?;
        Ok(Self {
            http,
            base: profile.host.trim_end_matches('/').to_owned(),
            oidc_base: profile.oidc_url.trim_end_matches('/').to_owned(),
            sub: profile.sub.clone(),
            aud: profile.aud.clone(),
            token,
            phone_home_dir: if profile.phone_home {
                crate::phone_home::dir()
            } else {
                None
            },
            reauth: None,
        })
    }

    /// Inject (or replace) the bearer token after construction.
    #[must_use]
    pub fn with_token(mut self, token: String) -> Self {
        self.token = Some(token);
        self
    }

    /// Attach a reactive-refresh binding, so a server `401` on a locally-valid
    /// bearer triggers one silent refresh and a single retry (delib-33b7 C2,
    /// kahneman-F7). Wired by `client_for` for real-OIDC profiles; absent for
    /// env / mock / operator-token profiles.
    #[must_use]
    pub fn with_reauth(mut self, reauth: ReactiveRefresh) -> Self {
        self.reauth = Some(Arc::new(reauth));
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Helper: build an authenticated request, attaching the bearer
    /// header when a token is present.
    ///
    /// Also lets any pending phone-home report ride along (passive
    /// opt-out remontée): only `request_id:code` pairs, drained for
    /// this host, gated by the profile's `phone_home` flag at
    /// construction time. Best-effort and silent — the remontée never
    /// affects the command's own outcome.
    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let mut rb = self.http.request(method, self.url(path));
        if let Some(token) = &self.token {
            rb = rb.bearer_auth(token);
        }
        if let Some(dir) = &self.phone_home_dir {
            if let Some(value) = crate::phone_home::drain_for_header(dir, &self.base) {
                rb = rb.header(crate::phone_home::HEADER, value);
            }
        }
        rb
    }

    /// Build an authenticated request against a canon route, with the
    /// path placeholders substituted in order. The only way client
    /// methods reach a `/v1/` route — the method/path tuple comes from
    /// the canon const, never from a literal.
    fn req_canon(&self, route: &CanonRoute, args: &[&str]) -> reqwest::RequestBuilder {
        let method = Method::from_bytes(route.method.as_bytes())
            .expect("canon methods are validated by cosmon-surface-canon");
        self.req(method, &route.path_with(args))
    }

    /// Send an authenticated request, transparently recovering from a single
    /// reactive `401` (delib-20260710-33b7 C2, kahneman-F7).
    ///
    /// The dominant expiry path is *proactive* — every command reads the
    /// credential store and refreshes on the 15-minute boundary before it ever
    /// dials the server. This seam covers the residual case that path cannot see:
    /// the server rejects a bearer we still believe fresh because its clock is
    /// ahead of ours past [`crate::oidc::REFRESH_LEEWAY_SECS`]. On a `401`, and
    /// only when a [`ReactiveRefresh`] binding is attached, it forces one silent
    /// refresh, swaps the bearer, and retries the request **exactly once**.
    ///
    /// The retry is built by cloning the already-built [`reqwest::Request`] and
    /// **replacing** its `Authorization` header (`HeaderMap::insert`, not
    /// `append` — no duplicate header), so the fresh bearer is presented cleanly.
    /// If the body is not clonable (a stream), or no reauth binding is present,
    /// or the refresh recovers no token, the original response is returned
    /// unchanged — the caller then surfaces the `401` and the operator re-runs
    /// `login`. A refresh is never attempted more than once per request, so a
    /// server that `401`s regardless cannot loop.
    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let request = rb.build()?;
        // Preserve a clone for the one permitted retry (None ⇒ streaming body).
        let retry = request.try_clone();
        let resp = self.http.execute(request).await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        let (Some(reauth), Some(mut retry)) = (self.reauth.as_ref(), retry) else {
            return Ok(resp);
        };
        let presented = self.token.as_deref().unwrap_or_default();
        let Some(fresh) = reauth.refreshed_bearer(presented).await? else {
            // No fresh token recovered — surface the original 401 verbatim.
            return Ok(resp);
        };
        let mut value = header::HeaderValue::from_str(&format!("Bearer {fresh}"))
            .map_err(|_| Error::Config("refreshed bearer is not a valid header value".into()))?;
        value.set_sensitive(true);
        retry.headers_mut().insert(header::AUTHORIZATION, value);
        Ok(self.http.execute(retry).await?)
    }

    // ── /healthz ───────────────────────────────────────────────────────

    /// `GET /healthz` — unauthenticated probe. Returns the raw JSON body.
    pub async fn healthz(&self) -> Result<serde_json::Value> {
        let resp = self.http.get(self.url("/healthz")).send().await?;
        decode_json(resp).await
    }

    // ── OIDC mint (cs-oidc-mock style) ────────────────────────────────

    /// Mint a JWT against the deployment's OIDC issuer. Mirrors the
    /// `auth-claude-jwt` / `mol-jwt` recipes of the Phase 0 justfile.
    /// `scopes` is space-joined and URL-encoded by reqwest.
    pub async fn mint_jwt(&self, scopes: &[String]) -> Result<MintedJwt> {
        if self.oidc_base.is_empty() {
            return Err(Error::Config(
                "oidc_url not set in profile; cannot mint a JWT".into(),
            ));
        }
        if self.sub.is_empty() || self.aud.is_empty() {
            return Err(Error::Config(
                "sub/aud not set in profile; cannot mint a JWT".into(),
            ));
        }
        let scope_param = scopes.join(" ");
        let url = format!("{}/issue", self.oidc_base);
        let resp = self
            .http
            .post(url)
            .query(&[
                ("sub", self.sub.as_str()),
                ("aud", self.aud.as_str()),
                ("scopes", scope_param.as_str()),
            ])
            .send()
            .await?;
        decode_json(resp).await
    }

    // ── Molecules ─────────────────────────────────────────────────────

    /// `POST /v1/molecules` — nucleate.
    pub async fn nucleate(&self, body: &NucleateRequest) -> Result<MoleculeEnvelope> {
        let resp = self
            .send(self.req_canon(canon::POST_V1_MOLECULES, &[]).json(body))
            .await?;
        decode_json(resp).await
    }

    /// `GET /v1/molecules` — ensemble list with optional filters.
    pub async fn list_molecules(&self, filters: &ListFilters) -> Result<EnsembleEnvelope> {
        let mut rb = self.req_canon(canon::GET_V1_MOLECULES, &[]);
        rb = filters.attach(rb);
        decode_json(self.send(rb).await?).await
    }

    /// `GET /v1/quota` — rate-limit snapshot. Returns the typed
    /// [`QuotaResponse`] so the CLI can format a table without
    /// cracking open a `serde_json::Value`.
    pub async fn quota(&self) -> Result<QuotaResponse> {
        let resp = self.send(self.req_canon(canon::GET_V1_QUOTA, &[])).await?;
        decode_json(resp).await
    }

    /// `GET /v1/workers` — list active workers in the caller's noyau.
    /// Returns the typed [`WorkersResponse`] envelope; the CLI can then
    /// render a table or pass through to `--json` without cracking
    /// open a `serde_json::Value`.
    pub async fn workers(&self) -> Result<WorkersResponse> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_WORKERS, &[]))
            .await?;
        decode_json(resp).await
    }

    /// `GET /v1/molecules/{id}` — observe.
    pub async fn get_molecule(&self, id: &str) -> Result<MoleculeEnvelope> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_MOLECULES_ID, &[id]))
            .await?;
        decode_json(resp).await
    }

    /// `GET /v1/molecules/{id}/result` — fetch the canonical deliverable.
    ///
    /// Reads the molecule's persistent dir (synthesis.md / result.md)
    /// with the ephemeral artifact dir as fallback, so a tackled
    /// molecule's output is retrievable even when the worker never wrote
    /// into `COSMON_ARTIFACT_DIR`.
    pub async fn get_result(&self, id: &str) -> Result<ResultEnvelope> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_MOLECULES_ID_RESULT, &[id]))
            .await?;
        decode_json(resp).await
    }

    /// `POST /v1/molecules/{id}/tackle` — dispatch a worker.
    pub async fn tackle(&self, id: &str) -> Result<TackleEnvelope> {
        let resp = self
            .send(self.req_canon(canon::POST_V1_MOLECULES_ID_TACKLE, &[id]))
            .await?;
        decode_json(resp).await
    }

    /// `POST /v1/molecules/{id}/run` — request the resident drain of
    /// the DAG rooted at `id` (B2 bounded drain, ADR-124). The server
    /// answers 202 as soon as the loop is spawned; the bounds in the
    /// envelope are binding-resolved and read-only.
    pub async fn run(&self, id: &str) -> Result<RunEnvelope> {
        let resp = self
            .send(self.req_canon(canon::POST_V1_MOLECULES_ID_RUN, &[id]))
            .await?;
        decode_json(resp).await
    }

    /// `POST /v1/molecules/{id}/tags`.
    pub async fn tag(&self, id: &str, body: &TagRequest) -> Result<serde_json::Value> {
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_MOLECULES_ID_TAGS, &[id])
                    .json(body),
            )
            .await?;
        decode_json(resp).await
    }

    /// `POST /v1/molecules/{id}/collapse`.
    pub async fn collapse(&self, id: &str, body: &CollapseRequest) -> Result<serde_json::Value> {
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_MOLECULES_ID_COLLAPSE, &[id])
                    .json(body),
            )
            .await?;
        decode_json(resp).await
    }

    /// Freeze (pause) a molecule via the fused freeze route.
    ///
    /// Fusion v1.0.0-rc made `state` mandatory in
    /// the wire body; the pre-fusion client still sent `{reason}` alone
    /// and got a 400 — the exact drift class (a delivered binary covered
    /// by no test) that triggered the tenant-CLI fusion.
    pub async fn freeze(&self, id: &str, reason: Option<&str>) -> Result<serde_json::Value> {
        self.set_freeze_state(id, "frozen", reason).await
    }

    /// Thaw (resume) a molecule. Dispatches to the same fused freeze
    /// route with `state: "active"` — the legacy `/thaw` endpoint the
    /// pre-A2 client dialled returns 410 Gone.
    pub async fn thaw(&self, id: &str, reason: Option<&str>) -> Result<serde_json::Value> {
        self.set_freeze_state(id, "active", reason).await
    }

    async fn set_freeze_state(
        &self,
        id: &str,
        state: &str,
        reason: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut body = serde_json::json!({ "state": state });
        if let Some(r) = reason {
            body["reason"] = serde_json::Value::String(r.to_owned());
        }
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_MOLECULES_ID_FREEZE, &[id])
                    .json(&body),
            )
            .await?;
        decode_json(resp).await
    }

    /// `POST /v1/molecules/{id}/stuck`.
    pub async fn stuck(&self, id: &str, body: &ReasonRequest) -> Result<serde_json::Value> {
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_MOLECULES_ID_STUCK, &[id])
                    .json(body),
            )
            .await?;
        decode_json(resp).await
    }

    // ── Artifacts (e653) ──────────────────────────────────────────────

    /// `GET /v1/molecules/{id}/artifacts`.
    pub async fn list_artifacts(&self, id: &str) -> Result<ArtifactManifest> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_MOLECULES_ID_ARTIFACTS, &[id]))
            .await?;
        decode_json(resp).await
    }

    /// `GET /v1/molecules/{id}/artifacts/{token}` — streams the bytes
    /// into `dest`. The directory of `dest` is created if absent.
    /// Returns the number of bytes written and the `Content-Type` header.
    pub async fn fetch_artifact(
        &self,
        id: &str,
        token: &str,
        dest: &Path,
    ) -> Result<FetchedArtifact> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_MOLECULES_ID_ARTIFACTS_TOKEN, &[id, token]))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(api_error_from(resp).await);
        }
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let bytes = resp.bytes().await?;
        let written = bytes.len() as u64;
        tokio::fs::write(dest, &bytes).await?;
        Ok(FetchedArtifact {
            dest: dest.to_path_buf(),
            bytes: written,
            content_type,
            etag,
        })
    }

    /// `PUT /v1/molecules/{id}/artifacts/{name}` — back-utterance.
    /// Reads `file` from disk, computes its BLAKE3 digest, and uploads
    /// with the proper RFC 9530 `Digest` header. `content_type` defaults
    /// to `application/octet-stream`.
    pub async fn push_artifact(
        &self,
        id: &str,
        name: &str,
        file: &Path,
        content_type: Option<&str>,
        if_match: Option<&str>,
    ) -> Result<ArtifactPushedEnvelope> {
        let bytes = tokio::fs::read(file).await?;
        let hex = blake3::hash(&bytes).to_hex().to_string();
        let mut rb = self
            .req_canon(canon::PUT_V1_MOLECULES_ID_ARTIFACTS_TOKEN, &[id, name])
            .header(
                header::CONTENT_TYPE,
                content_type.unwrap_or("application/octet-stream"),
            )
            .header(header::CONTENT_LENGTH, bytes.len())
            .header("Digest", format!("blake3={hex}"))
            .body(bytes);
        if let Some(prev) = if_match {
            rb = rb.header(header::IF_MATCH, prev);
        }
        decode_json(self.send(rb).await?).await
    }

    // ── Auth-claude (PKCE manual-paste) ──────────────────────────────

    pub async fn auth_start(&self) -> Result<AuthStartResponse> {
        let resp = self
            .send(self.req_canon(canon::POST_V1_AUTH_CLAUDE_START, &[]))
            .await?;
        decode_json(resp).await
    }

    pub async fn auth_email(&self, session_id: &str, email: &str) -> Result<AuthEmailResponse> {
        let body = serde_json::json!({
            "session_id": session_id,
            "email": email,
        });
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_AUTH_CLAUDE_EMAIL, &[])
                    .json(&body),
            )
            .await?;
        decode_json(resp).await
    }

    pub async fn auth_confirm(
        &self,
        session_id: &str,
        authorization_code: &str,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "session_id": session_id,
            "authorization_code": authorization_code,
        });
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_AUTH_CLAUDE_CONFIRM, &[])
                    .json(&body),
            )
            .await?;
        decode_json(resp).await
    }

    pub async fn auth_status(&self, session_id: &str) -> Result<AuthSessionStatus> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_AUTH_CLAUDE_SESSION_ID, &[session_id]))
            .await?;
        decode_json(resp).await
    }

    pub async fn auth_delete(&self, session_id: &str) -> Result<serde_json::Value> {
        let resp = self
            .send(self.req_canon(canon::DELETE_V1_AUTH_CLAUDE_SESSION_ID, &[session_id]))
            .await?;
        decode_json(resp).await
    }

    // ── Whoami (task-20260522-560a) ─────────────────────────────────

    /// `GET /v1/auth/me` — JWT introspection. The route requires only
    /// a valid bearer token; no scope is checked.
    pub async fn auth_me(&self) -> Result<AuthMeResponse> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_AUTH_ME, &[]))
            .await?;
        decode_json(resp).await
    }

    // ── Noyaux discovery (task-20260523-eb61) ───────────────────────

    /// `GET /v1/noyaux` — list noyaux visible to the JWT's `sub`. The
    /// route requires only a valid bearer token; no scope is checked.
    pub async fn noyaux(&self) -> Result<NoyauxResponse> {
        let resp = self.send(self.req_canon(canon::GET_V1_NOYAUX, &[])).await?;
        decode_json(resp).await
    }

    // ── D-AVATAR instance lifecycle (drained from cs-thin, A2 fusion) ─

    /// Read an avatar instance's lifecycle status. Returns the raw
    /// response envelope (`{request_id, avatar_status}`) — same opaque
    /// discipline as the cs-thin dispatch this replaces.
    pub async fn avatar_status(&self, instance_id: &str) -> Result<serde_json::Value> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_AVATAR_INSTANCE_ID_STATUS, &[instance_id]))
            .await?;
        decode_json(resp).await
    }

    /// Incarnate an avatar instance (bind moule → avatar).
    pub async fn avatar_incarnate(
        &self,
        instance_id: &str,
        pilote_id: &str,
        tenant_id: &str,
        juridiction: &str,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "pilote_id": pilote_id,
            "tenant_id": tenant_id,
            "juridiction": juridiction,
        });
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_AVATAR_INSTANCE_ID_INCARNATE, &[instance_id])
                    .json(&body),
            )
            .await?;
        decode_json(resp).await
    }

    /// Grant a canal to an avatar instance.
    pub async fn avatar_grant(
        &self,
        instance_id: &str,
        canal: &str,
        target: &str,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "canal": canal,
            "target": target,
        });
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_AVATAR_INSTANCE_ID_GRANT, &[instance_id])
                    .json(&body),
            )
            .await?;
        decode_json(resp).await
    }

    /// Read an avatar instance's audit trail (cicatrice + events).
    pub async fn avatar_audit(&self, instance_id: &str) -> Result<serde_json::Value> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_AVATAR_INSTANCE_ID_AUDIT, &[instance_id]))
            .await?;
        decode_json(resp).await
    }

    /// Read an avatar instance's pre-incarnation mould info.
    pub async fn avatar_mould_info(&self, instance_id: &str) -> Result<serde_json::Value> {
        let resp = self
            .send(self.req_canon(canon::GET_V1_AVATAR_INSTANCE_ID_MOULD_INFO, &[instance_id]))
            .await?;
        decode_json(resp).await
    }

    // ── D-AVATAR canal (b) — converse (task-20260610-0b57) ───────────

    /// Send a typed message to a bound avatar-tiers (canal (b)). The
    /// route is on-by-binding server-side: without an explicit
    /// operator binding for `avatar_id` the adapter refuses with
    /// `503 no_binding`. Synchronous `request` messages carry a `hop`
    /// counter; relay chains beyond the binding's bound are refused
    /// with `409 max_hops_exceeded` (L3 anti-cycle). `announce` is
    /// fire-and-forget and exempt from the bound.
    pub async fn converse(
        &self,
        avatar_id: &str,
        message: &serde_json::Value,
        kind: &str,
        hop: u32,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "avatar_id": avatar_id,
            "message": message,
            "kind": kind,
            "hop": hop,
        });
        let resp = self
            .send(
                self.req_canon(canon::POST_V1_AVATAR_CONVERSE, &[])
                    .json(&body),
            )
            .await?;
        decode_json(resp).await
    }

    // ── /v1/events (SSE) ──────────────────────────────────────────────

    /// `GET /v1/events` — open the SSE stream and invoke `on_event` for
    /// every parsed [`SseEvent`]. Runs until the server closes the
    /// connection or an I/O error occurs.
    ///
    /// The SSE wire format is line-oriented: every event ends with a
    /// blank line; lines `id:`, `event:`, `data:` set the matching
    /// field; lines starting with `:` are comments (keep-alive). The
    /// parser concatenates multi-line `data:` fields with `\n`
    /// following the W3C SSE spec.
    pub async fn events_stream<F>(
        &self,
        molecule_id: Option<&str>,
        last_event_id: Option<u64>,
        mut on_event: F,
    ) -> Result<()>
    where
        F: FnMut(SseEvent),
    {
        use futures_util::StreamExt;

        let mut url = self.url(canon::GET_V1_EVENTS.path);
        if let Some(m) = molecule_id {
            // Hand-rolled, single param; full-fat query encoding would
            // be a five-character win and a 200-byte loss.
            let enc = percent_encode_simple(m);
            url.push_str("?molecule_id=");
            url.push_str(&enc);
        }
        let mut rb = self.req_url(Method::GET, &url);
        rb = rb.header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(id) = last_event_id {
            rb = rb.header("Last-Event-ID", id.to_string());
        }
        let resp = self.send(rb).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let body_json = serde_json::from_str::<serde_json::Value>(&body)
                .unwrap_or(serde_json::Value::String(body));
            return Err(Error::Api {
                status: status.as_u16(),
                body: body_json,
            });
        }
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut current = SseEvent::default();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(Error::Http)?;
            // SSE chunks are valid UTF-8 by spec.
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            buf.push_str(text);
            // Process full lines; keep any trailing partial line in
            // `buf` for the next chunk.
            while let Some(nl) = buf.find('\n') {
                let line: String = buf.drain(..=nl).collect();
                let line = line.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    // End of one event — dispatch and reset.
                    if !current.is_empty() {
                        on_event(std::mem::take(&mut current));
                    }
                    continue;
                }
                if let Some(rest) = line.strip_prefix(':') {
                    // Comment / keep-alive — ignore but useful for
                    // operators tailing the stream in debug mode.
                    tracing::trace!(target: "cosmon_remote::sse", comment = rest);
                    continue;
                }
                let (field, value) = match line.split_once(':') {
                    Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                    None => (line, ""),
                };
                // Match the W3C SSE field set; everything else
                // (including `retry`, which V0 ignores — durable
                // reconnect-delay is a v2 concern) is a silent
                // no-op so a forward-compatible server can grow
                // new fields without breaking us.
                match field {
                    "id" => current.id = Some(value.to_owned()),
                    "event" => value.clone_into(&mut current.event),
                    "data" => {
                        if !current.data.is_empty() {
                            current.data.push('\n');
                        }
                        current.data.push_str(value);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Build a request against an absolute URL we already constructed
    /// (the SSE handler builds its own URL because reqwest's query
    /// builder does not give us deterministic ordering or escape rules
    /// for the SSE one-shot).
    fn req_url(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let mut rb = self.http.request(method, url);
        if let Some(token) = &self.token {
            rb = rb.bearer_auth(token);
        }
        rb
    }
}

/// Minimal percent-encoder for a query parameter value — covers the
/// subset that turns up in molecule ids (`-`, alphanumerics). Anything
/// else is encoded as `%XX`. Avoids pulling `urlencoding` for one call.
fn percent_encode_simple(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// One parsed SSE event.
///
/// The `data` field carries the raw JSON the adapter put on the wire
/// (the publisher embeds the structured payload as JSON in `data:`).
/// Consumers parse it into their own typed shape — the CLI stays
/// opaque to keep the binary independent of the adapter's data schema.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SseEvent {
    /// Monotonic adapter-wide event id (use as `Last-Event-ID` on
    /// reconnect to skip already-seen events).
    #[serde(default)]
    pub id: Option<String>,
    /// SSE event name — typically `molecule.state_changed` or
    /// `molecule.event_appended`.
    #[serde(default)]
    pub event: String,
    /// `data:` payload, verbatim. Empty if the event carried no data.
    #[serde(default)]
    pub data: String,
}

impl SseEvent {
    fn is_empty(&self) -> bool {
        self.event.is_empty() && self.data.is_empty() && self.id.is_none()
    }

    /// Try to parse `data` as JSON. Returns `None` for malformed
    /// payloads (the CLI then prints the raw string).
    #[must_use]
    pub fn data_obj(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.data).ok()
    }
}

/// Result of [`Client::fetch_artifact`].
#[derive(Debug, Clone)]
pub struct FetchedArtifact {
    pub dest: PathBuf,
    pub bytes: u64,
    pub content_type: Option<String>,
    pub etag: Option<String>,
}

/// Optional filters for `list_molecules`. All `Option<String>`.
#[derive(Debug, Clone, Default)]
pub struct ListFilters {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub fleet: Option<String>,
}

impl ListFilters {
    fn attach(&self, mut rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if let Some(s) = &self.status {
            pairs.push(("status", s.as_str()));
        }
        if let Some(k) = &self.kind {
            pairs.push(("kind", k.as_str()));
        }
        if let Some(t) = &self.tag {
            pairs.push(("tag", t.as_str()));
        }
        if let Some(f) = &self.fleet {
            pairs.push(("fleet", f.as_str()));
        }
        if !pairs.is_empty() {
            rb = rb.query(&pairs);
        }
        rb
    }
}

/// Decode a JSON success body, or convert a 4xx/5xx into [`Error::Api`].
async fn decode_json<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if status.is_success() {
        let body = resp.bytes().await?;
        return serde_json::from_slice(&body).map_err(Error::Json);
    }
    Err(api_error_from_status(status, resp).await)
}

async fn api_error_from(resp: reqwest::Response) -> Error {
    api_error_from_status(resp.status(), resp).await
}

async fn api_error_from_status(status: StatusCode, resp: reqwest::Response) -> Error {
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"error": "non_json_response"}));
    Error::Api {
        status: status.as_u16(),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_profile() -> Profile {
        Profile {
            host: "http://example.invalid".into(),
            sub: "s".into(),
            aud: "a".into(),
            oidc_url: "http://example.invalid".into(),
            issuer: None,
            client_id: None,
            noyau: None,
            scopes: vec!["cosmon:molecule:read".into()],
            artifacts_dir: None,
            timeout_secs: 5,
            phone_home: true,
        }
    }

    #[test]
    fn client_constructs_from_ready_profile() {
        let c = Client::new(&ready_profile(), Some("tok".into())).unwrap();
        assert_eq!(c.base, "http://example.invalid");
    }

    #[test]
    fn client_rejects_unset_profile() {
        let mut p = ready_profile();
        p.sub.clear();
        let err = Client::new(&p, None).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn list_filters_serialize() {
        let filters = ListFilters {
            status: Some("running".into()),
            kind: Some("task".into()),
            tag: None,
            fleet: None,
        };
        let client = reqwest::Client::new();
        let rb = client.get("http://x");
        let rb = filters.attach(rb);
        let req = rb.build().unwrap();
        let q = req.url().query().unwrap_or("");
        assert!(q.contains("status=running"));
        assert!(q.contains("kind=task"));
        assert!(!q.contains("tag="));
        assert!(!q.contains("fleet="));
    }

    #[test]
    fn ensemble_envelope_molecules_handles_missing_field() {
        let env = EnsembleEnvelope {
            request_id: "r".into(),
            ensemble: serde_json::json!({}),
        };
        assert!(env.molecules().is_empty());
    }

    #[test]
    fn ensemble_envelope_molecules_parses_array() {
        let env = EnsembleEnvelope {
            request_id: "r".into(),
            ensemble: serde_json::json!({
                "molecules": [
                    { "id": "task-1", "kind": "task", "status": "pending" },
                    { "id": "task-2", "kind": "task", "status": "running" }
                ]
            }),
        };
        let m = env.molecules();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, "task-1");
        assert_eq!(m[1].status, "running");
        assert_eq!(m[0].kind, Some(MoleculeKindWire::Known(MoleculeKind::Task)));
    }

    /// B1 collapse: a known `kind` decodes into the *shared* enum, not a
    /// hand-typed `String`. This is the single-source-of-truth contract —
    /// the known set is `cosmon_core::kind::MoleculeKind`.
    #[test]
    fn molecule_view_decodes_known_kind_into_shared_enum() {
        let v: MoleculeView = serde_json::from_value(serde_json::json!({
            "id": "delib-1",
            "kind": "deliberation",
            "status": "pending"
        }))
        .unwrap();
        assert_eq!(
            v.kind,
            Some(MoleculeKindWire::Known(MoleculeKind::Deliberation))
        );
        assert_eq!(v.kind_label(), "deliberation");
    }

    /// Runtime version skew: an old binary meeting a server that emits a
    /// kind it predates degrades to `Unknown(raw)` — never a crash, never a
    /// lost token. This is the one genuine cross-version boundary (gödel
    /// class (b)) this frontier crate crosses.
    #[test]
    fn molecule_view_tolerates_unknown_kind_value() {
        let v: MoleculeView = serde_json::from_value(serde_json::json!({
            "id": "x-1",
            "kind": "quasar",
            "status": "pending"
        }))
        .unwrap();
        assert_eq!(v.kind, Some(MoleculeKindWire::Unknown("quasar".to_owned())));
        assert_eq!(v.kind_label(), "quasar");
    }

    /// A *missing* `kind` is honest `None` — not the fabricated `""` the
    /// `#[serde(default)] String` silencer used to produce. `kind_label`
    /// renders it as a visibly-absent `"—"`.
    #[test]
    fn molecule_view_missing_kind_is_none_not_empty_string() {
        let v: MoleculeView = serde_json::from_value(serde_json::json!({
            "id": "legacy-1",
            "status": "pending"
        }))
        .unwrap();
        assert_eq!(v.kind, None);
        assert_eq!(v.kind_label(), "—");
    }

    /// The `extra` flatten still absorbs genuinely-unknown *fields* (the
    /// correct tolerance), but a *known* typed field (`kind`) is consumed
    /// by its slot and never leaks into `extra`.
    #[test]
    fn molecule_view_unknown_fields_land_in_extra_not_kind() {
        let v: MoleculeView = serde_json::from_value(serde_json::json!({
            "id": "t-1",
            "kind": "task",
            "status": "pending",
            "future_field": 42
        }))
        .unwrap();
        assert_eq!(v.kind, Some(MoleculeKindWire::Known(MoleculeKind::Task)));
        assert!(!v.extra.contains_key("kind"));
        assert_eq!(v.extra.get("future_field"), Some(&serde_json::json!(42)));
    }

    /// A present-but-malformed `kind` (wrong JSON type) is now a loud
    /// deserialize error — the panel's core demand. The old `String` +
    /// `serde(default)` would have either coerced or silenced it.
    #[test]
    fn molecule_view_malformed_kind_is_loud_error() {
        let r: std::result::Result<MoleculeView, _> = serde_json::from_value(serde_json::json!({
            "id": "bad-1",
            "kind": 7,
            "status": "pending"
        }));
        assert!(r.is_err(), "numeric kind must not silently decode");
    }

    /// `--json` round-trips the typed kind without loss for both the known
    /// and the skew case.
    #[test]
    fn molecule_view_kind_roundtrips_through_json() {
        for (raw, expect) in [
            ("task", MoleculeKindWire::Known(MoleculeKind::Task)),
            ("nebula", MoleculeKindWire::Unknown("nebula".to_owned())),
        ] {
            let v: MoleculeView = serde_json::from_value(serde_json::json!({
                "id": "r-1", "kind": raw, "status": "pending"
            }))
            .unwrap();
            assert_eq!(v.kind, Some(expect));
            let back = serde_json::to_value(&v).unwrap();
            assert_eq!(back.get("kind").and_then(|k| k.as_str()), Some(raw));
        }
    }
}
