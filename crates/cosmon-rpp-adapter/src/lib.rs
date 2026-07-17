// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-rpp-adapter` — Remote Pilot Port (RPP), the §8j HTTPS+OIDC
//! instantiation defined in [ADR-080].
//!
//! # Purpose: the central secure-delivery door of cosmon
//!
//! The RPP is cosmon's **central, audited secure-delivery capability**
//! (ADR-117): the single §8j boundary through which a remote pilot reaches
//! a cosmon instance over the network. It is not a feature built for any
//! one tenant — it is the door cosmon ships *for every* deployment that
//! must be reached remotely under a hardened posture.
//!
//! It *fronts* a tenant's hardware-encryption stack: the RPP is the
//! one-way, causal-closure-enforcing, OIDC-authenticated entrance into a
//! cosmon instance that itself runs inside a hardware-encrypted deployment
//! context (e.g. an AWS VM provisioned with the tenant's HW-encryption
//! solution). The encryption belongs to that deployment context; the RPP's
//! five-clause admission boundary is what makes *access* to it
//! cyber-secured. **As of today the RPP performs no cryptography of its
//! own** beyond TLS termination and JWT signature verification — it does
//! not envelope-encrypt payloads, attest enclaves, or seal the audit log
//! with a KMS key. Documentation here states purpose, never an unbacked
//! encryption feature (ADR-117 §2b honesty constraint).
//!
//! # Shape
//!
//! The RPP is a *Layer B port adapter* (ADR-023 hexagonal): a long-lived
//! `axum` server that admits remote pilot requests through a five-clause
//! admission boundary (identity, causal closure, rate limit, one-way
//! topology, subprocess envelope) and shells out to the real `cs` binary
//! for every admitted request. The adapter holds **no** in-RAM business
//! state — JWKS, rate-limiter, deny-list are idempotent projections of
//! the filesystem.
//!
//! V0 surface (this crate): a single read-only route
//! `GET /v1/molecules/:id` that proxies to `cs observe :id --json`.
//!
//! See [ADR-117] for the secure-delivery framing, [ADR-080] for the
//! architectural framing, [§8j] for the parent invariant, and [§8p] for
//! the "API surface ⊊ CLI surface" rule.
//!
//! [ADR-117]: ../../docs/adr/117-rpp-central-security-capability.md
//! [ADR-080]: ../../docs/adr/080-remote-pilot-port-https-oidc.md
//! [§8j]: ../../docs/architectural-invariants.md
//! [§8p]: ../../docs/architectural-invariants.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// The rate-limiter and clock skew arithmetic uses f64↔i64 casts on
// values bounded well within both representations (token counts in
// the low hundreds, ms-since-epoch fits in i64 for thousands of
// years). The casts are reviewed by tests, not the compiler.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
// Fine-grained pedantic lints that fire on idiomatic literal-handling
// code in this crate; suppressing them keeps the surface readable
// without lowering the Rust hygiene bar.
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::module_name_repetitions)]

pub mod admin_seal;
pub mod admission;
pub mod audit;
pub mod auth;
pub mod auth_claude;
pub mod backend_health;
pub mod config;
pub mod deny_list;
pub mod error;
pub mod events_bus;
pub mod image_init;
pub mod jwks_fetch;
pub mod jwt;
pub mod metrics;
pub mod nucleon_map;
pub mod oauth_discovery;
pub mod phone_home;
pub mod portee;
pub mod provisioner;
pub mod rate_limit;
pub mod reload;
pub mod routes;
pub mod scope_badge;
pub mod subprocess;
pub mod surface_events;
pub mod trust_bootstrap;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

pub use auth_claude::AuthClaudeState;
pub use backend_health::{BackendHealth, BackendHealthRegistry, BackendProbe, BackendStatus};
pub use error::{ApiError, RppRejectReason};
pub use events_bus::{EventBus, MoleculeEvent, SharedEventBus};
pub use jwks_fetch::{JwksFetcher, JwksProvider, JwksRefreshReport, TrustedIssuer, TrustedIssuers};
pub use jwt::{JwksStore, JwtVerifier, SharedJwksStore, ValidatedJwt};
pub use metrics::{MetricsRegistry, StatusClassCounts};
pub use nucleon_map::{HabilitationMap, SharedHabilitationMap};
pub use oauth_discovery::{ClientRegistry, DiscoveryError, OAuthClient, CURRENT_SCHEMA_VERSION};
pub use rate_limit::{IngressRateLimiter, RateState};
pub use scope_badge::{
    issue_badge, verify_badge, BadgeError, FederatedProvenance, GalaxyRef, ScopeBadge, SigAlg,
    VerifiedBadge, SCOPE_BADGE_VERSION,
};

/// Default per-request body size cap (turing G15). 1 MB is comfortable
/// for any V0 payload and forms a structural defence against memory
/// exhaustion via crafted bodies.
pub const DEFAULT_BODY_LIMIT_BYTES: usize = 1024 * 1024;

/// Default per-subprocess timeout — a `cs observe` call is bounded
/// well under a second; 30 s is an order-of-magnitude headroom that
/// also forms the upper bound on pilot-perceived latency.
pub const DEFAULT_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Default wall-clock deadline for one resident drain
/// (`POST /v1/molecules/{id}/run`, B2). Passed
/// to `cs run` as `--timeout` so the loop's deadline is a NAMED exit
/// (I4 — `timeout`, exit 124), never a stall. One hour: a drain
/// dispatches real Claude workers, so the bound is hours-shaped, not
/// request-shaped; the B3 budget (binding-resolved, obligatory) is the
/// well-founded measure that forces termination — this deadline is the
/// belt-and-braces wall clock on top.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(3600);

/// In-process registry of active resident drains, one slot per noyau
/// (B2 bounded drain).
///
/// The single-writer-trunk discipline (`MCStitch` I1) makes a second
/// concurrent loop in the same noyau useless at best (the advisory
/// `trunk.lock` serialises them) and a budget-burning footgun at
/// worst — so the request door refuses it outright with a stable
/// `409 drain_already_active` instead of queueing. In-RAM on purpose:
/// the slot guards *this adapter's* spawns; durability of the drain
/// itself lives in the tenant filesystem state like everything else.
#[derive(Debug, Default)]
pub struct DrainRegistry {
    active: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl DrainRegistry {
    /// Try to claim the noyau's drain slot. Returns `false` when a
    /// drain is already active for this noyau.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (a prior holder
    /// panicked) — unrecoverable adapter state.
    #[must_use]
    pub fn try_acquire(&self, noyau: &str) -> bool {
        self.active
            .lock()
            .expect("drain registry mutex poisoned")
            .insert(noyau.to_owned())
    }

    /// Release the noyau's drain slot (the spawned loop exited).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn release(&self, noyau: &str) {
        self.active
            .lock()
            .expect("drain registry mutex poisoned")
            .remove(noyau);
    }

    /// Whether a drain is currently active for the noyau.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn is_active(&self, noyau: &str) -> bool {
        self.active
            .lock()
            .expect("drain registry mutex poisoned")
            .contains(noyau)
    }
}

/// Adapter posture (ADR-076). `Prepared` is the V0 default: every
/// laxity (e.g. JWT `exp - iat > 15 min`) is *warned* but does not
/// reject. `Active` enforces the post-Day-J production flow with no
/// fallback. The flip is operator-only via `cs security activate`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Posture {
    /// Dev posture — warnings on every laxity, no hard reject on
    /// long-lived JWTs.
    #[default]
    Prepared,
    /// Production posture — 15 min `exp` cap, `DPoP` required (V1+).
    Active,
}

/// Shared adapter state injected into every handler. The struct holds
/// pure references to disk-backed projections (JWKS, nucleon map,
/// rate-limiter, deny-list) — never any per-request business state.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Absolute path to the `cs` binary that will be invoked per
    /// request (clause (e), subprocess envelope).
    pub cs_path: PathBuf,
    /// Cosmon state directory (`.cosmon/state/`). Used to resolve
    /// nucleon mappings, JWKS, deny-list, rate-limiter on-disk.
    pub state_dir: PathBuf,
    /// Whisper inbox root for §8j(b) causal closure
    /// (`.cosmon/whispers/inbox/api/`).
    pub inbox_root: PathBuf,
    /// Tenant galaxy root — the subprocess `cwd` is `galaxies_root /
    /// <noyau>` (clause (e)). Defaults to `$HOME/galaxies`.
    pub galaxies_root: PathBuf,
    /// Sealed JWKS store — the **authn door**. Loaded at boot from
    /// `<state_dir>/security/jwks/` and held behind a [`SharedJwksStore`]
    /// (`arc-swap`) so it can be hot-reloaded on `SIGHUP`, symmetric with
    /// [`nucleon_map`](AppState::nucleon_map) (the authz door). Adding or
    /// revoking a federated peer issuer's keys (ADR-0023 MVP-A) is then a
    /// host-side `cp`/`rm` under `security/jwks/` + one `SIGHUP` — no
    /// reboot, no dropped tmux worker. Handlers take a snapshot with
    /// `state.jwks.load()`; see [`crate::reload`].
    pub jwks: SharedJwksStore,
    /// Sealed `sub → nucleon_id` map (clause (a)).
    ///
    /// Held behind a [`SharedHabilitationMap`] (an `arc-swap` handle) rather
    /// than a bare `Arc` so the bindings can be reloaded at runtime on
    /// `SIGHUP` — staging a new binding no longer needs a process restart
    /// that would drop in-flight tmux workers. Handlers take a snapshot
    /// with `state.nucleon_map.load()`; see [`crate::reload`].
    pub nucleon_map: SharedHabilitationMap,
    /// Per-`sub` leaky bucket persisted under
    /// `<state_dir>/security/oidc-rate-limit/`.
    pub rate_limiter: Arc<IngressRateLimiter>,
    /// Deny-list (kill-switch), re-read with a 30 s TTL on cache miss.
    pub deny_list: Arc<deny_list::DenyList>,
    /// Adapter posture (`prepared` / `active`).
    pub posture: Posture,
    /// Per-subprocess timeout (default [`DEFAULT_SUBPROCESS_TIMEOUT`]).
    pub subprocess_timeout: Duration,
    /// Anthropic API key resolved at boot from the ladder
    /// (docker-secret → operator-file → env, see
    /// [`image_init::resolve_anthropic_key`]). Injected into the env of
    /// every `cs` subprocess spawn so the worker `claude` inherits it
    /// (step 3c — the binary equivalent of the script's `export`).
    /// `None` when no key resolved; `cs tackle` then fails with the
    /// upstream `ANTHROPIC_API_KEY not set` rather than a silent stall.
    pub anthropic_api_key: Option<String>,
    /// Model pin for tenant claude worker sessions (avatar-surface
    /// D1), resolved at boot from the instance config
    /// ([`config::RppConfig::resolved_claude_model`]) — operator
    /// binding, readable by the tenant, never written by it. Exported
    /// as `ANTHROPIC_MODEL` into every `cs tackle` subprocess spawn so
    /// the worker `claude` runs the pinned model. `None` is the
    /// explicit opt-out (`claude_model = ""` in `rpp.toml`): nothing
    /// is exported and the claude CLI resolves its own default. The
    /// value is carried opaquely — no model-id literal lives outside
    /// the config module's single named default.
    pub claude_model: Option<String>,
    /// In-RAM registry of LLM-backend health observations
    /// (T-V1-IFBDD-METER). Read-only diagnostic surface; backend
    /// wrappers record probes via
    /// [`BackendHealthRegistry::record`].
    pub backend_health: Arc<BackendHealthRegistry>,
    /// Optional auth-claude surface (ADR-0017 smithy). When `None`,
    /// the 5 `/v1/auth/claude/*` routes return `503 service_unavailable`;
    /// when `Some`, they are fully active. Defaulting to `None` keeps
    /// the addition strictly additive for callers that have not yet
    /// configured the surface.
    pub auth_claude: Option<Arc<AuthClaudeState>>,
    /// Root directory under which per-molecule artifact directories
    /// live (`<root>/<noyau>/<molecule_id>/`). Default
    /// [`routes::artifacts::DEFAULT_ARTIFACT_ROOT`] = `/tmp/cosmon`.
    /// The convention is created at tackle
    /// time (the adapter mkdir's the per-molecule dir before spawning
    /// the worker subprocess) and `COSMON_ARTIFACT_DIR` is exported
    /// into the worker env so the worker writes its outputs there.
    /// The three `/v1/molecules/{id}/artifacts*` routes serve from
    /// this root. Pinning the root in `AppState` keeps tests hermetic
    /// — they point the adapter at a `tempfile::TempDir` rather than
    /// scribbling on `/tmp/cosmon`.
    pub artifact_root: PathBuf,
    /// Per-platform binary distribution state (Phase 1 dist multi-OS).
    /// Drives
    /// `GET /dist/binary/{platform}/cosmon-remote`. Operational route
    /// (outside `/v1/`), unauthenticated, deliberately excluded from
    /// the §8p frozen surface. The default
    /// [`routes::dist::DEFAULT_DIST_ROOT`] points at where the
    /// adapter Dockerfile COPYs the host-built binaries; tests pin it
    /// to a `tempfile::TempDir` and seed fake bytes.
    pub dist: Arc<routes::dist::DistState>,
    /// Per-deployment values substituted into the served `install.sh`
    /// at fetch time. Resolves the AWS live-deploy
    /// finding "host seulement, sub/aud/oidc-url devinés par templating
    /// brittle" — the tenant's install lands with the full four-tuple
    /// persisted, not just the host. Defaults to all empty: install.sh
    /// then skips `config set` lines for the missing fields.
    pub install_templating: Arc<config::InstallTemplating>,
    /// In-process pub/sub bus for molecule lifecycle events
    /// (SSE `/v1/events`). Every mutation route
    /// publishes a [`MoleculeEvent`] after the state transition lands;
    /// the SSE handler subscribes and forwards to the wire. The bus is
    /// lossy by design (a slow subscriber will see lagged-receiver
    /// reset, not adapter backpressure) — durability lives in the
    /// per-tenant filesystem state.
    pub events: SharedEventBus,
    /// In-RAM metrics registry exposed by `/metrics` (Prometheus
    /// text) and `/diagnostics` (JSON). Reset
    /// on adapter restart; never persisted, never per-tenant. The
    /// adapter's `metrics_layer` middleware bumps status-class
    /// counters on every response; admission helpers explicitly
    /// call [`MetricsRegistry::record_reject`] when they convert a
    /// rejection into an HTTP error.
    pub metrics: Arc<MetricsRegistry>,
    /// One-slot-per-noyau registry of active resident drains
    /// (`POST /v1/molecules/{id}/run`, B2).
    /// See [`DrainRegistry`].
    pub drains: Arc<DrainRegistry>,
    /// Host-sealed operator credential guarding `/v1/admin/*`.
    /// Disjoint from the tenant OIDC chain —
    /// see [`admin_seal::AdminSeal`]. Fail-closed: when no seal is
    /// configured at boot the admin surface returns `403 admin_disabled`.
    pub admin_seal: Arc<admin_seal::AdminSeal>,
    /// Single-writer binding provisioner backing the admin surface.
    /// Writes `(iss, sub) → noyau` bindings and
    /// reloads the live [`SharedHabilitationMap`] in-process — the
    /// reload-without-reboot primitive. See [`provisioner::Provisioner`].
    pub provisioner: Arc<provisioner::Provisioner>,
    /// Presentation-layer federation tooling backing `/v1/admin/federations`
    /// (ADR-0023 G5). One operator gesture materialises N per-galaxy
    /// habilitations and groups them as one portée; it writes through the
    /// same [`provisioner::Provisioner`] (single-writer discipline holds).
    /// See [`portee::PorteeProvisioner`].
    pub portee_provisioner: Arc<portee::PorteeProvisioner>,
}

impl AppState {
    /// Convert a [`RppRejectReason`] into an [`ApiError`] and record
    /// the rejection in [`Self::metrics`] under its stable label.
    ///
    /// Routes use this helper instead of [`ApiError::from_reject`]
    /// directly so the `cosmon_adapter_admission_rejects_total`
    /// counter populates on the JWT-validation hot path.
    ///
    /// Takes [`RppRejectReason`] by value so handler `.map_err(|e|
    /// state.reject(e))` closures stay terse; internally we only need
    /// borrows. The `needless_pass_by_value` lint is suppressed for
    /// this ergonomic.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn reject(&self, reason: RppRejectReason) -> ApiError {
        let api = ApiError::from_reject(&reason, None);
        self.metrics.record_reject(reason.label());
        api
    }

    /// Variant of [`Self::reject`] that propagates a `request_id`
    /// (used by handlers that have already minted one for audit
    /// correlation, e.g. the artifact PUT path).
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn reject_with_request_id(&self, reason: RppRejectReason, request_id: String) -> ApiError {
        let api = ApiError::from_reject(&reason, Some(request_id));
        self.metrics.record_reject(reason.label());
        api
    }
}

/// Build the RPP router over a shared [`AppState`].
///
/// The router exposes the V0 read-only surface plus the V1 mutation
/// cuts: `GET /v1/molecules/:id`, `POST /v1/molecules`,
/// `POST /v1/molecules/:id/tags` (T-CST-V0), the T-CST-EXPAND
/// lifecycle quintet (`collapse`/`freeze`/`thaw`/`stuck` + the
/// `GET /v1/molecules` ensemble), and the T9 remote-tackle V2 dispatch cut
/// (`POST /v1/molecules/:id/tackle`,
/// [`tackle_molecule`](`crate::routes::molecules::tackle_molecule`)).
/// Any addition is a §8p violation that must trip
/// `tests::api_surface_freeze`.
///
/// Layers (outer → inner):
/// - [`TraceLayer`] for `tracing` integration;
/// - [`CorsLayer`] explicitly *closed* in V0 (no allowed origins);
/// - [`RequestBodyLimitLayer`] capped at [`DEFAULT_BODY_LIMIT_BYTES`];
/// - [`routes::quota::rate_limit_headers_layer`] injects
///   `X-RateLimit-{Limit,Remaining,Reset}` on every JWT-gated response
///   (task `20260522-2f91`, gap report ae3d §h). Applied on the whole
///   router; no-ops on routes that carry no JWT (`/healthz`,
///   `/install.sh`, …).
#[allow(clippy::too_many_lines)]
pub fn router(state: AppState) -> Router {
    use axum::routing::{delete, post};
    let shared = Arc::new(state);
    Router::new()
        .route("/v1/molecules", get(routes::molecules::list_molecules))
        .route("/v1/molecules/{id}", get(routes::molecules::get_molecule))
        .route("/v1/molecules", post(routes::molecules::post_molecule))
        .route(
            "/v1/molecules/{id}/tags",
            post(routes::molecules::tag_molecule),
        )
        .route(
            "/v1/molecules/{id}/collapse",
            post(routes::molecules::collapse_molecule),
        )
        .route(
            "/v1/molecules/{id}/freeze",
            post(routes::molecules::freeze_molecule),
        )
        // Legacy /thaw endpoint fused into /freeze {state} in v1.0.0-rc
        // (task-20260522-b538). Returns 410 Gone with a migration hint
        // until v1.2.0; dropped to axum 404 thereafter.
        .route(
            "/v1/molecules/{id}/thaw",
            post(routes::molecules::thaw_gone_handler),
        )
        .route(
            "/v1/molecules/{id}/stuck",
            post(routes::molecules::stuck_molecule),
        )
        .route(
            "/v1/molecules/{id}/tackle",
            post(routes::molecules::tackle_molecule),
        )
        // B2 bounded drain (task-20260610-56c4, ADR-124,
        // delib-20260610-9a0c K4). The client REQUESTS a drain of the
        // DAG rooted at {id}; the resident `cs run` loop spawned
        // inside the tenant container decides what to tackle, when,
        // under the binding's B1/B2/B3 bounds — the HTTP boundary is
        // a request door, never an orchestration cockpit.
        .route(
            "/v1/molecules/{id}/run",
            post(routes::molecules::run_molecule),
        )
        // Artifact endpoints (e653 spec, task-20260522-ef4f). The
        // PUT and GET share `/artifacts/{token}` — axum disambiguates
        // by method; the path segment carries the manifest token on
        // GET and the on-disk filename on PUT. Filesystem-mediated
        // (under `<artifact_root>/<noyau>/<mol_id>/`), no `cs` CLI
        // verb counterpart.
        .route(
            "/v1/molecules/{id}/artifacts",
            get(routes::artifacts::list_artifacts),
        )
        .route(
            "/v1/molecules/{id}/artifacts/{token}",
            get(routes::artifacts::fetch_artifact).put(routes::artifacts::push_artifact),
        )
        // Canonical-deliverable route (task-20260605-f46c). Reads the
        // *persistent* molecule dir (synthesis.md / result.md) with the
        // ephemeral artifact dir as fallback, so a tackled molecule's
        // output is recoverable even when the default task-work worker
        // never wrote into COSMON_ARTIFACT_DIR. Closes the onboarding
        // gap where artifacts=[] left the first molecule unrecoverable.
        .route("/v1/molecules/{id}/result", get(routes::result::get_result))
        // auth-claude surface (ADR-0017 smithy, no-direct-shell).
        // The handlers consult `AppState::auth_claude`; when `None`,
        // they return 503 service_unavailable so the endpoints are
        // discoverable but inert until the operator wires the surface.
        .route("/v1/auth/claude/start", post(auth_claude::routes::start))
        .route(
            "/v1/auth/claude/email",
            post(auth_claude::routes::submit_email),
        )
        .route(
            "/v1/auth/claude/{session_id}",
            get(auth_claude::routes::get_session),
        )
        .route(
            "/v1/auth/claude/{session_id}",
            delete(auth_claude::routes::delete_session),
        )
        .route(
            "/v1/auth/claude/confirm",
            post(auth_claude::routes::confirm),
        )
        // /v1/auth/me — JWT introspection / whoami (task-20260522-560a,
        // gap-report ae3d workflow j priority-1). Admission-side, no
        // scope check; same exclusion class as `/v1/auth/claude/*` for
        // the §8p bijection (no `cs` CLI counterpart).
        .route("/v1/auth/me", get(routes::auth_me::get_auth_me))
        // Quota / rate-limit snapshot (task-20260522-2f91, gap report
        // ae3d workflow §h). Tenant-visible read face of the §8j
        // ingress leaky bucket — same JWT + scope gate as
        // GET /v1/molecules; intentionally part of the §8p frozen
        // surface so its shape is stable for tooling.
        .route("/v1/quota", get(routes::quota::get_quota))
        // /v1/noyaux — discovery endpoint for multi-noyau operators
        // (task-20260523-eb61, gap report ae3d workflow f). Admission-
        // side route, no scope check; same exclusion class as
        // /v1/auth/me for the §8p bijection (no `cs` CLI counterpart).
        .route("/v1/noyaux", get(routes::noyaux::list_noyaux))
        // Operator-sealed admin provisioning surface (task-20260616-f112,
        // B2 impl of the B1 design). Auth is the host-side `AdminSeal`
        // (X-Cosmon-Admin-Token), DISJOINT from the tenant OIDC chain —
        // a tenant JWT never opens these doors. POST writes an
        // (iss, sub) → noyau binding and reloads the map in-process (no
        // SIGHUP, no reboot, no dropped tmux worker); GET lists the
        // provisioned bindings; DELETE revokes one; POST /reload re-reads
        // bindings staged by any channel. principal=operator,
        // exposure=adapter-only, scope=- (no `cs` verb twin — operator
        // surface is an HTTP-ingress concern). Fail-closed when no seal
        // is configured at boot (403 admin_disabled).
        .route(
            "/v1/admin/habilitations",
            post(routes::admin::provision_habilitation).get(routes::admin::list_habilitations),
        )
        .route(
            "/v1/admin/habilitations/{id}",
            delete(routes::admin::revoke_habilitation),
        )
        .route(
            "/v1/admin/reload",
            post(routes::admin::reload_habilitations),
        )
        // Portée tooling — the one-gesture federation surface (ADR-0023
        // G5). Same host-side `AdminSeal` as the habilitation routes
        // (operator, adapter-only, no `cs` verb twin). POST materialises
        // N per-galaxy habilitations from one gesture and groups them as
        // one relation; GET lists the grouped relations; DELETE dissolves
        // a whole relation; DELETE …/galaxies/{galaxy} revokes one galaxy.
        // The bindings are written through the same single-writer
        // provisioner — this is presentation/grouping, not a new core.
        .route(
            "/v1/admin/federations",
            post(routes::admin::federate).get(routes::admin::list_federations),
        )
        .route(
            "/v1/admin/federations/{id}",
            delete(routes::admin::dissolve_federation),
        )
        .route(
            "/v1/admin/federations/{id}/galaxies/{galaxy}",
            delete(routes::admin::revoke_federation_galaxy),
        )
        // Workers — list active workers in the caller's noyau
        // (task-20260523-f82b, gap report ae3d workflow §e priority 5).
        // Adapter-only — no `cs` CLI verb counterpart; operator-side
        // observability uses `cs status` / `cs ensemble` against the
        // on-disk fleet. Exempted from the §8p bijection check by the
        // `/v1/workers` path filter in `tests/api_surface_freeze.rs`.
        .route("/v1/workers", get(routes::workers::list_workers))
        // D-AVATAR canal (b) pilote↔avatar-tiers (task-20260524-270a,
        // ADR-0020 §5, spec d958). Tenant-verb since task-20260610-0b57
        // (delib-20260610-9a0c T3): carried by the client as the
        // TOP-LEVEL verb `converse` — never an `avatar` subcommand
        // (guide §12.2). On-by-binding server-side; `request` chains
        // are hop-bounded (L3 anti-cycle).
        .route("/v1/avatar/converse", post(routes::avatar::converse))
        // D-AVATAR canal (d) monde↔avatar (task-20260524-270a,
        // ADR-0020 §5, spec d958). Adapter-only — no CLI verb.
        // OFF by default (feature flag per-source).
        .route("/v1/avatar/perceive", post(routes::avatar::perceive))
        // D-AVATAR instance lifecycle (task-20260525-738e).
        // These routes ARE exposed as CLI verbs (§8p bijection) unlike
        // perceive which stays an adapter-only canal.
        .route(
            "/v1/avatar/{instance_id}/status",
            get(routes::avatar::avatar_status),
        )
        .route(
            "/v1/avatar/{instance_id}/incarnate",
            post(routes::avatar::avatar_incarnate),
        )
        .route(
            "/v1/avatar/{instance_id}/grant",
            post(routes::avatar::avatar_grant),
        )
        .route(
            "/v1/avatar/{instance_id}/audit",
            get(routes::avatar::avatar_audit),
        )
        .route(
            "/v1/avatar/{instance_id}/mould-info",
            get(routes::avatar::avatar_mould_info),
        )
        // Server-Sent Events stream of molecule lifecycle events
        // (task-20260522-c46a, workflow c of the gap-report ae3d).
        // Adapter-only — there is no `cs events stream` verb; durable
        // history lives in the per-tenant filesystem state. The route
        // is exempt from the §8p bijection check via the `/v1/events`
        // path filter in `tests/api_surface_freeze.rs`.
        .route("/v1/events", get(routes::events_stream::events_stream))
        // Server-Sent Events stream of per-molecule worker tmux
        // output (task-20260523-ad25, workflow d of the gap-report
        // ae3d). Adapter-only — there is no `cs logs stream` verb;
        // the live tail is fundamentally an HTTP-ingress concern.
        // The route is exempt from the §8p bijection check via the
        // `/logs` path filter in `tests/api_surface_freeze.rs`.
        .route(
            "/v1/molecules/{id}/logs",
            get(routes::logs_stream::logs_stream),
        )
        // Health routes are intentionally outside `/v1/` so they
        // never count toward the §8p frozen API surface.
        .route("/healthz", get(routes::healthz))
        // `/api/healthz` alias — kept because external welcome.md docs
        // and parent-pattern (jordan-showroom) reference it.
        .route("/api/healthz", get(routes::healthz))
        .route("/health/backends", get(routes::backends_health))
        // Extended healthz family (task-20260523-d820, gap report ae3d
        // workflow §i). `/healthz` stays minimal-plus-version (`ok`,
        // `service`, `version`, `api_surface_version` —
        // delib-20260610-9a0c, tolnay) so that Tailscale-style probes
        // keep their no-allocation contract;
        // `/metrics` (Prometheus text) and `/diagnostics` (JSON) carry
        // the dashboard payload. All three are operational — outside
        // `/v1/`, no JWT gate, excluded from the §8p frozen surface.
        .route("/metrics", get(routes::metrics_handler))
        .route("/diagnostics", get(routes::diagnostics_handler))
        // cosmon-remote Phase 0 — serve the bootstrap script and the
        // tenant justfile. Operational, unauthenticated, deliberately
        // outside `/v1/` so they never count toward the §8p frozen API
        // surface (same class as `/healthz` and `/`).
        .route("/install.sh", get(routes::serve_install_sh))
        .route("/dist/justfile", get(routes::serve_dist_justfile))
        // smithy avatar-surface C2 — the recommended CLAUDE.md block
        // the tenant copies into their agent's global CLAUDE.md. GET
        // only by construction (godel L1: no route ever writes the doc
        // that pilots the client agent).
        .route("/dist/CLAUDE.md", get(routes::serve_dist_claude_md))
        // cosmon-remote Phase 1 dist (task-20260522-aad5) — serve the
        // pre-built Rust CLI for the 4 cross-compile targets. Same
        // operational class as `/install.sh` and `/dist/justfile`
        // (outside `/v1/`, no JWT, excluded from §8p).
        .route(
            // Derive the route pattern from the same function the
            // snapshot test re-derives the shell URL against, so the
            // path layout has exactly one Rust-side source
            // (`task-20260607-4b79`, B2 path-layout residue).
            &routes::dist::binary_url_path("{platform}"),
            get(routes::dist::serve_binary),
        )
        // OAuth client-id reverse-discovery (task-20260710-909a,
        // delib-20260710-33b7 §C8). `GET /.well-known/cosmon-oauth-clients`
        // publishes the runtime-generated Forgejo `client_id`s (keyed by
        // audience, covering A=cs-rpp-adapter + B=claude-web) so the
        // pre-provisioned `cosmon-remote` client can LEARN its own
        // client_id. Public document (integrity, not confidentiality —
        // the RS-side closed audience allowlist in `jwt::JwtVerifier` is
        // the isolation wall). Operational-class: outside `/v1/`, no JWT,
        // excluded from the §8p frozen surface (cosmon-namespaced, not an
        // IANA well-known — does not squat `oauth-authorization-server`).
        .route(
            "/.well-known/cosmon-oauth-clients",
            get(routes::oauth_discovery::get_oauth_clients),
        )
        // Root landing page (`/`) — informational, no JWT gating.
        .route("/", get(routes::root_landing))
        // `/mcp` — Model Context Protocol surface, nested as a third
        // projection of the same core the `/v1/...` routes project
        // (delib-20260709-943e). Streamable HTTP (MCP 2025-03-26) behind
        // a bearer-required gate. Operational-class like `/healthz` and
        // `/install.sh`: outside `/v1/`, so it never counts toward the
        // §8p frozen surface and carries no `#[verb]` bijection twin. The
        // per-tool scope partition, `cwd` severance, and RFC 9728
        // discovery doc are the path-B seams tracked in
        // `routes/mcp.rs` — not wired here.
        .nest("/mcp", routes::mcp::mcp_router(shared.clone()))
        // X-RateLimit-* injection layer (task-20260522-2f91 §h). The
        // layer reads the bearer, re-validates the JWT against the
        // sealed JWKS, and sets three headers on the response. Silent
        // no-op when no JWT is present, so health/install routes are
        // untouched. Inserted before the CORS / body-limit layers so
        // the headers land even on requests that are rejected upstream
        // (e.g. body-limit overrun) — the tenant always sees its
        // remaining budget so long as the JWT validated.
        //
        // The no-op-without-JWT behaviour means the operational class
        // (`/healthz`, `/`, `/install.sh`, `/dist/*`, `/metrics`,
        // `/diagnostics`, `/.well-known/cosmon-oauth-clients`, `/mcp`
        // discovery) carries NO application-layer throttle. This is a
        // recorded, deliberate decision (task-20260710-4364, review df19
        // F3), not a gap: §8j clause (c) is scoped to the admission
        // boundary (JWT-authenticated spark sources), and DoS control for
        // the unauthenticated read-only class is delegated to the network
        // edge (reverse proxy / tailnet ACL), which is the only layer that
        // sees the real client peer behind the 127.0.0.1 TLS terminator.
        // See `docs/architectural-invariants.md` §8j — "Operational-class
        // routes are exempt from clause (c)". Do NOT add a per-IP or
        // global app-layer bucket here without first grounding a trusted
        // client-IP source: a per-IP limiter self-DoSes via IP rotation
        // and a global one starves the allocation-free `/healthz` probe.
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            routes::quota::rate_limit_headers_layer,
        ))
        // Phone-home ingest (delib-20260610-9a0c C3, passive opt-out
        // remontée). Reads `X-Cosmon-Phone-Home` off authenticated
        // requests and materialises `request_id + error code` reports
        // under `<inbox_root>/phone-home/` for the patrouille-abandon.
        // No new route — §8p untouched; the client cuts the channel
        // with `config set phone-home off` (D-AVATAR-1).
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            phone_home::phone_home_ingest_layer,
        ))
        // Per-response status-class counter (task-20260523-d820).
        // Layered on the whole router so 4xx/5xx anomalies surface in
        // `/metrics` even when they originate inside another middleware
        // (body-limit overruns, CORS rejects). One atomic add per
        // request.
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            metrics::metrics_layer,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::new()) // empty allow-list — refusing CORS `*` (G14)
        .layer(RequestBodyLimitLayer::new(DEFAULT_BODY_LIMIT_BYTES))
        .with_state(shared)
}

/// Enumerate the user-facing route paths exposed by [`router`]. The
/// list is the canonical surface tested by
/// `tests/api_surface_freeze.rs` (R3 in ADR-080 §12).
///
/// Only `/v1/...` paths count toward §8p. Operational endpoints such
/// as `/healthz` are deliberately excluded.
///
/// The surface is an **event-fold** over
/// [`crate::surface_events::SURFACE_EVENTS`], itself a compile-time
/// projection of `crates/cosmon-rpp-adapter/data/surface_events.txt`
/// (ADR-110 §I3). A molecule that adds a
/// route appends one line to the data file; the const array, length
/// assertions, and bijection test all derive automatically from that
/// single source. No `len()` literal anywhere refers to the surface size.
#[must_use]
pub fn frozen_api_surface() -> &'static [&'static str] {
    surface_events::SURFACE_ROUTES
}
