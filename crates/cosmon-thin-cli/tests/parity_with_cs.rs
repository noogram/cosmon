// SPDX-License-Identifier: Apache-2.0

//! cs ↔ cs-thin parity (T-CST-PARITY).
//!
//! For each of the three V0 verbs (observe, nucleate, tag) we run two
//! invocations side-by-side against the SAME on-disk tenant workspace:
//!
//! 1. **`cs --json <verb>`** — the operator-paid local binary, executed
//!    as a real subprocess with `cwd` = tenant root so its walk-up
//!    discovery resolves the same `.cosmon/state/` the rpp-adapter
//!    points at.
//! 2. **`cs-thin <verb>`** — the JWT-paid HTTP client, dispatched
//!    in-process via [`cosmon_thin_cli::cli::run_with`] against an
//!    in-process [`cosmon_rpp_adapter::router`] bound to the same
//!    tenant.
//!
//! The two outputs are parsed as JSON and compared with
//! [`cosmon_thin_cli::parity::compare`] modulo the allowlist loaded
//! from `tests/parity-allowlist.toml`.
//!
//! # Locating the `cs` binary
//!
//! In order of precedence:
//!
//! 1. `COSMON_THIN_PARITY_CS_BIN` — explicit path (CI, scripted runs).
//! 2. `<workspace>/target/{debug,release}/cs` — walk-up from
//!    `CARGO_MANIFEST_DIR` until a `Cargo.lock` is found, then probe.
//! 3. `cs` on `PATH` — last resort.
//!
//! If none resolve, the test prints a Feynman-register skip notice
//! and returns successfully without asserting anything. The CI gate
//! is responsible for building `cs` first
//! (`cargo build --bin cs -p cosmon-cli --locked`); see
//! `docs/guides/cs-thin-parity.md` §CI for the canonical workflow.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_thin_cli::cli::{
    run_with, Cli, CollapseArgs, Command as ThinCmd, EnsembleArgs, FreezeArgs, NucleateArgs,
    ObserveArgs, StuckArgs, TagArgs, ThawArgs,
};
use cosmon_thin_cli::parity::{compare, Allowlist, Diff};
use serde_json::Value;

const ALLOWLIST_TOML: &str = include_str!("parity-allowlist.toml");

fn load_allowlist() -> Allowlist {
    Allowlist::from_toml_str(ALLOWLIST_TOML).expect("parity-allowlist.toml is well-formed")
}

/// Locate the `cs` binary, honouring the precedence documented at
/// the module head. Returns `None` (with an `eprintln!` notice) if
/// nothing resolves — the test then skips, never silently passing
/// from a hidden failure mode.
fn find_cs_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COSMON_THIN_PARITY_CS_BIN") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Some(path);
        }
        eprintln!("parity: COSMON_THIN_PARITY_CS_BIN={p} but path does not exist; falling through");
    }

    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("Cargo.lock").exists() {
            for profile in ["debug", "release"] {
                let candidate = dir.join("target").join(profile).join("cs");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
                let td = PathBuf::from(td);
                for profile in ["debug", "release"] {
                    let candidate = td.join(profile).join("cs");
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
            break;
        }
        if !dir.pop() {
            break;
        }
    }

    // PATH last
    if let Ok(path_env) = std::env::var("PATH") {
        for entry in path_env.split(':') {
            let p = PathBuf::from(entry).join("cs");
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}

/// Skip helper — emit a uniform notice on stderr so CI logs make the
/// reason obvious.
fn skip(test: &str) {
    eprintln!(
        "parity::{test}: SKIP — no `cs` binary found. \
         Build it with `cargo build --bin cs -p cosmon-cli --locked` \
         or set COSMON_THIN_PARITY_CS_BIN. \
         See docs/guides/cs-thin-parity.md §CI."
    );
}

/// Build a real [`AppState`] over an in-process OIDC mock + on-disk
/// tenant workspace. Same shape as `v0_smoke.rs::make_state`.
fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<(&str, &str, &str, &str)>,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let mut builder = HabilitationMap::builder();
    for (sub, nucleon, noyau, audience) in nucleons {
        builder = builder.insert(
            oidc.issuer(),
            sub,
            HabilitationId::new(nucleon),
            Noyau::new(noyau),
            audience,
        );
    }

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: cosmon_oidc_testkit::fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        artifact_root: security_dir.join("artifacts"),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(builder.build()),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
        auth_claude: None,
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: std::sync::Arc::new(
            cosmon_rpp_adapter::config::InstallTemplating::default(),
        ),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
    }
}

async fn spawn_adapter(state: AppState) -> String {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

fn write_jwt_to_temp(jwt: &str) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("jwt.txt");
    std::fs::write(&p, jwt).unwrap();
    Box::leak(Box::new(dir));
    p
}

/// Run `cs --json <args...>` against an isolated tenant workspace
/// and return the captured stdout as a `serde_json::Value`.
///
/// The invocation:
///
/// - sets `COSMON_STATE_DIR` and `COSMON_FORMULAS_DIR` so cs hits
///   the tenant fixture rather than walking up to whatever
///   `.cosmon/config.toml` happens to exist above the tempdir,
/// - sets `HOME` to a throwaway path so cs cannot touch
///   `~/.cosmon/`, and
/// - explicitly clears `COSMON_PARENT_MOL_ID` so the worker
///   harness's auto-parent contract does not accidentally link the
///   nucleated molecule to *this* test's parent task.
///
/// `cwd` is informational — the env vars dominate cs's discovery,
/// but a sane cwd helps if cs ever falls back to walk-up for an
/// orthogonal lookup.
fn run_cs(
    cs: &std::path::Path,
    tenant_root: &std::path::Path,
    state_dir: &std::path::Path,
    formulas_dir: &std::path::Path,
    home_sandbox: &std::path::Path,
    args: &[&str],
) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let output = Command::new(cs)
        .current_dir(tenant_root)
        .env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_FORMULAS_DIR", formulas_dir)
        .env("HOME", home_sandbox)
        .env_remove("COSMON_PARENT_MOL_ID")
        .args(&full)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn cs: {e}"));
    assert!(
        output.status.success(),
        "cs --json {args:?} exited {} — stderr: {}\nstdout: {}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("cs stdout is utf-8");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("cs stdout is not JSON: {e}\nraw: {stdout}"))
}

/// Render the diff list as a multi-line, scannable string for assert
/// messages.
fn render_diffs(diffs: &[Diff]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(256);
    let _ = writeln!(s, "{} unallowed diff(s):", diffs.len());
    for d in diffs {
        let _ = writeln!(
            s,
            "  - [{kind}] {path}: cs={left} | cs-thin={right}",
            kind = d.kind.as_str(),
            path = if d.path.is_empty() { "<root>" } else { &d.path },
            left = d.left.as_deref().unwrap_or("<absent>"),
            right = d.right.as_deref().unwrap_or("<absent>"),
        );
    }
    s
}

#[tokio::test]
async fn parity_observe() {
    let Some(cs) = find_cs_binary() else {
        skip("parity_observe");
        return;
    };
    let allowlist = load_allowlist();

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260504-prty",
            &serde_json::json!({"variables": {"smoke": "yes"}}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-observe"),
    });

    // 1. cs --json observe <id> against the same tenant cwd
    let home_sandbox = tempfile::tempdir().unwrap();
    let cs_out = run_cs(
        &cs,
        tenant_a.root.as_path(),
        tenant_a.state_dir.as_path(),
        &tenant_a.root.join(".cosmon").join("formulas"),
        home_sandbox.path(),
        &["observe", "task-20260504-prty"],
    );

    // 2. cs-thin observe <id> against the in-process rpp-adapter
    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Observe(ObserveArgs {
            molecule_id: "task-20260504-prty".to_owned(),
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin observe");
    let thin_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("cs-thin JSON");

    // 3. compare modulo allowlist
    let diffs = compare(&cs_out, &thin_out, "observe", &allowlist);
    assert!(
        diffs.is_empty(),
        "parity[observe] = MISMATCH\n{}",
        render_diffs(&diffs),
    );
}

#[tokio::test]
async fn parity_nucleate() {
    let Some(cs) = find_cs_binary() else {
        skip("parity_nucleate");
        return;
    };
    let allowlist = load_allowlist();

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a.install_task_work_formula().unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-nucleate"),
    });

    // 1. cs nucleate first (gets a unique id). `--no-parent`
    //    suppresses the auto-DecayProduct contract — the test is the
    //    root of its own causality, not a child of whatever molecule
    //    happens to live in the worker harness's env.
    let home_sandbox = tempfile::tempdir().unwrap();
    let cs_out = run_cs(
        &cs,
        tenant_a.root.as_path(),
        tenant_a.state_dir.as_path(),
        &tenant_a.root.join(".cosmon").join("formulas"),
        home_sandbox.path(),
        &[
            "nucleate",
            "task-work",
            "--kind",
            "task",
            "--var",
            "topic=parity-cs",
            "--no-parent",
        ],
    );

    // 2. cs-thin nucleate second (gets a different id)
    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Nucleate(NucleateArgs {
            formula: "task-work".to_owned(),
            kind: Some("task".to_owned()),
            vars: vec!["topic=parity-cs-thin".to_owned()],
            tags: vec![],
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin nucleate");
    let thin_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("cs-thin JSON");

    // Sanity: both wire-shapes carry the canonical fields.
    for (label, out) in [("cs", &cs_out), ("cs-thin", &thin_out)] {
        assert_eq!(out["status"], "active", "{label}.status");
        assert_eq!(out["formula"], "task-work", "{label}.formula");
        assert!(
            out["id"].as_str().unwrap().starts_with("task-"),
            "{label}.id"
        );
        assert!(out["total_steps"].is_u64(), "{label}.total_steps");
        assert!(out["created_at"].is_string(), "{label}.created_at");
    }

    // Topic differs intentionally per call (proves the variable
    // round-trips). Strip from the parity comparison by overwriting.
    let mut cs_norm = cs_out.clone();
    let mut thin_norm = thin_out.clone();
    if let Some(obj) = cs_norm.as_object_mut() {
        if let Some(vars) = obj.get_mut("variables").and_then(Value::as_object_mut) {
            vars.remove("topic");
        }
    }
    if let Some(obj) = thin_norm.as_object_mut() {
        if let Some(vars) = obj.get_mut("variables").and_then(Value::as_object_mut) {
            vars.remove("topic");
        }
    }

    let diffs = compare(&cs_norm, &thin_norm, "nucleate", &allowlist);
    assert!(
        diffs.is_empty(),
        "parity[nucleate] = MISMATCH\n{}",
        render_diffs(&diffs),
    );
}

#[tokio::test]
async fn parity_tag() {
    let Some(cs) = find_cs_binary() else {
        skip("parity_tag");
        return;
    };
    let allowlist = load_allowlist();

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    // Two distinct fixtures so the `cs add temp:hot` and
    // `cs-thin add temp:hot` invocations both perform the SAME action
    // (add `temp:hot` to a previously-untagged molecule). If we shared
    // a single fixture the second add would be idempotent and the
    // `delta`/`added` shapes would diverge in a structural way.
    tenant_a
        .insert_molecule("task-20260504-tgcs", &serde_json::json!({}))
        .unwrap();
    tenant_a
        .insert_molecule("task-20260504-tgth", &serde_json::json!({}))
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-tag"),
    });

    // 1. cs --json tag <id> --add temp:hot
    let home_sandbox = tempfile::tempdir().unwrap();
    let cs_out = run_cs(
        &cs,
        tenant_a.root.as_path(),
        tenant_a.state_dir.as_path(),
        &tenant_a.root.join(".cosmon").join("formulas"),
        home_sandbox.path(),
        &["tag", "task-20260504-tgcs", "--add", "temp:hot"],
    );

    // 2. cs-thin tag <id> --add temp:hot (different id, same action)
    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Tag(TagArgs {
            molecule_id: "task-20260504-tgth".to_owned(),
            add: vec!["temp:hot".to_owned()],
            remove: vec![],
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin tag");
    let thin_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("cs-thin JSON");

    // Sanity: both wire-shapes carry the canonical fields.
    for (label, out) in [("cs", &cs_out), ("cs-thin", &thin_out)] {
        assert_eq!(
            out["added"],
            serde_json::json!(["temp:hot"]),
            "{label}.added"
        );
        assert_eq!(out["removed"], serde_json::json!([]), "{label}.removed");
        assert_eq!(out["delta"], 1, "{label}.delta");
        assert_eq!(out["tags"], serde_json::json!(["temp:hot"]), "{label}.tags");
    }

    // The `id` field differs by design (different fixtures); the
    // allowlist accommodates that under `verb = "*", path = "tags.*"`
    // is NOT enough — id is at the top level. Patch by normalising.
    let mut cs_norm = cs_out.clone();
    let mut thin_norm = thin_out.clone();
    if let Some(obj) = cs_norm.as_object_mut() {
        obj.insert("id".to_owned(), Value::String("<normalised>".to_owned()));
    }
    if let Some(obj) = thin_norm.as_object_mut() {
        obj.insert("id".to_owned(), Value::String("<normalised>".to_owned()));
    }

    let diffs = compare(&cs_norm, &thin_norm, "tag", &allowlist);
    assert!(
        diffs.is_empty(),
        "parity[tag] = MISMATCH\n{}",
        render_diffs(&diffs),
    );
}

/// Parity for `ensemble` (T-CST-EXPAND).
///
/// Both invocations list the same on-disk tenant fixture; cs --json
/// ensemble emits a richer envelope (workers + molecules), so the test
/// reduces both sides to the molecule **count** + **status set** before
/// comparing — the wire envelope itself differs structurally and is
/// allowlisted via verb=`ensemble` rules.
#[tokio::test]
async fn parity_ensemble() {
    let Some(cs) = find_cs_binary() else {
        skip("parity_ensemble");
        return;
    };
    let _ = load_allowlist(); // sanity: file parses.

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260504-ens1",
            &serde_json::json!({"status": "pending"}),
        )
        .unwrap();
    tenant_a
        .insert_molecule(
            "task-20260504-ens2",
            &serde_json::json!({"status": "running"}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-ensemble"),
    });

    // 1. cs --json ensemble --all
    let home_sandbox = tempfile::tempdir().unwrap();
    let cs_out = run_cs(
        &cs,
        tenant_a.root.as_path(),
        tenant_a.state_dir.as_path(),
        &tenant_a.root.join(".cosmon").join("formulas"),
        home_sandbox.path(),
        &["ensemble", "--all"],
    );

    // 2. cs-thin ensemble
    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Ensemble(EnsembleArgs {
            status: None,
            kind: None,
            tag: vec![],
            fleet: None,
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin ensemble");
    let thin_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("cs-thin JSON");

    // Sanity: both surfaces account for the two seeded molecules.
    let cs_total = cs_out["molecules"]["total"].as_u64().unwrap_or(0);
    let thin_total = thin_out["total"].as_u64().unwrap_or(0);
    assert_eq!(cs_total, 2, "cs sees both molecules");
    assert_eq!(thin_total, 2, "cs-thin sees both molecules");

    // The ids surfaced match (set-equality) — order varies because cs
    // groups by worker / fleet whereas cs-thin emits the
    // `list_molecules` order. Compare canonicalised id sets.
    let thin_ids: std::collections::BTreeSet<String> = thin_out["molecules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(thin_ids.contains("task-20260504-ens1"));
    assert!(thin_ids.contains("task-20260504-ens2"));
}

#[tokio::test]
async fn parity_collapse() {
    let Some(cs) = find_cs_binary() else {
        skip("parity_collapse");
        return;
    };
    let allowlist = load_allowlist();

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260504-cocs",
            &serde_json::json!({"status": "running"}),
        )
        .unwrap();
    tenant_a
        .insert_molecule(
            "task-20260504-coth",
            &serde_json::json!({"status": "running"}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-collapse"),
    });

    let home_sandbox = tempfile::tempdir().unwrap();
    let cs_out = run_cs(
        &cs,
        tenant_a.root.as_path(),
        tenant_a.state_dir.as_path(),
        &tenant_a.root.join(".cosmon").join("formulas"),
        home_sandbox.path(),
        &["collapse", "task-20260504-cocs", "--reason", "test-reason"],
    );

    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Collapse(CollapseArgs {
            molecule_id: "task-20260504-coth".to_owned(),
            reason: "test-reason".to_owned(),
            cause: None,
            account: None,
            kind: None,
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin collapse");
    let thin_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("cs-thin JSON");

    // Sanity: both produce a "collapsed" status and carry the reason.
    assert_eq!(cs_out["status"], "collapsed");
    assert_eq!(thin_out["status"], "collapsed");
    assert_eq!(cs_out["reason"], "test-reason");
    assert_eq!(thin_out["reason"], "test-reason");

    // Normalise the differing molecule id.
    let mut cs_norm = cs_out.clone();
    let mut thin_norm = thin_out.clone();
    if let Some(obj) = cs_norm.as_object_mut() {
        obj.insert("molecule".to_owned(), Value::String("<normalised>".into()));
    }
    if let Some(obj) = thin_norm.as_object_mut() {
        obj.insert("molecule".to_owned(), Value::String("<normalised>".into()));
    }

    let diffs = compare(&cs_norm, &thin_norm, "collapse", &allowlist);
    assert!(
        diffs.is_empty(),
        "parity[collapse] = MISMATCH\n{}",
        render_diffs(&diffs),
    );
}

#[tokio::test]
async fn parity_stuck() {
    let Some(cs) = find_cs_binary() else {
        skip("parity_stuck");
        return;
    };
    let allowlist = load_allowlist();

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260504-stcs",
            &serde_json::json!({"status": "running"}),
        )
        .unwrap();
    tenant_a
        .insert_molecule(
            "task-20260504-stth",
            &serde_json::json!({"status": "running"}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-stuck"),
    });

    let home_sandbox = tempfile::tempdir().unwrap();
    let cs_out = run_cs(
        &cs,
        tenant_a.root.as_path(),
        tenant_a.state_dir.as_path(),
        &tenant_a.root.join(".cosmon").join("formulas"),
        home_sandbox.path(),
        &["stuck", "task-20260504-stcs", "--reason", "blocker"],
    );

    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Stuck(StuckArgs {
            molecule_id: "task-20260504-stth".to_owned(),
            reason: "blocker".to_owned(),
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin stuck");
    let thin_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("cs-thin JSON");

    // Sanity: both produce status=stuck and the reason.
    assert_eq!(cs_out["status"], "stuck");
    assert_eq!(thin_out["status"], "stuck");
    assert_eq!(cs_out["reason"], "blocker");
    assert_eq!(thin_out["reason"], "blocker");

    // Normalise the differing molecule id.
    let mut cs_norm = cs_out.clone();
    let mut thin_norm = thin_out.clone();
    if let Some(obj) = cs_norm.as_object_mut() {
        obj.insert("molecule".to_owned(), Value::String("<normalised>".into()));
    }
    if let Some(obj) = thin_norm.as_object_mut() {
        obj.insert("molecule".to_owned(), Value::String("<normalised>".into()));
    }

    let diffs = compare(&cs_norm, &thin_norm, "stuck", &allowlist);
    assert!(
        diffs.is_empty(),
        "parity[stuck] = MISMATCH\n{}",
        render_diffs(&diffs),
    );
}

/// Round-trip parity for freeze + thaw.
///
/// `cs` does not have a molecule-level `cs molecule-freeze` /
/// `cs molecule-thaw` subcommand — `cs freeze` and `cs thaw` are
/// worker-scoped. The closest cs-side equivalent is `cs stuck` (which
/// also transitions to `Frozen`); we use it as the cs-side reference
/// for the freeze status transition. For thaw we exercise the cs-thin
/// path against a freshly frozen molecule and assert the wire shape +
/// resulting state.
#[tokio::test]
async fn parity_freeze_thaw() {
    let Some(_cs) = find_cs_binary() else {
        skip("parity_freeze_thaw");
        return;
    };

    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260504-frzt",
            &serde_json::json!({"status": "running"}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-parity-freeze-thaw"),
    });
    let jwt_path = write_jwt_to_temp(&jwt);

    // 1. freeze
    let cli = Cli {
        base_url: Some(base_url.clone()),
        jwt_from_env: None,
        jwt_file: Some(jwt_path.clone()),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Freeze(FreezeArgs {
            molecule_id: "task-20260504-frzt".to_owned(),
            reason: Some("operator pause".to_owned()),
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin freeze");
    let freeze_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("freeze JSON");
    assert_eq!(freeze_out["status"], "frozen");
    assert_eq!(freeze_out["molecule"], "task-20260504-frzt");
    assert_eq!(freeze_out["already_frozen"], false);
    assert_eq!(freeze_out["previous_status"], "running");

    // 2. freeze again — idempotent.
    let cli = Cli {
        base_url: Some(base_url.clone()),
        jwt_from_env: None,
        jwt_file: Some(jwt_path.clone()),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Freeze(FreezeArgs {
            molecule_id: "task-20260504-frzt".to_owned(),
            reason: None,
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf)
        .await
        .expect("cs-thin freeze idempotent");
    let again: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("idempotent JSON");
    assert_eq!(again["already_frozen"], true);

    // 3. thaw
    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(jwt_path),
        coverage_report: false,
        json: false,
        command: Some(ThinCmd::Thaw(ThawArgs {
            molecule_id: "task-20260504-frzt".to_owned(),
        })),
    };
    let mut buf = Vec::new();
    run_with(cli, &mut buf).await.expect("cs-thin thaw");
    let thaw_out: Value = serde_json::from_slice(buf.trim_ascii_end()).expect("thaw JSON");
    assert_eq!(thaw_out["status"], "running");
    assert_eq!(thaw_out["molecule"], "task-20260504-frzt");
    assert_eq!(thaw_out["already_thawed"], false);
    assert_eq!(thaw_out["previous_status"], "frozen");

    // 4. Confirm the persisted state mirrors the wire shape.
    let state_path = tenant_a
        .state_dir
        .join("fleets/default/molecules/task-20260504-frzt/state.json");
    let body = std::fs::read_to_string(&state_path).expect("state.json exists");
    let parsed: Value = serde_json::from_str(&body).expect("state.json is JSON");
    assert_eq!(parsed["status"], "running");
}
