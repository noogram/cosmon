// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-crate wire contract — the **real** adapter answered by the **real**
//! `cosmon-remote` client library (issue #53, U1).
//!
//! # The gap this closes
//!
//! Both sides of the RPP wire were covered, and neither covered the wire.
//! `tests/v1_auth_me.rs` and friends assert what the adapter *emits*, as
//! `serde_json::Value`. `cosmon-remote/tests/wire_contract.rs` asserts what
//! the CLI *accepts*, against wiremock fixtures a human transcribed. Rename a
//! field on one side and both suites stay green: the adapter still emits the
//! shape it asserts, the CLI still parses the fixture it asserts, and the
//! only thing that breaks is the tenant's terminal.
//!
//! So this suite boots the real `cosmon_rpp_adapter::router` on a loopback
//! socket and drives `cosmon_remote::client::Client` — the same code path the
//! shipped `cs-remote` binary runs — against it. Nothing here builds a JSON
//! body by hand and nothing parses one by hand: every assertion goes through
//! the client's own typed deserialization, so a drift in either crate is a
//! compile-or-parse failure *here*, before a tenant meets it.
//!
//! # Direction of the dependency
//!
//! The test lives in the **adapter** crate because `cosmon-remote` must never
//! depend on the adapter: the CLI ships to tenants who do not run a server.
//! The adapter already dev-depends on `cosmon-remote` (the `auth_claude`
//! contract net, `task-20260610-828e`), so this direction adds no edge and
//! cannot cycle.
//!
//! # What each case pins
//!
//! 1. `GET /v1/auth/me` → [`cosmon_remote::client::AuthMeResponse`], with the
//!    six fields `doctor` and `whoami` actually read populated.
//! 2. `GET /.well-known/cosmon-oauth-clients` →
//!    [`cosmon_remote::oidc::ClientRegistry`], version-gated and
//!    audience-looked-up the way `login` does it.
//! 3. `POST /v1/molecules` → [`cosmon_remote::client::MoleculeEnvelope`].
//! 4. `GET /v1/molecules/:id` → the same envelope, on the read path.
//! 5. `POST /v1/molecules/:id/done` → [`cosmon_remote::client::HarvestEnvelope`]
//!    on success, and the seven named [`DoorRefusal`] labels on the error
//!    path — each provoked by pinning the door's own exit code (70–76), so
//!    the mirror is walked across all three crates rather than asserted twice
//!    within one.
//!
//! [`DoorRefusal`]: cosmon_core::harvest_door::DoorRefusal

use std::sync::Arc;
use std::time::Duration;

use cosmon_core::harvest_door::DoorRefusal;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_remote::client::{Client, NucleateRequest};
use cosmon_remote::config::Profile;
use cosmon_remote::error::Error;
use cosmon_remote::oidc::ClientRegistry;
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::json;

/// The one noyau every case in this suite is bound to.
const NOYAU: &str = "a";
/// The audience pinned in the JWKS allowlist and in every minted token.
const AUDIENCE: &str = "cosmon-rpp-a";
/// The `sub` the nucleon binding resolves to [`NOYAU`].
const SUBJECT: &str = "sub-a";

/// Assemble the adapter's `AppState` over a tenant workspace.
///
/// Deliberately the same shape the route-level suites build, so this test
/// exercises the production router rather than a reduced one: a contract test
/// that boots a different server proves a different contract.
fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            SUBJECT,
            HabilitationId::new("nuc-a"),
            Noyau::new(NOYAU),
            AUDIENCE,
        )
        .build();

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        harvest_effect: std::sync::Arc::new(
            cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect,
        ),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
        events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::inert()),
    }
}

/// A live adapter on loopback, plus everything a case needs to talk to it.
///
/// The tempdirs are held here rather than dropped at the end of the builder:
/// letting them fall would delete the tenant workspace out from under the
/// still-running server, and the resulting 404 would look like a route bug.
struct Deployment {
    /// `http://127.0.0.1:<port>` — what a tenant would put in `host`.
    base: String,
    oidc: OidcMock,
    /// Held, never read: dropping the workspaces would delete the tenant
    /// tree out from under the still-running server, and the resulting 404
    /// would read as a route bug.
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
}

impl Deployment {
    /// Boot the real router on an ephemeral loopback port.
    ///
    /// A real socket, not `tower::ServiceExt::oneshot`: the point of this
    /// suite is that the bytes cross a wire the client's own `reqwest` stack
    /// wrote and read. `oneshot` would skip exactly the layer under test.
    async fn start(tenants: TenantWorkspaces) -> Self {
        let oidc = OidcMock::start_with(OidcMockConfig {
            audiences: vec![AUDIENCE.to_owned()],
            ..OidcMockConfig::default()
        })
        .await;
        let security_dir = tempfile::tempdir().unwrap();
        let app = router(make_state(&oidc, &tenants, security_dir.path()));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            base: format!("http://{addr}"),
            oidc,
            _tenants: tenants,
            _security_dir: security_dir,
        }
    }

    /// The tenant profile a `cs-remote` operator would hold for this host.
    fn profile(&self) -> Profile {
        Profile {
            host: self.base.clone(),
            sub: SUBJECT.to_owned(),
            aud: AUDIENCE.to_owned(),
            oidc_url: self.oidc.issuer().to_owned(),
            issuer: None,
            client_id: None,
            noyau: Some(NOYAU.to_owned()),
            scopes: vec![
                "cosmon:molecule:read".to_owned(),
                "cosmon:molecule:write".to_owned(),
            ],
            artifacts_dir: None,
            timeout_secs: 30,
            // Off on purpose: the passive remontée spools into the operator's
            // real config dir, which a test must not touch.
            phone_home: false,
        }
    }

    /// A `cosmon-remote` client authenticated exactly as the shipped binary
    /// would be, with a token minted through the oidc-testkit.
    fn client(&self, scopes: &[&str], jti: &str) -> Client {
        let jwt = self.oidc.issue(&IssueJwt {
            subject: SUBJECT,
            audience: Some(AUDIENCE),
            scopes,
            lifetime_secs: Some(60),
            jti: Some(jti),
        });
        Client::new(&self.profile(), Some(jwt)).expect("profile is ready")
    }
}

/// Write the `security/oauth-clients.toml` the discovery route serves.
///
/// Written as TOML rather than posted through an API because that is how an
/// operator provisions it; the route's job is to project this file onto the
/// wire, and the client's job is to parse what comes back.
fn seed_registry(security_dir: &std::path::Path, issuer: &str) {
    let sec = security_dir.join("security");
    std::fs::create_dir_all(&sec).unwrap();
    std::fs::write(
        sec.join("oauth-clients.toml"),
        format!(
            r#"
schema_version = 2
issuer = "{issuer}"

[[clients]]
audience = "cs-rpp-adapter"
client_id = "abcdef01-2345-6789-abcd-ef0123456789"
scopes = ["cosmon:molecule:read", "cosmon:molecule:write"]

[[clients]]
audience = "claude-web"
client_id = "fedcba98-7654-3210-fedc-ba9876543210"
"#
        ),
    )
    .unwrap();
}

// ── /v1/auth/me ────────────────────────────────────────────────────────────

/// The whoami envelope the client actually holds, not a `Value` that happens
/// to have the right keys.
///
/// `AuthMeResponse` carries `#[serde(flatten)] extra`, so a *renamed* field
/// does not fail to parse — it silently lands in `extra` and the typed field
/// takes its `Default`. That is why the assertions below are on the field
/// *values* and not merely on `from_slice` returning `Ok`: for `sub`,
/// `expires_at` and `issuer` (no `#[serde(default)]`) a rename is a parse
/// error, and for `aud`/`scopes`/`noyau` it is an empty value. Both are red
/// here; neither is red on either side alone.
#[tokio::test]
async fn auth_me_deserializes_into_the_remote_client_type() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add(NOYAU);
    let dep = Deployment::start(tenants).await;

    let client = dep.client(
        &["cosmon:molecule:read", "cosmon:molecule:write"],
        "jti-wire-me",
    );
    let me = client
        .auth_me()
        .await
        .expect("client must parse /v1/auth/me");

    assert_eq!(me.sub, SUBJECT, "the CLI reads `sub` for `whoami`");
    assert_eq!(
        me.aud,
        vec![AUDIENCE.to_owned()],
        "`aud` is the isolation slot the operator is shown",
    );
    assert!(
        me.scopes.contains(&"cosmon:molecule:write".to_owned()),
        "`scopes` drives the CLI's pre-flight refusal messages; got {:?}",
        me.scopes,
    );
    assert_eq!(
        me.noyau.as_deref(),
        Some(NOYAU),
        "`noyau` is the tenant axis `doctor` prints",
    );
    assert!(
        !me.expires_at.is_empty(),
        "`expires_at` is what tells the operator when to re-login",
    );
    assert_eq!(
        me.issuer,
        dep.oidc.issuer(),
        "`issuer` is compared byte-for-byte against the pinned profile",
    );
}

// ── /.well-known/cosmon-oauth-clients ──────────────────────────────────────

/// The reverse-discovery document, fetched and version-gated by the same code
/// `cs-remote login` runs before it ever contacts the IdP.
///
/// `ClientRegistry::fetch` is the whole contract: it parses, then calls
/// `require_supported`, then the caller looks the audience up. Driving those
/// three in order is what makes a `schema_version` bump or an `audience`
/// rename fail here instead of at a tenant's first login.
#[tokio::test]
async fn oauth_client_registry_deserializes_into_the_remote_client_type() {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add(NOYAU);

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec![AUDIENCE.to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    seed_registry(security_dir.path(), oidc.issuer());

    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let registry = ClientRegistry::fetch(&http, &format!("http://{addr}"))
        .await
        .expect("client must fetch, parse and version-gate the registry");

    assert_eq!(
        registry.schema_version,
        cosmon_rpp_adapter::oauth_discovery::CURRENT_SCHEMA_VERSION,
        "server and client disagree on the document version",
    );
    assert_eq!(
        registry.issuer,
        oidc.issuer(),
        "`issuer` is validated byte-for-byte before the document is trusted",
    );

    let entry = registry
        .client_for("cs-rpp-adapter")
        .expect("audience-keyed lookup is the client's only sanctioned selector");
    assert_eq!(entry.audience, "cs-rpp-adapter");
    assert_eq!(entry.client_id, "abcdef01-2345-6789-abcd-ef0123456789");
    assert_eq!(
        entry.scopes.as_deref(),
        Some(
            [
                "cosmon:molecule:read".to_owned(),
                "cosmon:molecule:write".to_owned()
            ]
            .as_slice()
        ),
        "published scopes are what the client falls back from, not to",
    );
    assert!(
        registry.client_for("claude-web").is_some(),
        "both provisioned apps must survive the round-trip",
    );
    assert!(
        registry.client_for("no-such-audience").is_none(),
        "lookup must not fabricate an entry",
    );
}

// ── /v1/molecules (nucleate) and /v1/molecules/:id (observe) ───────────────

/// The write path and the read path share one envelope type, so they share
/// one test: a drift that only one of them survives is still a drift.
///
/// # A finding this suite surfaced on its first run
///
/// The client's `MoleculeView::kind` is `Option<MoleculeKindWire>`, and it is
/// **always `None` here** — `cosmon_state::ops::observe::ObserveJson`, which
/// both RPP routes project through, has no `kind` field at all. So a tenant
/// who nucleates with `kind: "task"` reads back `—` in the Kind column of
/// every `cs-remote observe`/`ensemble`, on a value the server accepted and
/// stored. Neither side's own suite can see it: the adapter asserts the keys
/// it emits, the client asserts a fixture that carries one.
///
/// That is charged to a route this unit does not own (issue #53 U1's write-set
/// is the test surface), so the assertion below pins the contract **as it
/// actually is** rather than as it should be: absent, and therefore `None`.
/// It is not an inert assertion — the day a route starts emitting `kind`,
/// `MoleculeKindWire` makes a *present but malformed* value a loud
/// deserialize error rather than a silent `—`, and this test flips.
#[tokio::test]
async fn nucleate_and_observe_deserialize_into_the_remote_client_type() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add(NOYAU);
    // Library-direct nucleate resolves the formula from the tenant's own
    // `.cosmon/formulas/`; without it the route answers `formula_not_found`.
    tenant.install_task_work_formula().unwrap();
    let dep = Deployment::start(tenants).await;

    let client = dep.client(
        &["cosmon:molecule:read", "cosmon:molecule:write"],
        "jti-wire-mol",
    );

    let created = client
        .nucleate(&NucleateRequest {
            formula: "task-work".to_owned(),
            kind: Some("task".to_owned()),
            variables: [("topic".to_owned(), "wire-contract".to_owned())]
                .into_iter()
                .collect(),
            tags: vec!["temp:warm".to_owned()],
        })
        .await
        .expect("client must parse POST /v1/molecules");

    assert!(
        !created.request_id.is_empty(),
        "`request_id` is the handle the tenant quotes in a support thread",
    );
    let id = created.molecule.id.clone();
    assert!(id.starts_with("task-"), "unexpected id {id}");
    assert!(
        !created.molecule.status.is_empty(),
        "`status` is the column `cs-remote ensemble` renders",
    );
    assert!(
        created.molecule.kind.is_none(),
        "the RPP molecule projection carries no `kind` today; a value here \
         means the route grew one and the contract note above is stale (got {:?})",
        created.molecule.kind,
    );

    let observed = client
        .get_molecule(&id)
        .await
        .expect("client must parse GET /v1/molecules/:id");
    assert_eq!(observed.molecule.id, id);
    assert!(!observed.molecule.status.is_empty());
    assert_eq!(
        observed.molecule.kind_label(),
        created.molecule.kind_label(),
        "the read path and the write path must project one shape",
    );
    assert_eq!(
        observed.molecule.extra.contains_key("formula"),
        created.molecule.extra.contains_key("formula"),
        "a field typed on one path and untyped on the other is the drift \
         this suite exists to catch",
    );
}

// ── /v1/molecules/:id/done — the harvest door (#51, D4 reversed) ──────────
//
// Rewritten for the in-process door (issue #54). These cases used to pin a
// fake `cs`'s exit code in the tenant's molecule directory and read the
// label back off the wire. That subprocess is gone: the decision half runs
// in `cosmon_filestore::harvest_door::decide` over the tenant's own state
// files, and the effect half answers a typed refusal. So each case now
// PLANTS THE STATE that owns its answer — which is a stronger mirror, not a
// weaker one: an exit code is a number the test chose, while a molecule with
// `status: running` is the thing the door is actually about.

/// Arm `[harvest_authority] required` — the second key the decision half
/// reads (ADR-176 D1). Without it the door refuses `not_authorized` before
/// it has even loaded the molecule, and every case below would pass for the
/// wrong reason.
fn arm_harvest_authority(tenant: &cosmon_oidc_testkit::TenantPath) {
    let cosmon_dir = tenant.root.join(".cosmon");
    std::fs::create_dir_all(&cosmon_dir).unwrap();
    std::fs::write(
        cosmon_dir.join("config.toml"),
        "[harvest_authority]\nrequired = true\n",
    )
    .unwrap();
}

/// The effect half, as the CLIENT sees it: `501 harvest_effect_unavailable`.
///
/// The sealed `cs done` transaction has one implementation, and this
/// deployment declares none (the default `UnavailableHarvestEffect`), so a
/// molecule whose harvest the decision half ADMITS gets the typed refusal
/// rather than a merge. Asserting it here, at
/// the client, is what stops the day it becomes a real harvest from being a
/// silent change of contract for every tenant.
#[tokio::test]
async fn the_unavailable_sealed_effect_reaches_the_client_as_a_named_refusal() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add(NOYAU);
    arm_harvest_authority(&tenant);
    // Completed and unmerged: the one shape that survives the whole
    // decision half, so the answer below is the effect half's and nothing
    // else's.
    tenant
        .insert_molecule("task-20260904-wire", &json!({"status": "completed"}))
        .unwrap();
    let dep = Deployment::start(tenants).await;

    let client = dep.client(&["cosmon:molecule:write"], "jti-wire-land");
    let err = client
        .done(
            "task-20260904-wire",
            &cosmon_remote::client::DoneRequest::new("the wire test closes this molecule"),
        )
        .await
        .expect_err("this deployment declares no harvest effect");

    let Error::Api { status, body } = err else {
        panic!("the effect refusal reached the client as {err:?}, not a structured API error");
    };
    assert_eq!(status, 501);
    assert_eq!(body["error"], "harvest_effect_unavailable");
}

/// Idempotence as the client sees it: a repeat lands in the same typed
/// envelope, with `already_landed` rather than an error.
///
/// A client that treated the second call as a failure would make a retry over
/// a lossy network unsafe — which is the reason the door reports it as
/// success in the first place. Post-U6 the `merged_at` stamp on the tenant's
/// own molecule is what says so, in-process.
#[tokio::test]
async fn already_landed_is_still_a_success_envelope_for_the_client() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add(NOYAU);
    arm_harvest_authority(&tenant);
    tenant
        .insert_molecule(
            "task-20260904-again",
            &json!({
                "status": "completed",
                "merged_at": "2026-09-04T00:00:00Z",
            }),
        )
        .unwrap();
    let dep = Deployment::start(tenants).await;

    let client = dep.client(&["cosmon:molecule:write"], "jti-wire-again");
    let landed = client
        .done(
            "task-20260904-again",
            &cosmon_remote::client::DoneRequest::new("the wire test closes this molecule"),
        )
        .await
        .expect("200");
    assert_eq!(landed.harvest.outcome, "already_landed");
    // The typed client decodes the two fields the door added with the PR
    // #62 fix, and the retry states the same integration fact the first
    // call did: this molecule's branch is on the trunk.
    assert_eq!(landed.harvest.merged, Some(true));
    assert_eq!(landed.harvest.non_integration, None);
}

/// The pre-effect refusals, walked end to end across three crates.
///
/// `cosmon-core` owns the label; the adapter maps it to a status; the client
/// surfaces both in `Error::Api`. Asserting the label *at the client* is what
/// makes this different from `v1_done.rs`, where it is read out of a `Value`
/// on the server's own side of the boundary.
///
/// Only the refusals the in-process decision half can still produce are
/// walked. The rest of `ALL_REFUSALS` belongs to the sealed effect, which no
/// longer runs — and a test that pretended to provoke them would be asserting
/// its own fixture rather than the door. `ALL_REFUSALS` keeps its own
/// bijection test in `cosmon-core`; what is checked here is that a refusal
/// which DOES reach the wire keeps its name and its side of the 4xx/5xx line.
#[tokio::test]
async fn every_named_refusal_reaches_the_client_with_its_name_and_status() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add(NOYAU);
    arm_harvest_authority(&tenant);
    // not_completed: work still in flight.
    tenant
        .insert_molecule("task-20260904-flight", &json!({"status": "running"}))
        .unwrap();
    // reservation_requires_seal: a condition only a human lifts.
    tenant
        .insert_molecule(
            "task-20260904-held",
            &json!({"status": "completed", "tags": ["needs-review"]}),
        )
        .unwrap();
    let dep = Deployment::start(tenants).await;

    let client = dep.client(&["cosmon:molecule:write"], "jti-wire-refuse");

    for (id, refusal) in [
        ("task-20260904-flight", DoorRefusal::NotCompleted),
        ("task-20260904-held", DoorRefusal::ReservationRequiresSeal),
    ] {
        // The mirror the whole chain rests on: label and code stay a
        // bijection, so a refusal cannot be read as its neighbour.
        let code = refusal.exit_code();
        assert!(
            (70..=76).contains(&code),
            "{refusal:?} left the door's 70-76 block with {code}",
        );
        assert_eq!(DoorRefusal::from_exit_code(code), Some(refusal));

        let err = client
            .done(
                id,
                &cosmon_remote::client::DoneRequest::new("the wire test closes this molecule"),
            )
            .await
            .expect_err("a refusal must not decode as a success envelope");

        let Error::Api { status, body } = err else {
            panic!("{refusal:?} reached the client as {err:?}, not a structured API error");
        };
        assert_eq!(
            body["error"],
            refusal.as_str(),
            "{refusal:?} lost its name between the door and the client",
        );
        // ADR-176 D7: the operator's configuration fault is the only refusal
        // not charged to the requester as a 4xx.
        assert_eq!(
            status >= 500,
            refusal.is_operator_configuration_fault(),
            "{refusal:?} reached the client on the wrong side of the 4xx/5xx line ({status})",
        );
    }
}

/// `not_authorized`, fail-closed: a galaxy whose operator never armed
/// `[harvest_authority] required` refuses every harvest, and the client sees
/// the name.
///
/// The one case that must NOT arm the door — which is why it is its own test
/// rather than a row in the loop above.
#[tokio::test]
async fn an_unarmed_galaxy_refuses_not_authorized_at_the_client() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add(NOYAU);
    tenant
        .insert_molecule("task-20260904-unarmed", &json!({"status": "completed"}))
        .unwrap();
    let dep = Deployment::start(tenants).await;

    let client = dep.client(&["cosmon:molecule:write"], "jti-wire-unarmed");
    let err = client
        .done(
            "task-20260904-unarmed",
            &cosmon_remote::client::DoneRequest::new("the wire test closes this molecule"),
        )
        .await
        .expect_err("an unarmed galaxy has granted nobody anything");

    let Error::Api { status, body } = err else {
        panic!("not_authorized reached the client as {err:?}, not a structured API error");
    };
    assert_eq!(body["error"], DoorRefusal::NotAuthorized.as_str());
    assert_eq!(status, 403);
}
