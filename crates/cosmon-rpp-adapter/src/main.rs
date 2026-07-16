// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-rpp-adapter` binary entry — boots the §8j HTTPS+OIDC
//! ingress adapter (ADR-080).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use cosmon_rpp_adapter::{
    auth_claude::{AuthClaudeConfig, AuthClaudeState, FilesystemSessionStore},
    deny_list::DenyList,
    jwt::JwksStore,
    nucleon_map::{render_oidc_identity_toml, HabilitationBindingSpec},
    router,
    subprocess::resolve_cs_path,
    AppState, BackendHealthRegistry, HabilitationMap, IngressRateLimiter, Posture,
    SharedHabilitationMap, SharedJwksStore,
};
use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Remote Pilot Port — §8j HTTPS+OIDC ingress adapter (ADR-080)."
)]
struct Cli {
    /// Path to the operator config (`~/.config/cosmon/rpp.toml`).
    #[arg(long, default_value = "~/.config/cosmon/rpp.toml")]
    config: PathBuf,

    /// Override the bind address.
    #[arg(long)]
    bind: Option<String>,

    /// Override the posture (`prepared` | `active`).
    #[arg(long)]
    posture: Option<String>,

    /// Optional operator subcommand. When absent the binary serves the
    /// §8j ingress (the production CMD `cs-rpp-adapter --bind …`).
    #[command(subcommand)]
    cmd: Option<OperatorCmd>,
}

/// Operator-only, host-side maintenance verbs (Pierre hardening P2).
/// These are NOT served over HTTP — the binary
/// runs them and exits. Keeping them in the deployed binary means the
/// operator renders a binding with the SAME audited schema the server
/// loads, with no extra tool to install. The §8j root-of-trust
/// (creating a binding) is never exposed to a tenant JWT.
#[derive(Debug, Subcommand)]
enum OperatorCmd {
    /// Nucleon-binding maintenance (operator-only, host-side).
    Nucleon {
        #[command(subcommand)]
        sub: NucleonCmd,
    },
    /// Trust-state maintenance (operator-only, host-side).
    Trust {
        #[command(subcommand)]
        sub: TrustCmd,
    },
}

#[derive(Debug, Subcommand)]
enum TrustCmd {
    /// Run one trust-bootstrap convergence pass (ADR-141) and exit —
    /// the same code the server runs at boot. Reads the declaration
    /// sources (handoff dir from `[trust_bootstrap]` in the config, the
    /// `TRUSTED_ISS`/`TRUSTED_JWKS_URI`/`TRUSTED_AUDIENCES` env trio,
    /// static `[[trust_bootstrap.issuer]]` entries) and converges
    /// `security/trusted-issuers.toml` + the handoff-declared nucleon
    /// bindings. Exit non-zero on any fail-closed refusal.
    /// `TRUSTED_FORCE=1` escalates to a full allowlist rewrite.
    Converge,
}

#[derive(Debug, Subcommand)]
enum NucleonCmd {
    /// Render a validated `oidc-identity.toml` to stdout from the
    /// authorization four-tuple. The operator redirects it into the
    /// host-side `nucleons/<id>/oidc-identity.toml` bind-mount source;
    /// the adapter picks it up on its next boot. Pure: writes nothing,
    /// reads no server state, needs no running container.
    Render {
        /// Tenant axis (galaxy slot).
        #[arg(long)]
        noyau: String,
        /// JWT `sub` claim the binding admits.
        #[arg(long)]
        sub: String,
        /// JWT `iss` — must equal the `IdP` issuer and the minted JWT.
        #[arg(long)]
        iss: String,
        /// JWT `aud` pinned to this deployment.
        #[arg(long)]
        aud: String,
        /// Directory name under `nucleons/` (default: `<noyau>`).
        #[arg(long)]
        nucleon_id: Option<String>,
        /// Cognitive-substrate label (default: `Biological`).
        #[arg(long)]
        phase: Option<String>,
        /// Binding-granted scope (repeatable). Omit for JWT-scopes-only.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// ISO-8601 provisioning timestamp recorded in the file.
        #[arg(long)]
        sealed_at: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Operator subcommand path — render-and-exit, never serves.
    if let Some(OperatorCmd::Nucleon { sub }) = &cli.cmd {
        return run_nucleon_cmd(sub);
    }

    let config_path = expand_tilde(&cli.config);
    let cfg = cosmon_rpp_adapter::config::RppConfig::load(&config_path)?;

    // Operator one-shot: run the boot-time trust convergence and exit
    // (ADR-141). Same code path as the serving boot below — the
    // validation bench drives this exactly like the retired seed
    // entrypoint.
    if let Some(OperatorCmd::Trust {
        sub: TrustCmd::Converge,
    }) = &cli.cmd
    {
        let state_dir = cfg.resolved_state_dir();
        let report =
            cosmon_rpp_adapter::trust_bootstrap::converge(&state_dir, &cfg.trust_bootstrap)
                .map_err(|e| anyhow::anyhow!("trust converge failed (fail-closed): {e}"))?;
        println!(
            "trust converge OK — declared={} handoff_files={} wrote_allowlist={} \
             issuers_total={} foreign_preserved={} bindings_written={:?} bindings_unchanged={}",
            report.declared,
            report.handoff_files,
            report.wrote_allowlist,
            report.issuers_total,
            report.foreign_preserved,
            report.bindings_written,
            report.bindings_unchanged,
        );
        return Ok(());
    }

    tracing::info!(
        event = "boot.config",
        config_path = %config_path.display(),
        config_present = config_path.exists(),
        "loaded operator config",
    );

    let bind_addr = cli.bind.unwrap_or_else(|| cfg.resolved_bind_addr());
    let posture = match cli.posture.as_deref() {
        Some("active") => Posture::Active,
        Some("prepared") | None => cfg.resolved_posture(),
        Some(other) => anyhow::bail!("unknown posture: {other}"),
    };

    let state_dir = cfg.resolved_state_dir();
    let inbox_root = cfg.resolved_inbox_root();
    let galaxies_root = cfg.resolved_galaxies_root();
    let cs_path = resolve_cs_path(cfg.cs_path.as_deref());

    tracing::info!(
        event = "boot.posture",
        posture = ?posture,
        "posture resolved",
    );
    if matches!(posture, Posture::Prepared) {
        tracing::warn!(
            event = "boot.posture.prepared",
            posture = ?posture,
            "RPP starting in `prepared` posture — laxities are warned, not enforced",
        );
    }

    tracing::info!(
        event = "boot.paths",
        state_dir = %state_dir.display(),
        state_dir_source = cfg.state_dir_source(),
        inbox_root = %inbox_root.display(),
        galaxies_root = %galaxies_root.display(),
        cs_path = %cs_path.display(),
        "filesystem roots resolved",
    );

    // Trust bootstrap (ADR-141) — the server converges its OWN authn
    // allowlist + authz bindings from the declaration sources (IdP
    // handoff files, env trio, static config) BEFORE the JWKS fetch is
    // armed and BEFORE the habilitation map loads. Replaces the v3.0
    // `cosmon-seed` init-container. Fail-closed: any refusal (degenerate
    // declaration, parse-back failure, handoff wait expired) aborts the
    // boot — under `restart: unless-stopped` this is a self-healing
    // crash-loop, never a silent deny-all with a healthy-looking server.
    cosmon_rpp_adapter::trust_bootstrap::converge(&state_dir, &cfg.trust_bootstrap)
        .map_err(|e| anyhow::anyhow!("trust bootstrap failed (fail-closed): {e}"))?
        .log();

    // The JWKS store (authn door) is held behind an `arc-swap` handle so
    // its keys can be refreshed without a reboot. Keys reach it by one of
    // two delivery paths (jwt.rs module doctrine):
    //
    //   * HTTP-fetch (primary, OIDC standard) — when the host-side
    //     allowlist `<state_dir>/security/trusted-issuers.toml` is
    //     present, the adapter FETCHES each issuer's JWKS from its
    //     `jwks_uri` and refreshes on a TTL + on-demand cache-miss
    //     (smithy spec jwks-http-fetch-provisioning.md). This replaces
    //     the v2.4 file-stage + SIGHUP gesture (no debug access needed on
    //     the protected target).
    //   * File-stage (compat fallback) — no allowlist present: read
    //     `security/jwks/*.json` and rely on the SIGHUP listener below.
    //     Kept for the test bench and the oidc-mock.
    let trusted = cosmon_rpp_adapter::jwks_fetch::TrustedIssuers::load(&state_dir)?;
    let jwks = if trusted.is_empty() {
        let store = SharedJwksStore::new(JwksStore::load(&state_dir)?);
        let key_counts = store.load().key_counts_by_issuer();
        if key_counts.is_empty() {
            tracing::info!(
                event = "boot.jwks",
                mode = "file",
                issuers = 0,
                keys_total = 0,
                "no JWKS loaded (no trusted-issuers.toml, no security/jwks/*.json) — \
                 adapter will reject every request until keys are provisioned",
            );
        } else {
            let total: usize = key_counts.iter().map(|(_, n)| n).sum();
            for (iss, n) in &key_counts {
                tracing::info!(event = "boot.jwks.issuer", iss = %iss, keys = n, "JWKS loaded");
            }
            tracing::info!(
                event = "boot.jwks",
                mode = "file",
                issuers = key_counts.len(),
                keys_total = total,
                "JWKS load summary (file-stage fallback)",
            );
        }
        store
    } else {
        // HTTP-fetch primary path. Build the provider over a fresh store,
        // do one synchronous initial fetch (best-effort, fail-closed: an
        // unreachable issuer leaves its keys empty → deny), then spawn the
        // TTL + boot-backoff refresh loop. The provider holds a clone of
        // the SAME shared store that goes into AppState, so every handler's
        // `state.jwks.load()` transparently sees the fetched keys.
        let fetcher = cosmon_rpp_adapter::jwks_fetch::JwksFetcher::new()?;
        let provider = cosmon_rpp_adapter::jwks_fetch::JwksProvider::new(
            SharedJwksStore::new(JwksStore::default()),
            trusted.issuers.clone(),
            fetcher,
        );
        let store = provider.shared();
        tracing::info!(
            event = "boot.jwks",
            mode = "http-fetch",
            issuers = trusted.len(),
            "JWKS HTTP-fetch armed — fetching trusted issuers from their jwks_uri",
        );
        provider.refresh_all().await.log();
        let ttl = cfg.resolved_jwks_refresh_ttl();
        tokio::spawn(provider.run(ttl));
        store
    };

    // Nucleon-binding seed (smithy autonomie-pool, task-20260614-f16f):
    // symmetric to FORMULAS_SEED_DIR. Bootstrap the default binding from the
    // image-baked seed (`/opt/cosmon-nucleons`) when the instance's
    // `nucleons/` is empty, so a freshly-cut pool instance self-provisions
    // its operator binding at first boot — no root/SSM write per instance.
    // Runs BEFORE the map load: the binding IS the bootstrap (without it,
    // zero noyaux resolve and image_init has nothing to materialise).
    // No-clobber: an existing binding short-circuits the seed.
    {
        let seed_dir = {
            let p = PathBuf::from(cosmon_rpp_adapter::image_init::NUCLEONS_SEED_DIR);
            p.is_dir().then_some(p)
        };
        let seed = cosmon_rpp_adapter::image_init::seed_nucleons(&state_dir, seed_dir.as_deref());
        match &seed {
            cosmon_rpp_adapter::image_init::StepOutcome::Failed(reason) => tracing::warn!(
                event = "boot.nucleon_seed",
                outcome = "failed",
                reason = %reason,
            ),
            other => tracing::info!(
                event = "boot.nucleon_seed",
                outcome = ?other,
                "nucleon-binding seed",
            ),
        }
    }

    let nucleon_map = SharedHabilitationMap::new(HabilitationMap::load(&state_dir)?);
    tracing::info!(
        event = "boot.nucleons",
        bindings = nucleon_map.load().binding_count(),
        "nucleon bindings loaded",
    );

    // cs-server-image-init-discipline (idea-20260521-7f97, smithy):
    // materialise the per-noyau state tree at boot, absorbing the former
    // `cosmon-server-init.sh` ENTRYPOINT. Eager (B2): every noyau known
    // to the HabilitationMap at boot gets its galaxy tree, `cs init`, and a
    // local git repo. Best-effort — a per-noyau failure is logged, not
    // fatal, so the adapter still serves. Runs BEFORE any worker spawn
    // so the Claude Code config gates (3a/3b) are written ahead of the
    // first `cs tackle`.
    let claude_home =
        std::env::var_os("HOME").map_or_else(|| PathBuf::from("/root"), PathBuf::from);
    let formulas_seed_dir = {
        let p = PathBuf::from(cosmon_rpp_adapter::image_init::FORMULAS_SEED_DIR);
        p.is_dir().then_some(p)
    };
    let image_init = cosmon_rpp_adapter::image_init::ImageInit {
        inbox_root: inbox_root.clone(),
        galaxies_root: galaxies_root.clone(),
        cs_path: cs_path.clone(),
        claude_home,
        formulas_seed_dir,
    };
    let noyaux = nucleon_map.load().noyaux();
    tracing::info!(
        event = "boot.image_init",
        noyaux = noyaux.len(),
        "materialising per-noyau state tree (cs-server-image-init-discipline)",
    );
    let init_report = image_init.run(&noyaux);
    init_report.log();

    // Arm the SIGHUP reload listener (idea — the "reload" half of the
    // Pierre P2 supported nucleon-creation path, extended in ADR-0023
    // MVP-A to cover the JWKS authn door too). One `kill -HUP <pid>` now
    // re-reads BOTH doors: the binding map (authz) AND the JWKS store
    // (authn), so staging or revoking a federated peer issuer activates
    // without a restart that would tear down in-flight tmux workers. The
    // task holds cheap clones of both swappable handles and the boot
    // ImageInit; the originals move into `AppState` below. Unix-only —
    // non-unix builders fall back to a restart.
    #[cfg(unix)]
    tokio::spawn(cosmon_rpp_adapter::reload::sighup_reload_listener(
        nucleon_map.clone(),
        jwks.clone(),
        state_dir.clone(),
        image_init.clone(),
    ));

    // Step 3c — resolve the Anthropic key from the ladder once at boot,
    // for injection into every worker-spawn env (see `AppState`).
    let anthropic_api_key =
        if let Some((key, backend)) = cosmon_rpp_adapter::image_init::resolve_anthropic_key() {
            tracing::info!(
                event = "boot.anthropic_auth",
                backend = backend.as_str(),
                key_fp = %cosmon_rpp_adapter::image_init::key_fingerprint(&key),
                "anthropic key resolved for worker spawn env",
            );
            Some(key)
        } else {
            tracing::warn!(
                event = "boot.anthropic_auth",
                "no anthropic key (docker-secret / operator-file / env all empty) — \
             cs tackle will fail with 'ANTHROPIC_API_KEY not set'",
            );
            None
        };

    let rate_limiter = Arc::new(IngressRateLimiter::default_in(
        state_dir.join("security/oidc-rate-limit"),
    ));
    let deny_list = Arc::new(DenyList::new(state_dir.clone()));

    let backend_health = Arc::new(BackendHealthRegistry::new());
    let configured_backends = cfg.resolved_backends();
    backend_health.register_configured(configured_backends.clone());
    tracing::info!(
        event = "boot.backends",
        configured = configured_backends.len(),
        names = ?configured_backends,
        "LLM backends registered (status=configured-but-unused until first probe)",
    );

    // auth-claude surface (ADR-0017 smithy, no-direct-shell). Boot
    // it with the discovered upstream Anthropic OAuth config and a
    // filesystem session store under `<state_dir>/auth-sessions/`.
    // Failure to initialise leaves the optional surface as `None`;
    // the routes will return 503 service_unavailable but the rest of
    // the adapter still boots.
    let auth_claude = build_auth_claude_state(&state_dir).map(Arc::new);
    if auth_claude.is_some() {
        tracing::info!(
            event = "boot.auth_claude_api",
            enabled = true,
            "auth-claude surface mounted at /v1/auth/claude/* (ADR-0017)",
        );
    } else {
        tracing::warn!(
            event = "boot.auth_claude_api",
            enabled = false,
            "auth-claude surface NOT mounted — session store init failed",
        );
    }

    // Host-sealed operator credential for the admin provisioning surface
    // (task-20260616-f112). Fail-closed: absent COSMON_ADMIN_TOKEN ⇒
    // `/v1/admin/*` returns 403 admin_disabled, a non-regressive default.
    let admin_seal = std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::from_env());
    tracing::info!(
        event = "boot.admin_seal",
        enabled = admin_seal.is_enabled(),
        "admin provisioning surface (/v1/admin/*) — sealed-operator auth",
    );
    // Single-writer binding provisioner: reuses the boot ImageInit so a
    // freshly-bound noyau materialises its galaxy tree before the binding
    // becomes resolvable, and reloads the live map in-process (no reboot).
    let provisioner = std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::new(
        state_dir.clone(),
        galaxies_root.clone(),
        nucleon_map.clone(),
        image_init.clone(),
    ));
    // Presentation-layer federation tooling (ADR-0023 G5): one gesture →
    // N per-galaxy habilitations, grouped as one portée. Writes through
    // the same single-writer provisioner.
    let portee_provisioner = std::sync::Arc::new(
        cosmon_rpp_adapter::portee::PorteeProvisioner::new(state_dir.clone(), provisioner.clone()),
    );

    let state = AppState {
        cs_path,
        state_dir,
        inbox_root,
        galaxies_root,
        jwks,
        nucleon_map,
        rate_limiter,
        deny_list,
        posture,
        subprocess_timeout: cfg.resolved_subprocess_timeout(),
        anthropic_api_key,
        claude_model: cfg.resolved_claude_model(),
        backend_health,
        auth_claude,
        artifact_root: cfg.resolved_artifact_root(),
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            cfg.resolved_dist_root(),
        )),
        install_templating: std::sync::Arc::new(cfg.install_templating.clone()),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal,
        provisioner,
        portee_provisioner,
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(
        event = "boot.listening",
        addr = %bind_addr,
        "cosmon-rpp-adapter listening",
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// Execute an operator `nucleon …` verb and exit. Pure with respect to
/// server state — `render` only validates the four-tuple and prints the
/// `oidc-identity.toml` body to stdout.
fn run_nucleon_cmd(cmd: &NucleonCmd) -> anyhow::Result<()> {
    match cmd {
        NucleonCmd::Render {
            noyau,
            sub,
            iss,
            aud,
            nucleon_id,
            phase,
            scopes,
            sealed_at,
        } => {
            let spec = HabilitationBindingSpec {
                noyau: noyau.clone(),
                sub: sub.clone(),
                issuer: iss.clone(),
                audience: aud.clone(),
                nucleon_id: nucleon_id.clone(),
                phase: phase.clone(),
                scopes: scopes.clone(),
                sealed_at: sealed_at.clone(),
            };
            let body = render_oidc_identity_toml(&spec)
                .map_err(|e| anyhow::anyhow!("invalid nucleon binding: {e}"))?;
            print!("{body}");
            Ok(())
        }
    }
}

fn expand_tilde(p: &std::path::Path) -> PathBuf {
    if let Ok(s) = p.to_path_buf().into_os_string().into_string() {
        if let Some(rest) = s.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
    }
    p.to_path_buf()
}

/// Construct the auth-claude state, or return `None` if the session
/// store cannot be initialised. The credentials path lives under the
/// current user's `$HOME` so a container running as `cosmon` writes
/// `/cosmon/.claude/.credentials.json` (the image sets `HOME=/cosmon`
/// and `useradd --home-dir /cosmon` keeps `/etc/passwd` in agreement).
fn build_auth_claude_state(state_dir: &std::path::Path) -> Option<AuthClaudeState> {
    let store = match FilesystemSessionStore::new(state_dir) {
        Ok(s) => Arc::new(s) as Arc<dyn cosmon_rpp_adapter::auth_claude::SessionStore>,
        Err(e) => {
            tracing::error!(
                event = "boot.auth_claude_api.store_init_failed",
                error = %e,
            );
            return None;
        }
    };
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/root"), PathBuf::from);
    let config = AuthClaudeConfig::defaults_with_home(&home);
    Some(AuthClaudeState::new(config, store))
}
