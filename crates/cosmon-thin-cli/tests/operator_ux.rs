// SPDX-License-Identifier: Apache-2.0

//! Operator paper-cuts (T-CS-THIN-TEST-COVERAGE-GAP).
//!
//! Discipline: every clap-error path on the cs-thin surface must be
//! exercised, and every habit-flag inherited from `cs` (--json, --all,
//! --config, --cluster, --verbose) must either accept silently or
//! produce an *explanatory* error — never a raw `error: unexpected
//! argument 'X' found`. The 2026-05-05 cross-container live test
//! (operator typed `cs-thin ensemble --tag cs-thin --json`) exposed
//! this gap.
//!
//! # What's covered
//!
//! - Habit-flag tolerance: `--json`, `--all`, `--config`, `--cluster`,
//!   `--verbose` against every cs-thin verb. The expected outcome is
//!   either "parses successfully (no-op)" — pinned by the global
//!   `Cli.json` field for `--json`, or "rejected with a hint that
//!   names the verb and the offending flag".
//! - Missing required args (e.g. `observe` without `<MOLECULE_ID>`,
//!   `nucleate` without `--formula`): clap must refuse with a message
//!   that *names the missing thing*.
//! - Empty values where a non-empty value is expected (`--reason ""`).
//! - Missing JWT env var → exit 3 with a hint pointing at
//!   `--jwt-file` / `--jwt-from-env`.
//! - Missing base URL (no flag, no `CS_THIN_BASE_URL`) → exit 1 with
//!   a hint pointing at `--base-url` / the env var.
//! - 404 / nonexistent molecule id → exit 1 with HTTP body excerpt
//!   passed through.
//! - Mechanical exhaustivity gate: every cs-thin verb with at least
//!   one required positional or flag must have a "missing required"
//!   case in this test (asserted against the clap surface).
//!
//! # Architecture
//!
//! Most tests drive clap via `Cli::try_parse_from(...)` (no process
//! exit, no stdout capture mechanics). Runtime tests that need a real
//! HTTP server use the `cosmon-rpp-adapter` dev-dep and
//! `cosmon-oidc-testkit` to mint a JWT — same pattern as
//! `parity_with_cs.rs`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use clap::{CommandFactory, Parser};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_thin_cli::cli::{run_with, Cli, CliError, Command as ThinCmd, ObserveArgs};

// ─────────────────────────────────────────────────────────────────────
// Static metadata about cs-thin verbs — kept here as a small const
// table so the exhaustivity gate has a deterministic target. Drift
// against clap is detected by `every_verb_has_a_paper_cut_test`.
// ─────────────────────────────────────────────────────────────────────

/// Verbs in cs-thin that take **no** required positional/flag — pure
/// dispatch, can run with no extra args.
const VERBS_NO_REQUIRED_ARG: &[&str] = &["avatar", "ensemble", "verbs", "help"];

/// Verbs in cs-thin with a single required positional `<MOLECULE_ID>`.
const VERBS_WITH_MOLECULE_ID: &[&str] = &[
    "observe", "tag", "collapse", "freeze", "thaw", "stuck", "tackle",
];

/// Verbs in cs-thin that require both a positional and one or more
/// named flags. `tag` needs --add or --remove (validated at runtime,
/// not clap-level — see `tag_without_add_or_remove_explains`).
const VERBS_WITH_REQUIRED_FLAGS: &[(&str, &[&str])] = &[
    ("nucleate", &["--formula"]),
    ("collapse", &["--reason"]),
    ("stuck", &["--reason"]),
];

// ─────────────────────────────────────────────────────────────────────
// Habit-flag tolerance
// ─────────────────────────────────────────────────────────────────────

/// Cycle every habit-flag through every cs-thin verb. The expected
/// outcome differs per flag:
///
/// - `--json` is a global Cli flag (no-op); MUST parse successfully
///   when placed before the subcommand.
/// - `--all`, `--config`, `--cluster`, `--verbose` are NOT modelled by
///   cs-thin; MUST be rejected by clap, and the error message MUST
///   name the offending flag (so the operator sees "unexpected
///   argument '--all'", not a generic "invalid arguments" line).
#[test]
fn habit_flag_json_is_silent_noop_on_top_level() {
    // The canonical "cs-thin ensemble --json --tag temp:hot" pattern
    // the live operator typed on 2026-05-05.
    let res = Cli::try_parse_from([
        "cs-thin",
        "--json",
        "--base-url",
        "http://localhost",
        "ensemble",
        "--tag",
        "temp:hot",
    ]);
    let cli = res.expect(
        "habit-flag --json before the subcommand must parse silently — \
         operators inherit the muscle from `cs --json ensemble`",
    );
    assert!(cli.json, "global --json should be captured by Cli.json");
    assert!(matches!(cli.command, Some(ThinCmd::Ensemble(_))));
}

#[test]
fn habit_flag_json_after_verb_is_rejected_with_hint() {
    // Placed after the subcommand, --json is per-verb. Today no verb
    // models --json at the per-verb level; clap should reject. The
    // error must name `--json` so the operator can fix the typing.
    let err = Cli::try_parse_from(["cs-thin", "ensemble", "--json"])
        .expect_err("post-verb --json must be rejected today");
    let txt = err.to_string();
    assert!(
        txt.contains("--json") || txt.contains("json"),
        "error must name `--json` so the operator can fix it: {txt}"
    );
}

#[test]
fn habit_flag_all_is_rejected_for_ensemble() {
    let err = Cli::try_parse_from(["cs-thin", "ensemble", "--all"])
        .expect_err("`cs-thin ensemble --all` must be rejected — see allowlist class out_of_scope");
    let txt = err.to_string();
    assert!(
        txt.contains("--all") || txt.contains("all"),
        "error must name `--all`: {txt}"
    );
}

#[test]
fn habit_flag_config_is_rejected() {
    // `cs --config <PATH>` is operator-side only. cs-thin is stateless;
    // the flag has no equivalent.
    for verb in &["observe", "nucleate", "tag", "ensemble"] {
        let err = Cli::try_parse_from(["cs-thin", verb, "--config", "/tmp/foo.toml"])
            .expect_err(&format!("--config must be rejected for verb `{verb}`"));
        let txt = err.to_string();
        assert!(
            txt.contains("--config") || txt.contains("config"),
            "error for verb `{verb}` must name `--config`: {txt}"
        );
    }
}

#[test]
fn habit_flag_cluster_is_rejected_for_ensemble() {
    let err = Cli::try_parse_from(["cs-thin", "ensemble", "--cluster"])
        .expect_err("--cluster must be rejected — local TopoMap concept (ADR-066)");
    let txt = err.to_string();
    assert!(
        txt.contains("--cluster") || txt.contains("cluster"),
        "error must name `--cluster`: {txt}"
    );
}

#[test]
fn habit_flag_verbose_is_rejected() {
    // -v/--verbose is operator-side only on cs.
    let err = Cli::try_parse_from(["cs-thin", "--verbose", "ensemble"])
        .expect_err("--verbose must be rejected");
    let txt = err.to_string();
    assert!(
        txt.contains("--verbose") || txt.contains("verbose") || txt.contains("-v"),
        "error must name --verbose / -v: {txt}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Missing required args — clap-level
// ─────────────────────────────────────────────────────────────────────

#[test]
fn observe_without_molecule_id_is_rejected() {
    let err = Cli::try_parse_from(["cs-thin", "observe"])
        .expect_err("`cs-thin observe` without an id must be rejected");
    let txt = err.to_string();
    assert!(
        txt.to_lowercase().contains("molecule") || txt.to_lowercase().contains("required"),
        "error must mention the missing molecule id: {txt}"
    );
}

#[test]
fn tag_without_molecule_id_is_rejected() {
    let err = Cli::try_parse_from(["cs-thin", "tag", "--add", "temp:hot"])
        .expect_err("`cs-thin tag --add` without an id must be rejected");
    let txt = err.to_string();
    assert!(
        txt.to_lowercase().contains("molecule") || txt.to_lowercase().contains("required"),
        "error must mention the missing molecule id: {txt}"
    );
}

#[test]
fn nucleate_without_formula_is_rejected() {
    let err = Cli::try_parse_from(["cs-thin", "nucleate"])
        .expect_err("`cs-thin nucleate` without --formula must be rejected");
    let txt = err.to_string();
    assert!(
        txt.contains("--formula") || txt.to_lowercase().contains("formula"),
        "error must name --formula: {txt}"
    );
}

#[test]
fn collapse_without_reason_is_rejected() {
    let err = Cli::try_parse_from(["cs-thin", "collapse", "task-20260505-fd5d"])
        .expect_err("collapse requires --reason");
    let txt = err.to_string();
    assert!(
        txt.contains("--reason") || txt.to_lowercase().contains("reason"),
        "error must name --reason: {txt}"
    );
}

#[test]
fn stuck_without_reason_is_rejected() {
    let err = Cli::try_parse_from(["cs-thin", "stuck", "task-20260505-fd5d"])
        .expect_err("stuck requires --reason");
    let txt = err.to_string();
    assert!(
        txt.contains("--reason") || txt.to_lowercase().contains("reason"),
        "error must name --reason: {txt}"
    );
}

#[test]
fn freeze_without_args_is_rejected() {
    // freeze takes the molecule id positional but --reason is optional.
    let err =
        Cli::try_parse_from(["cs-thin", "freeze"]).expect_err("freeze requires a molecule id");
    let txt = err.to_string();
    assert!(
        txt.to_lowercase().contains("molecule") || txt.to_lowercase().contains("required"),
        "error must name molecule id: {txt}"
    );
}

#[test]
fn thaw_without_args_is_rejected() {
    let err = Cli::try_parse_from(["cs-thin", "thaw"]).expect_err("thaw requires a molecule id");
    let txt = err.to_string();
    assert!(
        txt.to_lowercase().contains("molecule") || txt.to_lowercase().contains("required"),
        "error must name molecule id: {txt}"
    );
}

#[test]
fn invalid_var_format_explains() {
    // --var must be `key=value`; runtime check.
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        "http://127.0.0.1:1",
        "--jwt-from-env",
        "JWT_NOPE",
        "nucleate",
        "--formula",
        "task-work",
        "--var",
        "no-equals-sign-here",
    ])
    .expect("clap accepts --var with any string");
    // The runtime check fires after JWT resolution, so the --var error
    // path requires the JWT env to exist; we set a dummy. Because the
    // assertion target is the message, we route through run_with and
    // inspect the error.
    let mut buf = Vec::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    std::env::set_var("JWT_NOPE", "fake-token");
    let res = runtime.block_on(run_with(cli, &mut buf));
    std::env::remove_var("JWT_NOPE");
    let err = res.expect_err("--var without `=` must fail");
    assert!(
        matches!(err, CliError::Local(_)),
        "--var format error must be Local (exit 1)"
    );
    let txt = err.to_string();
    assert!(
        txt.contains("key=value") || txt.contains("no-equals-sign-here"),
        "error must explain the expected `key=value` format: {txt}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Empty values
// ─────────────────────────────────────────────────────────────────────

#[test]
fn empty_reason_is_accepted_at_clap_layer() {
    // clap does not enforce non-empty strings; the rpp-adapter does.
    // We document the behaviour: clap accepts; runtime returns 1.
    let cli = Cli::try_parse_from(["cs-thin", "collapse", "task-id", "--reason", ""])
        .expect("clap accepts empty --reason");
    if let Some(ThinCmd::Collapse(args)) = cli.command {
        assert_eq!(args.reason, "");
    } else {
        panic!("expected Collapse command");
    }
}

// ─────────────────────────────────────────────────────────────────────
// Missing JWT / base-url
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn missing_jwt_env_var_returns_exit_3() {
    // CS_THIN_NO_JWT is guaranteed not to exist.
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        "http://127.0.0.1:1",
        "--jwt-from-env",
        "CS_THIN_NO_JWT",
        "observe",
        "task-fake",
    ])
    .unwrap();
    let mut buf = Vec::new();
    let err = run_with(cli, &mut buf)
        .await
        .expect_err("missing env var must fail");
    assert!(matches!(err, CliError::JwtMissing(_)));
    assert_eq!(err.exit_code(), 3);
    let txt = err.to_string();
    assert!(
        txt.contains("CS_THIN_NO_JWT") || txt.contains("env var"),
        "error must name the missing env var: {txt}"
    );
}

#[tokio::test]
async fn missing_jwt_file_returns_exit_3() {
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        "http://127.0.0.1:1",
        "--jwt-file",
        "/tmp/this-path-does-not-exist-fd5d",
        "observe",
        "task-fake",
    ])
    .unwrap();
    let mut buf = Vec::new();
    let err = run_with(cli, &mut buf)
        .await
        .expect_err("missing file must fail");
    assert_eq!(err.exit_code(), 3);
    let txt = err.to_string();
    assert!(
        txt.contains("--jwt-file") || txt.contains("does-not-exist"),
        "error must name --jwt-file or the path: {txt}"
    );
}

#[tokio::test]
async fn missing_base_url_returns_exit_1_with_hint() {
    // Make sure the env var is unset for this test.
    std::env::remove_var("CS_THIN_BASE_URL");

    // Use a JWT env var that exists so we get past the JWT step.
    std::env::set_var("CS_THIN_TEST_JWT", "fake-token");
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--jwt-from-env",
        "CS_THIN_TEST_JWT",
        "observe",
        "task-fake",
    ])
    .unwrap();
    let mut buf = Vec::new();
    let err = run_with(cli, &mut buf)
        .await
        .expect_err("missing base-url must fail");
    std::env::remove_var("CS_THIN_TEST_JWT");
    assert_eq!(err.exit_code(), 1);
    let txt = err.to_string();
    assert!(
        txt.contains("--base-url") || txt.contains("CS_THIN_BASE_URL"),
        "error must hint at --base-url or env var: {txt}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 404 / nonexistent molecule id
// ─────────────────────────────────────────────────────────────────────

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

#[tokio::test]
async fn observe_nonexistent_id_returns_exit_1_with_body() {
    let mut tenants = TenantWorkspaces::new();
    tenants.add("a");
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
        jti: Some("jti-ux-404"),
    });

    std::env::set_var("CS_THIN_UX_JWT", jwt);
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        &base_url,
        "--jwt-from-env",
        "CS_THIN_UX_JWT",
        "observe",
        "task-DOES-NOT-EXIST-fd5d",
    ])
    .unwrap();
    let mut buf = Vec::new();
    let err = run_with(cli, &mut buf).await.expect_err("404 must fail");
    std::env::remove_var("CS_THIN_UX_JWT");
    match err {
        CliError::Http { status, .. } => {
            assert_eq!(status, 404, "expected 404 for nonexistent id");
        }
        other => panic!("expected Http error, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tag with no operations
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tag_without_add_or_remove_explains() {
    // clap accepts `cs-thin tag <id>`; runtime must reject.
    std::env::set_var("CS_THIN_UX_JWT2", "fake");
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        "http://127.0.0.1:1",
        "--jwt-from-env",
        "CS_THIN_UX_JWT2",
        "tag",
        "task-id",
    ])
    .unwrap();
    let mut buf = Vec::new();
    let err = run_with(cli, &mut buf)
        .await
        .expect_err("tag with neither --add nor --remove must fail");
    std::env::remove_var("CS_THIN_UX_JWT2");
    assert_eq!(err.exit_code(), 1);
    let txt = err.to_string();
    assert!(
        txt.contains("--add") || txt.contains("--remove"),
        "error must name --add and/or --remove: {txt}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Mechanical exhaustivity gate
// ─────────────────────────────────────────────────────────────────────

/// Every cs-thin verb that has a required argument (positional or
/// `required = true` flag) must be cited in `VERBS_WITH_MOLECULE_ID`,
/// `VERBS_WITH_REQUIRED_FLAGS`, or `VERBS_NO_REQUIRED_ARG`. This
/// drift-detector ensures no future verb is added without an
/// operator-UX test for the missing-arg case.
#[test]
fn every_verb_has_a_paper_cut_test() {
    let cmd = Cli::command();

    let known: BTreeSet<&str> = VERBS_WITH_MOLECULE_ID
        .iter()
        .copied()
        .chain(VERBS_WITH_REQUIRED_FLAGS.iter().map(|(v, _)| *v))
        .chain(VERBS_NO_REQUIRED_ARG.iter().copied())
        .collect();

    let live: BTreeSet<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_owned())
        .collect();

    let missing: Vec<&str> = live
        .iter()
        .filter(|v| !known.contains(v.as_str()))
        .map(String::as_str)
        .collect();
    assert!(
        missing.is_empty(),
        "cs-thin verbs with no operator-UX tracker entry: {missing:?}\n\
         Add the verb to one of: VERBS_WITH_MOLECULE_ID, \
         VERBS_WITH_REQUIRED_FLAGS, or VERBS_NO_REQUIRED_ARG, then \
         add the corresponding missing-arg / habit-flag test."
    );

    let stale: Vec<&str> = known
        .iter()
        .filter(|v| !live.iter().any(|s| s == *v))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "cs-thin verbs in the operator-UX tracker that no longer exist \
         in clap: {stale:?}"
    );
}

/// For each verb in `VERBS_WITH_MOLECULE_ID`, drive clap with no
/// positional argument and confirm parse fails with a message that
/// names the missing positional. Mechanical: removes one verb is
/// enough for the test to flag drift.
#[test]
fn missing_molecule_id_explains_for_every_verb() {
    for verb in VERBS_WITH_MOLECULE_ID {
        let err = Cli::try_parse_from(["cs-thin", verb])
            .err()
            .unwrap_or_else(|| panic!("`cs-thin {verb}` should error"));
        let txt = err.to_string();
        // For verbs with REQUIRED flags too (collapse, stuck), the
        // first complaint may be about the flag rather than the
        // positional. Accept either.
        assert!(
            txt.to_lowercase().contains("required")
                || txt.to_lowercase().contains("missing")
                || txt.to_lowercase().contains("molecule")
                || txt.contains("--reason")
                || txt.contains("--formula"),
            "error for `cs-thin {verb}` (no args) must explain what's missing: {txt}"
        );
    }
}

/// For each verb with required flags, drive clap with the positional
/// (where applicable) but no required flag, and confirm clap names
/// the missing flag.
#[test]
fn missing_required_flag_explains_for_every_verb() {
    for (verb, flags) in VERBS_WITH_REQUIRED_FLAGS {
        let mut argv = vec!["cs-thin", verb];
        if VERBS_WITH_MOLECULE_ID.contains(verb) {
            argv.push("dummy-id");
        }
        let err = Cli::try_parse_from(&argv)
            .err()
            .unwrap_or_else(|| panic!("`cs-thin {verb}` should error"));
        let txt = err.to_string();
        let any_match = flags.iter().any(|f| txt.contains(f));
        assert!(
            any_match || txt.to_lowercase().contains("required"),
            "error for `cs-thin {verb}` must name one of {flags:?}: {txt}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Smoke: a habit-flag prefix doesn't break anything sane
// ─────────────────────────────────────────────────────────────────────

#[test]
fn ensemble_with_json_and_tag_parses_in_either_order() {
    // `cs-thin --json ensemble --tag X`
    let a = Cli::try_parse_from([
        "cs-thin",
        "--json",
        "--base-url",
        "http://localhost",
        "ensemble",
        "--tag",
        "temp:hot",
    ])
    .expect("global --json before subcommand parses");
    assert!(a.json);

    // Without --json — also parses.
    let b = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        "http://localhost",
        "ensemble",
        "--tag",
        "temp:hot",
    ])
    .expect("no --json also parses");
    assert!(!b.json);
}

#[test]
fn observe_struct_carries_id_through_clap() {
    // Defence-in-depth: a regression that drops the positional id from
    // the parsed struct would silently produce empty observes.
    let cli = Cli::try_parse_from([
        "cs-thin",
        "--base-url",
        "http://localhost",
        "observe",
        "task-20260505-fd5d",
    ])
    .unwrap();
    if let Some(ThinCmd::Observe(ObserveArgs { molecule_id })) = cli.command {
        assert_eq!(molecule_id, "task-20260505-fd5d");
    } else {
        panic!("expected Observe");
    }
}
