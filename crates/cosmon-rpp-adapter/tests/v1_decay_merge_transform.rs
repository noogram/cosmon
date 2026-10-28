// SPDX-License-Identifier: AGPL-3.0-only

//! The remote surface of `cs decay`, `cs merge` and `cs transform` (pilot plan
//! 0001, lot E2; asked for by Leanear).
//!
//! Each route calls the operation its `cs` verb calls. These tests drive the
//! production router over a real file store and pin, per verb, what the route
//! leaves in the store and who may ask. The other half of "same as `cs`" is
//! `crates/cosmon-cli/tests/remote_interaction_parity.rs`, which runs the real
//! `cs` binary on the same fixture and asserts the same projection of the
//! store: the `EXPECTED_*` projections below are written once, in both files.
//!
//! All three verbs need `cosmon:molecule:write` and nothing else. `merge` is
//! the one that can touch more than one tenant's data if it is careless, so
//! most of this file is about it: every source is looked up in the caller's
//! own tenant before anything is written, and a source that is not there —
//! because it never existed or because it belongs to another binding — refuses
//! the whole merge, with the answer a nonexistent molecule gets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::MoleculeLink;
use cosmon_filestore::FileStore;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_remote::client::Client;
use cosmon_remote::config::Profile;
use cosmon_rpp_adapter::auth_claude::{
    AuthClaudeConfig, AuthClaudeState, FilesystemSessionStore, SessionStore,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::worker_env::WorkerBackends;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::MockBackend;
use serde_json::{json, Value};
use tower::ServiceExt;

const READ: &str = "cosmon:molecule:read";
const WRITE: &str = "cosmon:molecule:write";
const AUDIENCE: &str = "cosmon-rpp-a";

/// Two caller bindings: `sub-a` reaches tenant `a`, `sub-b` reaches tenant `b`.
const AUDIENCE_B: &str = "cosmon-rpp-b";

/// A source of tenant `a` that can decay and merge.
const IDEA: &str = "idea-20261028-aaaa";
/// A second mergeable source of tenant `a`.
const TASK_1: &str = "task-20261028-1111";
/// A third mergeable source of tenant `a`.
const TASK_2: &str = "task-20261028-2222";
/// A molecule of tenant `a` whose kind cannot merge.
const DECISION: &str = "decision-20261028-dddd";
/// A mergeable molecule of tenant `b`, which `sub-a` has no business with.
const FOREIGN: &str = "task-20261028-bbbb";
/// An id no tenant holds.
const NOWHERE: &str = "task-20261028-ffff";

struct Env {
    oidc: OidcMock,
    tenants: TenantWorkspaces,
    security_dir: tempfile::TempDir,
    home: tempfile::TempDir,
    jti: AtomicU32,
}

impl Env {
    async fn new() -> Self {
        let mut tenants = TenantWorkspaces::new();
        for noyau in ["a", "b"] {
            let _ = tenants.add(noyau);
            tenants
                .tenant(noyau)
                .unwrap()
                .install_task_work_formula()
                .unwrap();
        }
        let oidc = OidcMock::start_with(OidcMockConfig {
            audiences: vec![AUDIENCE.to_owned(), AUDIENCE_B.to_owned()],
            ..OidcMockConfig::default()
        })
        .await;
        let env = Self {
            oidc,
            tenants,
            security_dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            jti: AtomicU32::new(0),
        };
        env.plant(
            "a",
            IDEA,
            &json!({"kind": "idea", "variables": {"topic": "x"}}),
        );
        env.plant(
            "a",
            TASK_1,
            &json!({"kind": "task", "variables": {"topic": "one"}}),
        );
        env.plant(
            "a",
            TASK_2,
            &json!({"kind": "task", "variables": {"extra": "two"}}),
        );
        env.plant("a", DECISION, &json!({"kind": "decision"}));
        env.plant(
            "b",
            FOREIGN,
            &json!({"kind": "task", "variables": {"secret": "b"}}),
        );
        env
    }

    fn state_dir(&self, noyau: &str) -> PathBuf {
        self.tenants.tenant(noyau).unwrap().state_dir.clone()
    }

    fn plant(&self, noyau: &str, id: &str, overrides: &Value) {
        self.tenants
            .tenant(noyau)
            .unwrap()
            .insert_molecule(id, overrides)
            .unwrap();
    }

    fn store(&self, noyau: &str) -> FileStore {
        FileStore::new(self.state_dir(noyau))
    }

    fn load(&self, noyau: &str, id: &str) -> MoleculeData {
        self.store(noyau)
            .load_molecule(&MoleculeId::new(id).unwrap())
            .unwrap()
    }

    /// Every file under `dir` with its bytes: the store, byte for byte.
    fn snapshot(&self, noyau: &str) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let root = self.state_dir(noyau);
        let mut out = BTreeMap::new();
        walk(&root, &root, &mut out);
        out
    }

    /// The molecule ids a tenant's store holds.
    fn molecules(&self, noyau: &str) -> Vec<String> {
        let dir = self.state_dir(noyau).join("fleets/default/molecules");
        let mut ids: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort();
        ids
    }

    fn app(&self) -> axum::Router {
        let dir = self.security_dir.path();
        let _ = self.oidc.write_jwks_file(dir).unwrap();
        let jwks = JwksStore::load(dir).unwrap();
        let nucleons = HabilitationMap::builder()
            .insert(
                self.oidc.issuer(),
                "sub-a",
                HabilitationId::new("nuc-a"),
                Noyau::new("a"),
                AUDIENCE,
            )
            .insert(
                self.oidc.issuer(),
                "sub-b",
                HabilitationId::new("nuc-b"),
                Noyau::new("b"),
                AUDIENCE_B,
            )
            .build();
        let store: Arc<dyn SessionStore> = Arc::new(FilesystemSessionStore::new(dir).unwrap());
        router(AppState {
            harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
            worker_backend: WorkerBackends::fixed(Arc::new(MockBackend::new())),
            state_dir: dir.to_path_buf(),
            inbox_root: dir.join("whispers/inbox"),
            galaxies_root: self.tenants.galaxies_root().to_path_buf(),
            jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
            nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleons),
            rate_limiter: Arc::new(IngressRateLimiter::new(
                dir.join("oidc-rate-limit"),
                4096.0,
                0.0,
            )),
            deny_list: Arc::new(DenyList::new(dir.to_path_buf()).with_ttl(Duration::from_secs(0))),
            posture: Posture::Prepared,
            drain_timeout: Duration::from_secs(10),
            anthropic_api_key: None,
            claude_model: None,
            backend_health: Arc::new(BackendHealthRegistry::new()),
            auth_claude: Some(Arc::new(AuthClaudeState::new(
                AuthClaudeConfig::defaults_with_home(self.home.path()),
                store,
            ))),
            artifact_root: dir.join("artifacts"),
            dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
                dir.join("dist"),
            )),
            install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
            events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
            metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
            drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
            admin_seal: Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
            provisioner: Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
            portee_provisioner: Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::inert()),
        })
    }

    fn jwt_for(&self, subject: &str, audience: &str, scopes: &[&str]) -> String {
        let n = self.jti.fetch_add(1, Ordering::SeqCst);
        self.oidc.issue(&IssueJwt {
            subject,
            audience: Some(audience),
            scopes,
            lifetime_secs: Some(60),
            jti: Some(&format!("jti-e2-{n}")),
        })
    }

    fn jwt(&self, scopes: &[&str]) -> String {
        self.jwt_for("sub-a", AUDIENCE, scopes)
    }

    /// Call as `sub-a`, the caller bound to tenant `a`.
    async fn post(&self, uri: &str, scopes: &[&str], body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", format!("Bearer {}", self.jwt(scopes)))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = self.app().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

/// What a molecule's typed links say, with ids replaced by the role the id
/// plays in the fixture (`names` maps an id to its role).
fn links(data: &MoleculeData, names: &[(&str, &str)]) -> Vec<String> {
    let name = |id: &MoleculeId| {
        names
            .iter()
            .find(|(raw, _)| *raw == id.as_str())
            .map_or_else(|| id.to_string(), |(_, role)| (*role).to_owned())
    };
    data.typed_links
        .iter()
        .map(|link| match link {
            MoleculeLink::DecayedFrom { id } => format!("decayed_from:{}", name(id)),
            MoleculeLink::DecayProduct { id } => format!("decay_product:{}", name(id)),
            MoleculeLink::MergedFrom { ids } => format!(
                "merged_from:{}",
                ids.iter().map(&name).collect::<Vec<_>>().join(",")
            ),
            MoleculeLink::MergedInto { id } => format!("merged_into:{}", name(id)),
            MoleculeLink::TransformedFrom { kind } => format!("transformed_from:{kind}"),
            other => format!("{other:?}"),
        })
        .collect()
}

/// One molecule as the parity projection sees it: kind, status, links.
fn project(data: &MoleculeData, names: &[(&str, &str)]) -> Value {
    json!({
        "kind": data.kind.map(|k| k.to_string()),
        "status": data.status.to_string(),
        "formula": data.formula_id.as_str(),
        "links": links(data, names),
    })
}

/// The projection `cs decay` leaves for `IDEA` decayed on `task-work`. The
/// same literal is asserted in `remote_interaction_parity.rs`.
fn expected_decay() -> Value {
    json!({
        "source": {
            "kind": "idea", "status": "completed", "formula": "task-work",
            "links": ["decay_product:<product>"],
        },
        "product": {
            "kind": "task", "status": "pending", "formula": "task-work",
            "links": ["decayed_from:<source>"],
        },
    })
}

/// The projection `cs merge` leaves for two tasks merged on `task-work`.
fn expected_merge() -> Value {
    json!({
        "source_1": {
            "kind": "task", "status": "completed", "formula": "task-work",
            "links": ["merged_into:<product>"],
        },
        "source_2": {
            "kind": "task", "status": "completed", "formula": "task-work",
            "links": ["merged_into:<product>"],
        },
        "product": {
            "kind": "decision", "status": "pending", "formula": "task-work",
            "links": ["merged_from:<source_1>,<source_2>"],
        },
    })
}

/// The projection `cs transform --to task` leaves on an idea.
fn expected_transform() -> Value {
    json!({
        "molecule": {
            "kind": "task", "status": "pending", "formula": "task-work",
            "links": ["transformed_from:idea"],
        },
    })
}

fn interactions(env: &Env, noyau: &str) -> Vec<Value> {
    std::fs::read_to_string(env.state_dir(noyau).join("interactions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The kinds of the events a tenant's log holds: the legacy envelopes tag
/// theirs `kind`, the `EventV2` records `type`.
fn event_types(env: &Env, noyau: &str) -> Vec<String> {
    std::fs::read_to_string(env.state_dir(noyau).join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            v["kind"]
                .as_str()
                .or_else(|| v["type"].as_str())
                .map(str::to_owned)
        })
        .collect()
}

// ─── decay ─────────────────────────────────────────────────────────────────

/// Decaying an idea over HTTP leaves the store as `cs decay` does: the source
/// completed and pointing at its product, the product a pending task pointing
/// back, one interaction record, the decay events.
#[tokio::test]
async fn decay_leaves_the_state_cs_decay_leaves() {
    let env = Env::new().await;
    let (status, body) = env
        .post(
            &format!("/v1/molecules/{IDEA}/decay"),
            &[WRITE],
            json!({"formula": "task-work", "reason": "split the idea"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["decay"]["interaction"], "decay");
    assert_eq!(body["decay"]["source"], IDEA);
    assert_eq!(body["decay"]["product_kind"], "task");
    let products = body["decay"]["products"].as_array().unwrap();
    assert_eq!(products.len(), 1);
    let product = products[0].as_str().unwrap();

    let names = [(IDEA, "<source>"), (product, "<product>")];
    assert_eq!(
        json!({
            "source": project(&env.load("a", IDEA), &names),
            "product": project(&env.load("a", product), &names),
        }),
        expected_decay()
    );
    let log = interactions(&env, "a");
    assert_eq!(log.len(), 1, "one interaction record: {log:?}");
    assert_eq!(log[0]["type"], "decay");
    assert_eq!(log[0]["source"], IDEA);
    assert_eq!(log[0]["reason"], "split the idea");
    let events = event_types(&env, "a");
    assert!(
        events.contains(&"molecule_decayed".to_owned()),
        "the legacy decay event: {events:?}"
    );
}

/// Without `cosmon:molecule:write` the route answers 403 and writes nothing.
#[tokio::test]
async fn decay_without_the_write_scope_is_forbidden() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    for scopes in [&[READ][..], &[][..]] {
        let (status, _) = env
            .post(
                &format!("/v1/molecules/{IDEA}/decay"),
                scopes,
                json!({"formula": "task-work", "reason": "r"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "scopes {scopes:?}");
    }
    assert_eq!(env.snapshot("a"), before);
}

/// A kind that cannot decay is refused and nothing is written.
#[tokio::test]
async fn decay_of_a_task_is_refused_and_writes_nothing() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    let (status, body) = env
        .post(
            &format!("/v1/molecules/{TASK_1}/decay"),
            &[WRITE],
            json!({"formula": "task-work", "reason": "r"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "cannot_decay");
    assert_eq!(env.snapshot("a"), before);
}

/// A `blocks` target that does not exist is refused before the product is
/// written (the CLI used to leave the product behind).
#[tokio::test]
async fn decay_with_a_missing_blocks_target_writes_nothing() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    let (status, body) = env
        .post(
            &format!("/v1/molecules/{IDEA}/decay"),
            &[WRITE],
            json!({"formula": "task-work", "reason": "r", "blocks": [NOWHERE]}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_found");
    assert_eq!(env.snapshot("a"), before);
}

/// A molecule of another binding cannot be decayed: it is not in the caller's
/// store, which is the answer for a molecule that does not exist.
#[tokio::test]
async fn decay_of_a_foreign_molecule_is_not_found() {
    let env = Env::new().await;
    env.plant("b", "idea-20261028-bbbb", &json!({"kind": "idea"}));
    let before = (env.snapshot("a"), env.snapshot("b"));
    let (status, body) = env
        .post(
            "/v1/molecules/idea-20261028-bbbb/decay",
            &[WRITE],
            json!({"formula": "task-work", "reason": "r"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_found");
    assert_eq!((env.snapshot("a"), env.snapshot("b")), before);
}

// ─── transform ─────────────────────────────────────────────────────────────

/// Transforming an idea into a task leaves the store as `cs transform` does.
#[tokio::test]
async fn transform_leaves_the_state_cs_transform_leaves() {
    let env = Env::new().await;
    let (status, body) = env
        .post(
            &format!("/v1/molecules/{IDEA}/transform"),
            &[WRITE],
            json!({"to": "task", "reason": "ready to build"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["transform"]["from"], "idea");
    assert_eq!(body["transform"]["to"], "task");
    assert_eq!(
        json!({"molecule": project(&env.load("a", IDEA), &[])}),
        expected_transform()
    );
    let log = interactions(&env, "a");
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["type"], "transform");
    assert!(event_types(&env, "a").contains(&"molecule_transformed".to_owned()));
}

/// Without `cosmon:molecule:write` the route answers 403 and writes nothing.
#[tokio::test]
async fn transform_without_the_write_scope_is_forbidden() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    for scopes in [&[READ][..], &[][..]] {
        let (status, _) = env
            .post(
                &format!("/v1/molecules/{IDEA}/transform"),
                scopes,
                json!({"to": "task", "reason": "r"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "scopes {scopes:?}");
    }
    assert_eq!(env.snapshot("a"), before);
}

/// An edge the kind graph does not have is refused, nothing written.
#[tokio::test]
async fn transform_along_a_missing_edge_is_refused() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    let (status, body) = env
        .post(
            &format!("/v1/molecules/{DECISION}/transform"),
            &[WRITE],
            json!({"to": "task", "reason": "r"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "not_transformable");
    assert_eq!(env.snapshot("a"), before);
}

// ─── merge ─────────────────────────────────────────────────────────────────

fn merge_body(sources: &[&str]) -> Value {
    json!({"sources": sources, "formula": "task-work", "reason": "synthesise"})
}

/// Merging two tasks over HTTP leaves the store as `cs merge` does: both
/// sources completed and pointing at the synthesis, the synthesis a pending
/// decision pointing back at both, one interaction record, the merge event.
#[tokio::test]
async fn merge_leaves_the_state_cs_merge_leaves() {
    let env = Env::new().await;
    let (status, body) = env
        .post(
            "/v1/molecules/merge",
            &[WRITE],
            merge_body(&[TASK_1, TASK_2]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["merge"]["interaction"], "merge");
    assert_eq!(body["merge"]["product_kind"], "decision");
    let product = body["merge"]["product"].as_str().unwrap();

    let names = [
        (TASK_1, "<source_1>"),
        (TASK_2, "<source_2>"),
        (product, "<product>"),
    ];
    assert_eq!(
        json!({
            "source_1": project(&env.load("a", TASK_1), &names),
            "source_2": project(&env.load("a", TASK_2), &names),
            "product": project(&env.load("a", product), &names),
        }),
        expected_merge()
    );
    // The synthesis carries the variables of every source.
    let synthesis = env.load("a", product);
    assert_eq!(
        synthesis.variables.get("topic").map(String::as_str),
        Some("one")
    );
    assert_eq!(
        synthesis.variables.get("extra").map(String::as_str),
        Some("two")
    );
    let log = interactions(&env, "a");
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["type"], "merge");
    assert!(event_types(&env, "a").contains(&"molecule_merged".to_owned()));
}

/// Without `cosmon:molecule:write` the route answers 403 and writes nothing.
#[tokio::test]
async fn merge_without_the_write_scope_is_forbidden() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    for scopes in [&[READ][..], &[][..]] {
        let (status, _) = env
            .post("/v1/molecules/merge", scopes, merge_body(&[TASK_1, TASK_2]))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "scopes {scopes:?}");
    }
    assert_eq!(env.snapshot("a"), before);
}

/// One source outside the caller's binding refuses the whole merge: no
/// synthesis molecule, no source changed, no event, no interaction record —
/// the caller's store and the other binding's store are byte-for-byte what
/// they were. The foreign source is tried in every position, because a check
/// that looks only at the first source would let the others through.
#[tokio::test]
async fn merge_with_a_source_outside_the_binding_changes_nothing() {
    let env = Env::new().await;
    let before = (env.snapshot("a"), env.snapshot("b"));
    let molecules_before = env.molecules("a");

    for sources in [
        [FOREIGN, TASK_1, TASK_2],
        [TASK_1, FOREIGN, TASK_2],
        [TASK_1, TASK_2, FOREIGN],
    ] {
        let (status, body) = env
            .post("/v1/molecules/merge", &[WRITE], merge_body(&sources))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{sources:?}: {body}");
        assert_eq!(body["error"], "not_found", "{sources:?}: {body}");
        assert_eq!(
            (env.snapshot("a"), env.snapshot("b")),
            before,
            "{sources:?} left a trace"
        );
        assert_eq!(env.molecules("a"), molecules_before, "{sources:?}");
    }
    assert!(!env.state_dir("a").join("interactions.jsonl").exists());
    assert!(!event_types(&env, "a").contains(&"molecule_merged".to_owned()));
}

/// The refusal for a source of another binding is the refusal for a source
/// that does not exist: same status, same body but for the request id. Nothing
/// in it tells the caller the foreign molecule is there.
#[tokio::test]
async fn the_refusal_for_a_foreign_source_is_that_of_a_nonexistent_one() {
    let env = Env::new().await;
    let (foreign_status, mut foreign) = env
        .post(
            "/v1/molecules/merge",
            &[WRITE],
            merge_body(&[TASK_1, FOREIGN]),
        )
        .await;
    let (nowhere_status, mut nowhere) = env
        .post(
            "/v1/molecules/merge",
            &[WRITE],
            merge_body(&[TASK_1, NOWHERE]),
        )
        .await;
    assert_eq!(foreign_status, nowhere_status);
    foreign.as_object_mut().unwrap().remove("request_id");
    nowhere.as_object_mut().unwrap().remove("request_id");
    assert_eq!(foreign, nowhere);
    let text = foreign.to_string();
    assert!(
        !text.contains(FOREIGN) && !text.contains(NOWHERE),
        "the refusal names a source: {text}"
    );
}

/// The caller bound to tenant `b` can merge what is in `b`, and sees nothing
/// of `a`: the check is the binding, not a property of the ids.
#[tokio::test]
async fn each_binding_merges_only_its_own_molecules() {
    let env = Env::new().await;
    env.plant("b", "task-20261028-b2b2", &json!({"kind": "task"}));
    let req = |sources: &[&str]| {
        Request::builder()
            .method("POST")
            .uri("/v1/molecules/merge")
            .header(
                "Authorization",
                format!("Bearer {}", env.jwt_for("sub-b", AUDIENCE_B, &[WRITE])),
            )
            .header("content-type", "application/json")
            .body(Body::from(merge_body(sources).to_string()))
            .unwrap()
    };
    let refused = env
        .app()
        .oneshot(req(&[FOREIGN, TASK_1]))
        .await
        .unwrap()
        .status();
    assert_eq!(refused, StatusCode::NOT_FOUND, "b reaching a's TASK_1");
    let merged = env
        .app()
        .oneshot(req(&[FOREIGN, "task-20261028-b2b2"]))
        .await
        .unwrap()
        .status();
    assert_eq!(merged, StatusCode::OK, "b merging its own pair");
    assert_eq!(env.load("a", TASK_1).status.to_string(), "pending");
}

/// A source of a kind that cannot merge is the caller's own molecule, so the
/// refusal names it. Nothing is written.
#[tokio::test]
async fn merge_names_a_held_source_that_cannot_merge() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    let (status, body) = env
        .post(
            "/v1/molecules/merge",
            &[WRITE],
            merge_body(&[TASK_1, DECISION]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "source_not_mergeable");
    assert_eq!(body["source"], DECISION);
    assert_eq!(env.snapshot("a"), before);
}

/// One source is not a merge.
#[tokio::test]
async fn merge_of_a_single_source_is_refused() {
    let env = Env::new().await;
    let before = env.snapshot("a");
    let (status, body) = env
        .post("/v1/molecules/merge", &[WRITE], merge_body(&[TASK_1]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "merge_needs_two_sources");
    assert_eq!(env.snapshot("a"), before);
}

// ─── cosmon-remote, end to end ─────────────────────────────────────────────

/// A live adapter on loopback, driven by the real `cosmon-remote` client.
async fn serve(env: &Env) -> Profile {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = env.app();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Profile {
        host: format!("http://{addr}"),
        sub: "sub-a".to_owned(),
        aud: AUDIENCE.to_owned(),
        oidc_url: env.oidc.issuer().to_owned(),
        issuer: None,
        client_id: None,
        attachment: None,
        noyau: Some("a".to_owned()),
        scopes: vec![READ.to_owned(), WRITE.to_owned()],
        artifacts_dir: None,
        timeout_secs: 30,
        phone_home: false,
    }
}

/// `cosmon-remote molecule merge`, `decay` and `transform` through the real
/// adapter: the client's own methods, the production router, a real store.
/// The merge half includes the refusal: a foreign source comes back as the
/// client error a nonexistent one does, and the store is not touched.
#[tokio::test]
async fn cosmon_remote_drives_merge_decay_and_transform_through_the_adapter() {
    let env = Env::new().await;
    let profile = serve(&env).await;
    let client = Client::new(&profile, Some(env.jwt(&[READ, WRITE]))).unwrap();

    let before = env.snapshot("a");
    let refused = client
        .merge(
            &[TASK_1.to_owned(), FOREIGN.to_owned()],
            "task-work",
            "synthesise",
            None,
        )
        .await
        .expect_err("a foreign source refuses the merge");
    assert!(
        refused.to_string().contains("not_found"),
        "refusal reads as a missing molecule: {refused}"
    );
    assert_eq!(env.snapshot("a"), before, "the refused merge wrote");

    let merged = client
        .merge(
            &[TASK_1.to_owned(), TASK_2.to_owned()],
            "task-work",
            "synthesise",
            None,
        )
        .await
        .expect("merge through the adapter");
    let product = merged["merge"]["product"].as_str().unwrap();
    assert_eq!(env.load("a", TASK_1).status.to_string(), "completed");
    assert_eq!(env.load("a", product).kind.unwrap().to_string(), "decision");

    let decayed = client
        .decay(IDEA, "task-work", "split", None, &[])
        .await
        .expect("decay through the adapter");
    assert_eq!(decayed["decay"]["products"].as_array().unwrap().len(), 1);

    env.plant("a", "idea-20261028-cccc", &json!({"kind": "idea"}));
    let transformed = client
        .transform("idea-20261028-cccc", "task", "ready")
        .await
        .expect("transform through the adapter");
    assert_eq!(transformed["transform"]["to"], "task");
}
