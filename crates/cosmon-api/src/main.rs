// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-api` — local HTTP daemon shelling out to the `cs` CLI.
//!
//! Designed for native pilots (Mac menubar, iOS, iPad) that need to
//! drive `cs session start|note|end` over HTTP instead of spawning a
//! subprocess directly. See the crate-level docs on [`cosmon_api`] for
//! the endpoint surface and security model.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use cosmon_api::{default_galaxies_root, router, AppState};

/// Command-line surface for the `cs-api` binary.
#[derive(Debug, Parser)]
#[command(
    name = "cs-api",
    about = "Local HTTP adapter for cosmon session + inbox commands",
    version
)]
struct Cli {
    /// Address to bind. Default `127.0.0.1:4222` (loopback only).
    #[arg(long, default_value = "127.0.0.1:4222")]
    bind: SocketAddr,

    /// Path to the `cs` binary. Defaults to `cs` on `$PATH`.
    #[arg(long, value_name = "PATH")]
    cs_path: Option<PathBuf>,

    /// Override for `$COSMON_STATE_DIR`. When omitted the child `cs`
    /// processes inherit the server's environment and the inbox /
    /// whispers endpoints fall back to walk-up / `$HOME/.cosmon/state`.
    #[arg(long, value_name = "PATH")]
    cosmon_state: Option<PathBuf>,

    /// Override for the whisper inbox root. Defaults to
    /// `<cosmon_state parent>/whispers/inbox` which mirrors the
    /// `cosmon-matrix-tick` layout.
    #[arg(long, value_name = "PATH")]
    whispers_inbox: Option<PathBuf>,

    /// Root directory scanned by `GET /galaxies`. Defaults to
    /// `$HOME/galaxies`.
    #[arg(long, value_name = "PATH")]
    galaxies_root: Option<PathBuf>,

    /// Override for the cluster-config file (ADR-066). Defaults to
    /// `$COSMON_CLUSTER_CONFIG` or `$HOME/.config/cosmon/cluster.toml`.
    #[arg(long, value_name = "PATH")]
    cluster_config: Option<PathBuf>,

    /// Enable verbose logging (info-level by default; debug with this flag).
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        "cosmon_api=debug,tower_http=debug,info"
    } else {
        "cosmon_api=info,warn"
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .try_init();

    let cs_path = resolve_cs_path(cli.cs_path.as_ref())?;
    let galaxies_root = cli.galaxies_root.unwrap_or_else(default_galaxies_root);
    tracing::info!(
        cs_path = %cs_path.display(),
        bind = %cli.bind,
        galaxies_root = %galaxies_root.display(),
        cosmon_state = cli.cosmon_state.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<inherit>".to_owned()),
        "cs-api starting"
    );

    let mut state = AppState::new(cs_path).with_galaxies_root(galaxies_root);
    if let Some(dir) = cli.cosmon_state {
        state = state.with_state_dir(dir);
    }
    if let Some(dir) = cli.whispers_inbox {
        state = state.with_whispers_inbox_root(dir);
    }
    if let Some(path) = cli.cluster_config {
        state = state.with_cluster_config_path(path);
    }
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

/// Resolve the absolute path to the `cs` binary. When `--cs-path` is
/// omitted we walk `$PATH` and pick the first `cs` we find, so startup
/// fails fast if `cs` is not installed.
fn resolve_cs_path(explicit: Option<&PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.exists() {
            anyhow::bail!("cs binary does not exist at {}", path.display());
        }
        return Ok(path.clone());
    }
    let path = std::env::var_os("PATH").ok_or_else(|| anyhow::anyhow!("$PATH is unset"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("cs");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not find `cs` on $PATH — pass --cs-path explicitly")
}
