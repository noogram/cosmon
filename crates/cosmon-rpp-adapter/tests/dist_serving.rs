// SPDX-License-Identifier: AGPL-3.0-only

//! Acceptance test for the cosmon-remote Phase 0 serving endpoints
//! (`GET /install.sh` + `GET /dist/justfile`).
//!
//! Both sit outside `/v1/` (operational, like `/healthz` and `/`), are
//! unauthenticated, and template their `__COSMON_HOST__` placeholder
//! with the request's base URL so the served artefacts point back at
//! the host the tenant fetched them from. Adding them must NOT trip the
//! §8p surface freeze (see `api_surface_freeze.rs`).

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, OidcMock, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use tower::ServiceExt;

async fn make_state(security_dir: &std::path::Path) -> AppState {
    make_state_with_dist_root(security_dir, std::path::PathBuf::from("/tmp/cosmon-dist")).await
}

async fn make_state_with_dist_root(
    security_dir: &std::path::Path,
    dist_root: std::path::PathBuf,
) -> AppState {
    let oidc = OidcMock::start().await;
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));
    let tenants = TenantWorkspaces::new();
    AppState {
        cs_path: fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(
            HabilitationMap::builder().build(),
        ),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(5),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(dist_root)),
        install_templating: std::sync::Arc::new(
            cosmon_rpp_adapter::config::InstallTemplating::default(),
        ),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
    }
}

async fn get(uri: &str, host: &str, forwarded_proto: Option<&str>) -> (StatusCode, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(make_state(tmp.path()).await);
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("Host", host);
    if let Some(proto) = forwarded_proto {
        builder = builder.header("X-Forwarded-Proto", proto);
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = String::from_utf8(
        to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, content_type, body)
}

#[tokio::test]
async fn install_sh_served_unauthenticated_with_loopback_http() {
    // No Authorization header — operational endpoint, never JWT-gated.
    let (status, content_type, body) = get("/install.sh", "127.0.0.1:8443", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/x-shellscript"),
        "content-type was {content_type:?}"
    );
    assert!(body.starts_with("#!/bin/sh"), "must be a shell script");
    // Loopback host with no X-Forwarded-Proto → http scheme.
    assert!(
        body.contains("http://127.0.0.1:8443"),
        "host must be templated as loopback http"
    );
    assert!(
        !body.contains("__COSMON_HOST__"),
        "placeholder must be fully substituted"
    );
}

#[tokio::test]
async fn install_sh_uses_https_for_remote_host_and_forwarded_proto() {
    let (status, _ct, body) = get("/install.sh", "cosmon.example.ts.net", Some("https")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("https://cosmon.example.ts.net"),
        "X-Forwarded-Proto https must win"
    );
    assert!(!body.contains("__COSMON_HOST__"));
}

/// Regression for the Dave onboarding finding (2026-06-05): a tenant
/// deployment served in **clear HTTP** behind a non-TLS proxy (AWS Tenant-Demo
/// VM, a local VM) sends a request with a non-loopback `Host` and **no**
/// `X-Forwarded-Proto`. The adapter listens in plaintext, so the honest
/// scheme is `http`. The earlier "non-loopback ⇒ https" heuristic templated
/// `https://<host>/dist/binary/...`, so `curl install.sh | sh` fetched the
/// binary over HTTPS against a plaintext port and died with
/// `curl: (35) SSL wrong version number`. The tenant must NOT have to pass
/// `COSMON_HOST=http://…` by hand.
#[tokio::test]
async fn install_sh_defaults_to_http_for_remote_host_without_forwarded_proto() {
    let (status, _ct, body) = get("/install.sh", "tenant.tenant-demo.internal:8443", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("http://tenant.tenant-demo.internal:8443"),
        "no X-Forwarded-Proto + plaintext listener → http, not https"
    );
    assert!(
        !body.contains("https://tenant.tenant-demo.internal"),
        "must NOT guess https for a clear-HTTP tenant deployment \
         (the SSL-wrong-version-number bug)"
    );
    assert!(!body.contains("__COSMON_HOST__"));
}

/// A reverse proxy that terminates nothing (pure HTTP pass-through) may
/// still advertise the scheme explicitly as `http`. The adapter must honour
/// it rather than override it.
#[tokio::test]
async fn install_sh_honours_explicit_forwarded_proto_http() {
    let (status, _ct, body) = get("/install.sh", "tenant.tenant-demo.internal", Some("http")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("http://tenant.tenant-demo.internal"),
        "explicit X-Forwarded-Proto: http must win"
    );
    assert!(!body.contains("https://tenant.tenant-demo.internal"));
}

/// A chained proxy hop sends a comma-separated `X-Forwarded-Proto` list
/// (`"https, http"`); the first token is the original client-facing scheme.
#[tokio::test]
async fn install_sh_takes_first_token_of_chained_forwarded_proto() {
    let (status, _ct, body) =
        get("/install.sh", "cosmon.example.ts.net", Some("https, http")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("https://cosmon.example.ts.net"),
        "first token of a chained X-Forwarded-Proto list wins"
    );
    assert!(!body.contains("__COSMON_HOST__"));
}

#[tokio::test]
async fn dist_justfile_served_with_templated_adapter_url() {
    // TLS-terminated public deployment: the reverse proxy advertises
    // `X-Forwarded-Proto: https`, so the templated host is https. (Absent
    // that header the adapter templates http — see
    // `install_*_defaults_to_http_*` below.)
    let (status, content_type, body) =
        get("/dist/justfile", "cosmon.example.ts.net", Some("https")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/plain"),
        "content-type was {content_type:?}"
    );
    // The serving host is pinned to the justfile as the fallback when no
    // profile is set (it appears in the `_cfg` host/oidc default expansions).
    assert!(
        body.contains("https://cosmon.example.ts.net"),
        "serving host must be pinned into the justfile as the fallback"
    );
    assert!(
        !body.contains("__COSMON_HOST__"),
        "placeholder must be templated away"
    );
    // task-20260525-a476: the justfile now sources host/sub/aud/oidc_url
    // from the SAME profile the cosmon-remote binary reads.
    assert!(
        body.contains("cosmon-remote/profiles"),
        "justfile must read the shared cosmon-remote profile (config unification)"
    );
    // JWT cache unified off /tmp into the cosmon-remote cache dir.
    assert!(
        body.contains("cosmon-remote") && body.contains(".cache"),
        "JWT cache must live under the cosmon-remote cache dir, not /tmp"
    );
    // The two tenant surfaces must be present.
    assert!(body.contains("mol-list"), "molecule recipes missing");
    assert!(body.contains("auth-claude-start"), "auth recipes missing");
}

/// Regression: the original `case "$COSMON_HOST" in __COSMON_HOST__) die`
/// safety check was templating-fragile — `String::replace` substitutes ALL
/// occurrences of the placeholder, including the case pattern. After
/// templating, `COSMON_HOST` equalled the case pattern verbatim and
/// `curl install.sh | sh` died on the first run unless the operator set
/// `COSMON_HOST` to a different string. Discovered by live operator test
/// on 2026-05-22.
///
/// This test runs the templated script end-to-end under `sh` with `curl`
/// and `just` stubbed out, `COSMON_HOST` unset, and asserts the script exits
/// 0 — i.e. no die fires before it reaches the network step.
#[tokio::test]
async fn install_sh_runs_end_to_end_without_override() {
    use std::os::unix::fs::PermissionsExt;

    let (status, _ct, body) = get("/install.sh", "cosmon.example.ts.net", Some("https")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("https://cosmon.example.ts.net"),
        "host must be templated"
    );

    let workdir = tempfile::tempdir().unwrap();
    let script = workdir.path().join("install.sh");
    std::fs::write(&script, &body).unwrap();

    // Stub `curl`: parse `-o <target>` and write a minimal valid shell
    // script (with shebang) so that after chmod +x the downloaded
    // "binary" can still be executed by Phase 1's `config init` call.
    let stub_bin = workdir.path().join("stub_bin");
    std::fs::create_dir(&stub_bin).unwrap();
    let curl_stub = stub_bin.join("curl");
    std::fs::write(
        &curl_stub,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
           case \"$1\" in\n\
             -o) shift; printf '#!/bin/sh\\nexit 0\\n' > \"$1\"; shift ;;\n\
             *) shift ;;\n\
           esac\n\
         done\n\
         exit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&curl_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = std::process::Command::new("sh")
        .arg(script.to_str().unwrap())
        .env("PATH", format!("{}:/bin:/usr/bin", stub_bin.display()))
        .env(
            "COSMON_BIN_DIR",
            workdir.path().join("local-bin").display().to_string(),
        )
        // Hermetic HOME: the script's no-crutch PATH step appends an
        // export line to `$HOME/.{zshrc,bashrc,profile}` when BIN_DIR
        // is not on the PATH — which is exactly the case here. Pin
        // HOME to the workdir so the test never edits the developer's
        // real shell rc (it did, once).
        .env("HOME", workdir.path())
        // Do NOT set COSMON_HOST — this is the path that died under the
        // Phase 0 bug; Phase 1 must keep it working.
        .env_remove("COSMON_HOST")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "install.sh exited {:?} without COSMON_HOST override\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        workdir
            .path()
            .join("local-bin")
            .join("cosmon-remote")
            .exists(),
        "cosmon-remote was not installed to COSMON_BIN_DIR"
    );
    // No-crutch PATH (smithy C1): BIN_DIR was deliberately not on
    // the PATH, so the script must have configured the shell itself —
    // one export line in the hermetic HOME's rc — instead of printing
    // an instruction for the user to do it.
    let configured = [".zshrc", ".bashrc", ".profile"].iter().any(|rc| {
        std::fs::read_to_string(workdir.path().join(rc))
            .map(|s| s.contains("local-bin"))
            .unwrap_or(false)
    });
    assert!(
        configured,
        "install.sh must write the PATH export line into a shell rc itself"
    );
}

#[tokio::test]
async fn serving_endpoints_do_not_break_v1_freeze() {
    // The §8p surface freeze counts only `/v1/...`; operational routes
    // (`/install.sh`, `/dist/...`, `/healthz`, `/metrics`, …) must not
    // leak into the freeze. This guard documents that contract at the
    // test layer alongside `api_surface_freeze.rs`. The size of the
    // freeze is no longer asserted here — it derives from the
    // append-only event log at `data/surface_events.txt`
    // (vg-20260523-a682, Phase 1 / Commit 3 — wheeler
    // I-ADDITIVE-COUNTERS; ADR-110 §I3).
    let surface = cosmon_rpp_adapter::frozen_api_surface();
    assert!(
        !surface.is_empty(),
        "surface must not be empty — at least the V0 read-only base lives in the event log",
    );
    assert!(
        surface.iter().all(|r| r.contains("/v1/")),
        "every frozen route is under /v1/",
    );
}

// ---------------------------------------------------------------------
// Phase 1 dist multi-OS — `GET /dist/binary/{platform}/cosmon-remote`
// (task-20260522-aad5)
// ---------------------------------------------------------------------

/// Helper: build an app whose dist root is seeded with fake binary bytes
/// for the requested platforms. Returns (`security_tmp`, `dist_tmp`, app).
async fn app_with_seeded_dist(
    platforms: &[&str],
) -> (tempfile::TempDir, tempfile::TempDir, axum::Router) {
    let security_dir = tempfile::tempdir().unwrap();
    let dist_root = tempfile::tempdir().unwrap();
    for p in platforms {
        let dir = dist_root.path().join(p);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cosmon-remote"),
            format!("FAKE-{p}-BYTES").as_bytes(),
        )
        .unwrap();
    }
    let state =
        make_state_with_dist_root(security_dir.path(), dist_root.path().to_path_buf()).await;
    let app = router(state);
    (security_dir, dist_root, app)
}

async fn get_bytes(app: axum::Router, uri: &str) -> (StatusCode, String, String, Vec<u8>) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("Host", "cosmon.example.ts.net")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let content_disposition = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, content_type, content_disposition, bytes)
}

#[tokio::test]
async fn dist_binary_serves_seeded_bytes_for_each_platform() {
    let (_sec, _dist, app) =
        app_with_seeded_dist(&["macos-arm64", "macos-amd64", "linux-arm64", "linux-amd64"]).await;

    for platform in ["macos-arm64", "macos-amd64", "linux-arm64", "linux-amd64"] {
        let uri = format!("/dist/binary/{platform}/cosmon-remote");
        let (status, ct, cd, body) = get_bytes(app.clone(), &uri).await;
        assert_eq!(status, StatusCode::OK, "{platform} → {status}");
        assert_eq!(ct, "application/octet-stream");
        assert_eq!(cd, "attachment; filename=\"cosmon-remote\"");
        assert_eq!(body, format!("FAKE-{platform}-BYTES").into_bytes());
    }
}

#[tokio::test]
async fn dist_binary_404_for_unknown_platform() {
    let (_sec, _dist, app) = app_with_seeded_dist(&["macos-arm64"]).await;
    let (status, _ct, _cd, body) = get_bytes(app, "/dist/binary/freebsd-arm64/cosmon-remote").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let msg = String::from_utf8(body).unwrap();
    assert!(
        msg.contains("unknown platform"),
        "body should explain allow-list; got {msg:?}"
    );
}

#[tokio::test]
async fn dist_binary_404_when_image_missing_file() {
    // Known platform but on-disk file absent — should 404 with a
    // hint pointing at `just dist-binaries`, not a 500.
    let (_sec, _dist, app) = app_with_seeded_dist(&["macos-arm64"]).await;
    let (status, _ct, _cd, body) = get_bytes(app, "/dist/binary/linux-amd64/cosmon-remote").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let msg = String::from_utf8(body).unwrap();
    assert!(
        msg.contains("dist-binaries"),
        "404 body should hint at the build recipe; got {msg:?}"
    );
}

// ---------------------------------------------------------------------
// `task-20260525-a476` (Pierre v1.5 retour) — guard the canonical dist
// root. The production image (smithy `Dockerfile.cosmon-server`) and
// the cosmon-rpp-adapter Dockerfile both COPY the binaries to
// `/opt/cosmon-remote/dist`. The default MUST match that path so a
// deployment without an `rpp.toml` `dist_root` override still serves the
// binaries instead of 404'ing `curl install.sh | sh`. These tests pin
// the alignment so it cannot silently drift again.
// ---------------------------------------------------------------------

#[test]
fn default_dist_root_is_canonical_opt_path() {
    assert_eq!(
        cosmon_rpp_adapter::routes::dist::DEFAULT_DIST_ROOT,
        "/opt/cosmon-remote/dist",
        "DEFAULT_DIST_ROOT must match the Dockerfile COPY path \
         (/opt/cosmon-remote/dist) — see task-20260525-a476. If you \
         change the COPY destination in BOTH Dockerfiles, update this \
         constant in lock-step.",
    );
}

#[test]
fn default_config_resolves_dist_root_to_copy_path() {
    // A deployment that never sets `dist_root` in rpp.toml (the failure
    // mode Pierre hit) must still resolve to where the image COPYs the
    // binaries — not to an empty /usr/local/share path that 404s.
    let cfg = cosmon_rpp_adapter::config::RppConfig::default();
    let resolved = cfg.resolved_dist_root();
    assert_eq!(
        resolved,
        std::path::PathBuf::from("/opt/cosmon-remote/dist"),
        "no-override config must resolve to the canonical COPY path",
    );
    // And the per-platform layout the route reads lands under it.
    let dist = cosmon_rpp_adapter::routes::dist::DistState::new(resolved);
    assert_eq!(
        dist.binary_path("linux-arm64").unwrap(),
        std::path::PathBuf::from("/opt/cosmon-remote/dist/linux-arm64/cosmon-remote"),
    );
}

#[tokio::test]
async fn install_sh_emits_config_set_block_for_configured_deployment() {
    // When the operator wires the four-tuple in `install_templating`,
    // the served install.sh ships a `cosmon-remote config set` line
    // for each non-empty field. This resolves the AWS live-deploy finding
    // (`COSMON_HOST seulement, sub/aud/oidc-url devinés par
    // templating brittle`).
    let security_dir = tempfile::tempdir().unwrap();
    let dist_root = tempfile::tempdir().unwrap();
    let mut state =
        make_state_with_dist_root(security_dir.path(), dist_root.path().to_path_buf()).await;
    state.install_templating = std::sync::Arc::new(cosmon_rpp_adapter::config::InstallTemplating {
        sub: "tenant-demo-operator".into(),
        aud: "cosmon-rpp-tenant".into(),
        // Use the host placeholder so the per-deployment OIDC URL
        // rebinds to whichever host the request landed on.
        oidc_url: "__COSMON_HOST__/oidc".into(),
        noyau: "tenant-demo".into(),
    });
    let app = router(state);
    // TLS-terminated public deployment: proxy sets X-Forwarded-Proto: https,
    // so the templated oidc-url is https.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install.sh")
                .header("Host", "tenant-demo.tailnet0.ts.net")
                .header("X-Forwarded-Proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("config set sub 'tenant-demo-operator'"));
    assert!(body.contains("config set aud 'cosmon-rpp-tenant'"));
    assert!(body.contains("config set oidc-url 'https://tenant-demo.tailnet0.ts.net/oidc'"));
    assert!(body.contains("config set noyau 'tenant-demo'"));
    // No leftover placeholder in any path.
    assert!(!body.contains("__COSMON_HOST__"));
    assert!(!body.contains("__COSMON_CONFIG_SET_BLOCK__"));
}

#[tokio::test]
async fn install_sh_emits_oidc_url_by_default_when_otherwise_unconfigured() {
    // Pierre hardening P3 (`task-20260605-e26a`): `oidc_url` now defaults
    // to `__COSMON_HOST__/oidc` ([`InstallTemplating::default`]), so even
    // a deployment that configures nothing server-side persists a working
    // `oidc-url` in the tenant profile — pointed at the serving host's own
    // OIDC surface. The other three fields (sub/aud/noyau) stay opt-in and
    // are still omitted when unset. The old "no per-deployment fields"
    // fallback comment therefore no longer appears (the block is non-empty).
    let (status, _ct, body) = get("/install.sh", "cosmon.example.ts.net", Some("https")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("config set sub"),
        "no config-set line when sub unset"
    );
    assert!(
        !body.contains("config set aud"),
        "no config-set line when aud unset"
    );
    assert!(
        !body.contains("config set noyau"),
        "no config-set line when noyau unset"
    );
    assert!(
        body.contains("config set oidc-url 'https://cosmon.example.ts.net/oidc'"),
        "oidc-url must be templated to the serving host by default"
    );
    assert!(
        !body.contains("# (no per-deployment fields configured server-side"),
        "the fallback comment is gone — the block carries the default oidc-url"
    );
    assert!(!body.contains("__COSMON_CONFIG_SET_BLOCK__"));
}

/// v1.3.1 regression: install.sh must (a) carry zero `__COSMON_*__`
/// placeholder tokens after serving, and (b) syntax-validate under
/// `sh -n`, on both http (loopback) and https (remote) topologies,
/// and on both empty and fully-configured `install_templating`.
///
/// v1.3 smoke surfaced two defects on the local http loopback that
/// neither existing test exercised:
///
///   1. `curl --proto '=https' --tlsv1.2` flatly rejected the http
///      URL the script then issued — every install over loopback or
///      Tailscale-internal http endpoint died with
///      `curl: (1) Protocol "http" disabled`.
///   2. The literal token `__COSMON_CONFIG_SET_BLOCK__` appeared inside
///      a `#`-prefixed *comment* in the template. When the placeholder
///      expanded to a multi-line block, only the line that carried the
///      placeholder remained commented; the rest became executable
///      shell that fired `config: command not found` and
///      `→: command not found` before the script reached its real
///      work.
///
/// This test pins both invariants at the script-text level so neither
/// defect can re-enter the template without a failing red test.
async fn fetch_install_sh_with_state(
    state: AppState,
    host: &str,
    forwarded_proto: Option<&str>,
) -> String {
    let app = router(state);
    let mut builder = Request::builder()
        .method("GET")
        .uri("/install.sh")
        .header("Host", host);
    if let Some(p) = forwarded_proto {
        builder = builder.header("X-Forwarded-Proto", p);
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    String::from_utf8(
        to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn assert_no_placeholder_leakage(body: &str, host: &str, label: &str) {
    for placeholder in ["__COSMON_HOST__", "__COSMON_CONFIG_SET_BLOCK__"] {
        assert!(
            !body.contains(placeholder),
            "({host}, {label}) placeholder {placeholder} leaked into served script"
        );
    }
    // Catch-all: any token shaped like `__COSMON_<…>__` is suspect.
    // Hand-rolled scan keeps the test free of a regex dependency.
    let mut cursor = 0;
    while let Some(start) = body[cursor..].find("__COSMON_") {
        let abs = cursor + start;
        let tail = &body[abs..];
        if let Some(end) = tail[2..].find("__") {
            let tok = &tail[..end + 4];
            assert!(
                tok.contains(char::is_whitespace),
                "({host}, {label}) suspect placeholder token left in body: {tok:?}"
            );
        }
        cursor = abs + "__COSMON_".len();
    }
}

fn assert_sh_syntax_valid(body: &str, host: &str, label: &str) {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let workdir = tempfile::tempdir().unwrap();
    let script = workdir.path().join("install.sh");
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new("sh").arg("-n").arg(&script).output().unwrap();
    assert!(
        out.status.success(),
        "({host}, {label}) `sh -n install.sh` failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// v1.3.1 regression: install.sh must (a) carry zero `__COSMON_*__`
/// placeholder tokens after serving, and (b) syntax-validate under
/// `sh -n`, on both http (loopback) and https (remote) topologies,
/// and on both empty and fully-configured `install_templating`.
///
/// v1.3 smoke surfaced two defects on the local http loopback that
/// neither existing test exercised:
///
///   1. `curl --proto '=https' --tlsv1.2` flatly rejected the http
///      URL the script then issued — every install over loopback or
///      Tailscale-internal http endpoint died with
///      `curl: (1) Protocol "http" disabled`.
///   2. The literal token `__COSMON_CONFIG_SET_BLOCK__` appeared inside
///      a `#`-prefixed *comment* in the template. When the placeholder
///      expanded to a multi-line block, only the line that carried the
///      placeholder remained commented; the rest became executable
///      shell that fired `config: command not found` and
///      `→: command not found` before the script reached its real
///      work.
///
/// This test pins both invariants at the script-text level so neither
/// defect can re-enter the template without a failing red test.
#[tokio::test]
async fn install_sh_no_placeholder_leakage_and_syntax_valid_all_topologies() {
    // Two templatings × two topologies = four bodies to check.
    let topologies: &[(&str, Option<&str>)] = &[
        ("127.0.0.1:8443", None),                 // local http loopback
        ("cosmon.example.ts.net", Some("https")), // public Tailscale
    ];

    for (host, fwd_proto) in topologies {
        // ---- empty templating ----------------------------------------
        let security_dir = tempfile::tempdir().unwrap();
        let state = make_state(security_dir.path()).await;
        let body = fetch_install_sh_with_state(state, host, *fwd_proto).await;
        assert_no_placeholder_leakage(&body, host, "empty-templating");
        assert_sh_syntax_valid(&body, host, "empty-templating");

        // ---- configured templating (mirrors AWS live-deploy) -------------
        let security_dir2 = tempfile::tempdir().unwrap();
        let dist_root = tempfile::tempdir().unwrap();
        let mut state2 =
            make_state_with_dist_root(security_dir2.path(), dist_root.path().to_path_buf()).await;
        state2.install_templating =
            std::sync::Arc::new(cosmon_rpp_adapter::config::InstallTemplating {
                sub: "tenant-demo-operator".into(),
                aud: "cosmon-rpp-tenant".into(),
                oidc_url: "__COSMON_HOST__/oidc".into(),
                noyau: "tenant-demo".into(),
            });
        let body2 = fetch_install_sh_with_state(state2, host, *fwd_proto).await;
        assert_no_placeholder_leakage(&body2, host, "configured-templating");
        assert_sh_syntax_valid(&body2, host, "configured-templating");
        // Pin the v1.3.1 regression specifically: no *executable*
        // (non-comment) line may pin curl to https-only. Comments
        // referencing the historical flag are fine — only an actively
        // executed `--proto '=https'` is the defect.
        let exec_proto_https = body2
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .any(|l| l.contains("--proto '=https'"));
        assert!(
            !exec_proto_https,
            "({host}, configured) active curl line still pins https-only — \
             v1.3 defect would re-block http loopback installs"
        );
    }
}

// ---------------------------------------------------------------------
// B2 path-layout residue — snapshot the served install.sh against the
// single Rust-side source of the dist URL layout (`task-20260607-4b79`,
// parent `delib-20260607-aec8`).
//
// The B2 *scheme* drift (http/https) is already collapsed: the five
// `install_sh_*` tests above pin every honest scheme case (loopback→http,
// remote-no-header→http, remote `X-Forwarded-Proto: https`→https, explicit
// http, chained-list first-token). The *remaining* drift is the binary
// **path layout** — stated in Rust (the `/dist/binary/{platform}/cosmon-
// remote` route, derived from `dist::binary_url_path`) **and** in the shell
// (the URL the script builds, install.sh ~line 73). The script runs via
// `curl|sh` on a host with no cosmon binary and no Rust: "the socket is a
// type-system event horizon" (tolnay) — no shared type can reach across it.
//
// This is a *trace, not a mechanism*: one `#[test]`, no validator in the
// running system, no CI gate as standing policy, no daemon, no schema
// registry. It pins a generated artefact against its own source (the script
// is re-derived from `binary_url_path`, then diffed); it is NOT the
// forbidden checker reconciling two independently-maintained authorities
// (godel). It fails at `cargo test` iff a human edits the shell path
// without editing the Rust route, or vice versa — the cheapest possible
// substitute for the type the two languages cannot unify.
// ---------------------------------------------------------------------

#[tokio::test]
async fn install_sh_paths_match_dist_route() {
    use cosmon_rpp_adapter::routes::dist::{binary_url_path, KNOWN_PLATFORMS};

    // Render the script from a remote TLS-terminated connection so the
    // scheme is genuinely rendered (not hand-typed) into the host.
    let (status, _ct, rendered) = get("/install.sh", "cosmon.example.ts.net", Some("https")).await;
    assert_eq!(status, StatusCode::OK);

    // (1) Path layout pinned: the shell assembles `$COSMON_HOST` + the
    // exact URL path the route serves, with the platform deferred to the
    // runtime `$PLATFORM` shell variable. `binary_url_path` is the single
    // Rust-side source the route registration is also derived from, so
    // this re-derives the script against itself.
    let url_template = binary_url_path("$PLATFORM");
    assert!(
        rendered.contains(&url_template),
        "install.sh must build the dist URL from the route's path layout \
         ({url_template:?}); a hand-edit of either side desynced them"
    );
    assert!(
        rendered.contains(&format!("\"$COSMON_HOST{url_template}\"")),
        "the script must prefix the path with $COSMON_HOST (scheme+host)"
    );

    // (2) Platform name set pinned: every platform the Rust route accepts
    // must be reachable from the shell's `uname` case block as a
    // `PLATFORM=<name>` assignment. Add a platform to `KNOWN_PLATFORMS`
    // without teaching the shell to detect it → this fails.
    for p in KNOWN_PLATFORMS {
        assert!(
            rendered.contains(&format!("PLATFORM={p}")),
            "install.sh case block must map a uname to PLATFORM={p} \
             (in KNOWN_PLATFORMS but absent from the shell)"
        );
    }

    // (3) Scheme rendered server-side, not hand-typed in the template.
    assert!(
        rendered.contains("https://cosmon.example.ts.net"),
        "scheme+host must be rendered from the connection, not literal"
    );

    // (4) Placeholder consumed — no `__COSMON_HOST__` survives serving.
    assert!(
        !rendered.contains("__COSMON_HOST__"),
        "host placeholder must be fully substituted"
    );
}

#[tokio::test]
async fn dist_binary_path_traversal_is_rejected_by_allow_list() {
    // Axum's path matching would already strip `..` segments, but the
    // allow-list is the contractual guarantee: any platform string
    // outside `KNOWN_PLATFORMS` returns 404 before any filesystem
    // operation is attempted.
    let (_sec, _dist, app) = app_with_seeded_dist(&["macos-arm64"]).await;
    // URL-encoded `..%2F..%2Fetc%2Fpasswd` as platform segment.
    let (status, _ct, _cd, _body) =
        get_bytes(app, "/dist/binary/..%2F..%2Fetc%2Fpasswd/cosmon-remote").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- /dist/CLAUDE.md — the recommended tenant CLAUDE.md block --------------
//
// smithy avatar-surface C2 (delib-20260610-9a0c K6/K7). The block is
// frozen prose with three contractual gates, each re-computed here
// rather than declared: (1) at most 10 content lines — the short block
// IS the test that the CLI self-documents; (2) no dated-future wording
// — direction is stated by invariant, never by calendar (godel Q7);
// (3) no internal entity names — the block is tenant-facing.

/// The served block, fetched over HTTP like a tenant would.
async fn fetch_claude_md() -> String {
    let (status, content_type, body) = get("/dist/CLAUDE.md", "127.0.0.1:8443", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/markdown"),
        "content-type was {content_type:?}"
    );
    body
}

#[tokio::test]
async fn dist_claude_md_served_unauthenticated_as_markdown() {
    let body = fetch_claude_md().await;
    assert!(
        body.starts_with("## cosmon-remote"),
        "block must open with its heading"
    );
    // Host-agnostic by design: nothing to template, nothing templated.
    assert!(!body.contains("__COSMON_HOST__"));
}

#[tokio::test]
async fn dist_claude_md_stays_within_ten_content_lines() {
    let body = fetch_claude_md().await;
    // Content lines = non-empty lines that are not the heading.
    let content_lines = body
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .count();
    assert!(
        content_lines <= 10,
        "the block must hold in <=10 content lines (got {content_lines}); \
         if it cannot, the CLI is not self-documenting enough — fix the \
         CLI, do not grow the block"
    );
}

#[tokio::test]
async fn dist_claude_md_names_no_dated_future_and_no_internal_entities() {
    let body = fetch_claude_md().await.to_lowercase();
    // godel Q7: direction by invariant, never by calendar. Any of these
    // words is a Goedel sentence falsifiable at our expense.
    for forbidden in ["bientôt", "demain", "à terme", "v2", "saas"] {
        assert!(
            !body.contains(forbidden),
            "dated-future / banned wording {forbidden:?} must not appear"
        );
    }
    // Tenant-facing confidentiality: internal entities never named.
    for forbidden in ["democorp", "noogram"] {
        assert!(
            !body.contains(forbidden),
            "internal entity {forbidden:?} must not appear in a tenant-facing artefact"
        );
    }
}

#[tokio::test]
async fn dist_claude_md_carries_the_load_bearing_invariants() {
    // The rate-distortion content the panel froze (jobs x shannon):
    // each item below is something `--help` can never teach the agent.
    let body = fetch_claude_md().await;
    // Single slit — no direct shell.
    assert!(body.contains("ssh") && body.contains("docker exec"));
    // Discovery pointer — the help IS the reference.
    assert!(body.contains("--help"));
    // Two-badge sequence.
    assert!(body.contains("deux badges"));
    // Cost model — only tackle burns credit.
    assert!(body.contains("tackle") && body.contains("crédit"));
    // Interruption asymmetry.
    assert!(body.contains("ne t'interrompt jamais"));
    // Named §8p refusals — the agent must not search for operator verbs.
    assert!(body.contains("§8p") && body.contains("`done`") && body.contains("`evolve`"));
}
