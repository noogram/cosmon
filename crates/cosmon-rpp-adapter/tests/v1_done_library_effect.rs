// SPDX-License-Identifier: AGPL-3.0-only

//! `POST /v1/molecules/{id}/done` **merges**, on a deployment that declares
//! nothing (ADR-176 §12's follow-up).
//!
//! `v1_done.rs` pins the door's decision half and the typed `501` an
//! effect-less deployment answers. This file pins the property that made the
//! `501` a defect rather than a design: a stock adapter, with no
//! `harvest_cs_binary` line anywhere, closes a real molecule in a real git
//! tenant and leaves the merge commit — with the lineage trailers `cs done`
//! writes — on the base branch.
//!
//! Each test is written to fail for its own reason:
//!
//! 1. `a_stock_deployment_merges_and_writes_the_lineage_trailers` — the whole
//!    claim, asserted on `git log` and on the commit's trailer block, whose
//!    expected text is a **golden captured from the `cs` binary built before
//!    the transaction moved** (see the molecule's `result.md`, falsifier 3).
//!    Comparing against a value the new code computes would prove only that
//!    it agrees with itself.
//! 2. `no_cs_process_is_spawned_on_the_library_path` — the same harvest with
//!    a poisoned `cs` on `PATH`: a shim that records having been run and then
//!    fails. A green merge with an unwritten witness is what "in-process"
//!    means.
//!
//! Both tests set `PATH`, which is process-wide, so they live in their own
//! test binary and install the *same* shim directory — no test in this file
//! can observe another's `PATH`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantPath, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::{json, Value};
use tower::ServiceExt;

/// The merge message `cs done` wrote for a molecule with no dependencies,
/// captured from the binary built at the commit **before** this crate could
/// call the transaction as a library.
///
/// `{id}` is the only substitution. Two trailers and no more: a completion
/// merge always projects `Mol-Id` and `Mission-Id`, and this molecule has no
/// `Depends-On` edge to project.
const MERGE_MESSAGE_GOLDEN: &str = "Merge branch 'feat/{id}'\n\nMol-Id: {id}\nMission-Id: {id}\n\n";

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().unwrap()];
    full.extend_from_slice(args);
    Command::new("git").args(&full).output().expect("git")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Arm `[harvest_authority] required` **and** seal the grant it then demands.
///
/// ADR-172 §D1 is two halves, and a test that armed only the first would sail
/// past the door's `not_authorized` and then fail inside the transaction at
/// its effect boundary — which is exactly what a stock deployment must NOT do.
/// So the tenant gets a pinned operator key and one grant per molecule,
/// sealed by the test operator. The shipped tree owns no such signer.
fn seal_grant_for(tenant: &TenantPath, molecule: &str, base: &str) {
    use cosmon_core::harvest_authorization::{
        DoneAuthorization, GrantEpoch, HarvestAction, HarvestGrant, HarvestScope,
    };
    use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};

    let operator = cosmon_minisign_testkit::Operator::from_seed(7);
    std::fs::write(
        tenant.root.join(cosmon_filestore::HARVEST_PUBKEY_REL),
        operator.public_key_file(),
    )
    .unwrap();

    let id = cosmon_core::id::MoleculeId::new(molecule).unwrap();
    let grant = HarvestGrant::new(
        "library-harvest",
        HarvestScope::Molecule {
            molecule: id.clone(),
        },
        base,
        HarvestAction::Done,
        std::iter::empty::<&str>(),
        GrantEpoch::first(),
        None,
    )
    .unwrap();

    let minisig = operator.sign(&grant.canonical_bytes());
    let mut lines = minisig.lines();
    let untrusted = lines
        .next()
        .and_then(|l| l.strip_prefix("untrusted comment: "))
        .unwrap_or_default()
        .to_owned();
    let signature = lines.next().unwrap_or_default().to_owned();
    let trusted = lines
        .next()
        .and_then(|l| l.strip_prefix("trusted comment: "))
        .unwrap_or_default()
        .to_owned();
    let global_signature = lines.next().unwrap_or_default().to_owned();

    let seal = cosmon_core::harvest_authorization::OperatorHarvestSeal::new(
        grant,
        OperatorAttestation {
            key_id: OperatorKeyId::parse(&operator.key_id_display()).unwrap(),
            signature,
            global_signature,
            trusted_comment: trusted,
            untrusted_comment: untrusted,
        },
    )
    .unwrap();
    let auth = DoneAuthorization::Ratified(seal);
    cosmon_filestore::harvest_authority::store_authorization(&tenant.state_dir, &id, &auth)
        .unwrap();
}

/// A tenant galaxy that is also a real git repository: one commit on `main`,
/// a `.cosmon/` that is NOT tracked, an armed `[harvest_authority]`, and a
/// project identity (the transaction refuses without one).
fn arm_git_tenant(tenant: &TenantPath) {
    let repo = &tenant.root;
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "worker@example.com"]);
    git_ok(repo, &["config", "user.name", "Worker"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);

    let cosmon = repo.join(".cosmon");
    std::fs::create_dir_all(cosmon.join("state")).unwrap();
    std::fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"library-harvest\"\n\n\
         [harvest_authority]\nrequired = true\n",
    )
    .unwrap();
    std::fs::write(
        cosmon.join("state/fleet.json"),
        serde_json::to_vec(&json!({"workers": {}})).unwrap(),
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-qm", "base"]);
}

/// A `Completed`, unmerged molecule with one commit on its own branch — the
/// shape the whole decision half admits and the effect half has work to do
/// for.
fn plant_completed_with_work(tenant: &TenantPath, id: &str) {
    tenant
        .insert_molecule(id, &json!({"status": "completed"}))
        .unwrap();
    let repo = &tenant.root;
    let branch = format!("feat/{id}");
    git_ok(repo, &["checkout", "-q", "-b", &branch, "main"]);
    std::fs::write(repo.join("worker.txt"), "worker\n").unwrap();
    git_ok(repo, &["add", "worker.txt"]);
    git_ok(repo, &["commit", "-qm", "worker output"]);
    git_ok(repo, &["checkout", "-q", "main"]);
}

/// A directory holding a `cs` that must never run, and the witness it writes
/// if it does.
///
/// Prepended to `PATH` rather than replacing it: `git` still has to resolve,
/// because the harvest is nothing but git. Only the name `cs` is poisoned.
fn poison_cs_on_path(dir: &Path) -> std::path::PathBuf {
    let witness = dir.join("cs-was-spawned");
    let shim = dir.join("cs");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\nexit 1\n",
            witness.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{path}", dir.display()));
    witness
}

/// The adapter as an operator gets it out of the box: `harvest_cs_binary`
/// absent, so [`cosmon_rpp_adapter::harvest_effect::from_config`] chooses the
/// library.
fn stock_state(oidc: &OidcMock, tenants: &TenantWorkspaces, security_dir: &Path) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();

    AppState {
        // The whole point: no configuration line, and the harvest still runs.
        harvest_effect: cosmon_rpp_adapter::harvest_effect::from_config(None),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::Fixed(
            cosmon_rpp_adapter::worker_env::SharedBackend(Arc::new(
                cosmon_transport::MockBackend::new(),
            )),
        ),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(IngressRateLimiter::new(
            security_dir.join("oidc-rate-limit"),
            64.0,
            0.0,
        )),
        deny_list: Arc::new(DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::ZERO)),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: security_dir.join("artifacts"),
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            security_dir.join("dist"),
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

fn done_request(jwt: &str, id: &str, reason: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/molecules/{id}/done"))
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({ "reason": reason })).unwrap(),
        ))
        .unwrap()
}

fn jwt_with(oidc: &OidcMock, jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

async fn oidc_mock() -> OidcMock {
    OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await
}

/// The headline: no configuration, and the work lands on `main` under a merge
/// commit carrying the trailers `cs done` writes.
#[tokio::test]
async fn a_stock_deployment_merges_and_writes_the_lineage_trailers() {
    let shim_dir = tempfile::tempdir().unwrap();
    let witness = poison_cs_on_path(shim_dir.path());

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_git_tenant(&tenant_a);
    let id = "task-20260909-1ib1";
    plant_completed_with_work(&tenant_a, id);
    seal_grant_for(&tenant_a, id, "main");

    let main_before = git(&tenant_a.root, &["rev-parse", "main"]);
    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(stock_state(&oidc, &tenants, security_dir.path()));

    let resp = app
        .oneshot(done_request(
            &jwt_with(&oidc, "jti-lib-merge"),
            id,
            "the route closes this molecule",
        ))
        .await
        .unwrap();

    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "a stock deployment must MERGE, not refuse: {body}"
    );
    assert_eq!(body["harvest"]["outcome"], "landed");

    // 1. `main` moved, and it moved to a merge commit.
    let main_after = git(&tenant_a.root, &["rev-parse", "main"]);
    assert_ne!(
        String::from_utf8_lossy(&main_before.stdout),
        String::from_utf8_lossy(&main_after.stdout),
        "the base branch must have advanced"
    );
    assert!(
        git(&tenant_a.root, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "the worker's file must be on main after the harvest"
    );

    // 2. The merge message is byte-for-byte what the pre-move `cs` binary
    //    wrote — subject line and lineage trailer block.
    let message =
        String::from_utf8_lossy(&git(&tenant_a.root, &["log", "-1", "--format=%B", "main"]).stdout)
            .into_owned();
    assert_eq!(
        message,
        MERGE_MESSAGE_GOLDEN.replace("{id}", id),
        "a route-closed molecule must carry the same merge message a \
         CLI-closed one does"
    );

    // 3. And the reason the requester gave is traced trunk-side, not
    //    fabricated and not dropped.
    let state: Value = serde_json::from_slice(
        &std::fs::read(
            tenant_a
                .state_dir
                .join("fleets/default/molecules")
                .join(id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["harvest_reason"], "the route closes this molecule");
    assert!(state["merged_at"].is_string());

    // 4. The merge event landed in the TENANT's log.
    //
    // Not a detail: the transaction used to resolve this path by walking up
    // from the process's working directory, which for a server is the
    // adapter's own installation. A harvest for tenant `a` appended its
    // `MergeDispatched` to whatever galaxy the adapter lives in, and then
    // tried to commit that foreign path into the tenant's repository. Revert
    // the fix and this assertion goes red while every other one stays green.
    let events = std::fs::read_to_string(tenant_a.state_dir.join("events.jsonl"))
        .expect("the tenant's own events log must exist");
    assert!(
        events.contains(id),
        "the harvest's events must be written to the tenant's log, not to the \
         galaxy the server happens to be installed in"
    );

    assert!(
        !witness.exists(),
        "no `cs` process may be spawned on the library path"
    );
}

/// The same harvest, stated as the negative: the door reached the effect and
/// the effect reached git, with a `cs` on `PATH` that would have recorded
/// itself and failed the merge had anything shelled out to it.
///
/// The witness is the falsifier. Restore `UnavailableHarvestEffect` as the
/// default and the status assertion fails; wire a subprocess effect that
/// discovers `cs` on `PATH` and the witness assertion fails.
#[tokio::test]
async fn no_cs_process_is_spawned_on_the_library_path() {
    let shim_dir = tempfile::tempdir().unwrap();
    let witness = poison_cs_on_path(shim_dir.path());

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_git_tenant(&tenant_a);
    let id = "task-20260909-1ib2";
    plant_completed_with_work(&tenant_a, id);
    seal_grant_for(&tenant_a, id, "main");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(stock_state(&oidc, &tenants, security_dir.path()));

    let resp = app
        .oneshot(done_request(
            &jwt_with(&oidc, "jti-lib-nospawn"),
            id,
            "closed in-process",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert!(
        !witness.exists(),
        "the harvest ran a `cs` found on PATH — the library path must spawn \
         no `cs` at all, and there is deliberately no PATH discovery"
    );
}
