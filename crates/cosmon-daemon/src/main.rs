// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-daemon` binary — boot the multi-galaxy HTTP daemon.
//!
//! Bind: `host.example:8790` via [`apps_transport_http`]
//! Tailscale auto-discovery (override with `COSMON_DAEMON_HTTP_BIND`).
//! The same daemon serves every galaxy under `/srv/cosmon/` (override
//! with `COSMON_GALAXIES_ROOT`).

use std::sync::Arc;

use apps_transport_http::{
    access_log_layer, request_id_layer, serve_http_on_tailscale, TailscaleBind,
};
use axum::{middleware, Router};
use cosmon_daemon::{handlers, AppState, GalaxiesRoot};
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 8790;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let root = GalaxiesRoot::from_env();
    eprintln!(
        "cosmon-daemon: galaxies root = {}",
        root.as_path().display()
    );

    let state = Arc::new(AppState::new(root));
    let app: Router = handlers::build_router(Arc::clone(&state))
        .layer(middleware::from_fn(access_log_layer))
        .layer(middleware::from_fn(request_id_layer));

    let bind = TailscaleBind::EnvOrAuto {
        port: DEFAULT_PORT,
        env_var: "COSMON_DAEMON_HTTP_BIND",
    };
    let outcome = serve_http_on_tailscale(bind, app, async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("cosmon-daemon: ctrl-c, shutting down");
    })
    .await?;
    tracing::info!(
        bind = %outcome.addr,
        source = outcome.source.as_str(),
        "cosmon-daemon: server stopped"
    );
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,cosmon_daemon=info,apps_transport_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
