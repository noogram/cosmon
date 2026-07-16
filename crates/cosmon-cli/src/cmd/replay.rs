// SPDX-License-Identifier: AGPL-3.0-only

//! `cs replay` — open an interactive D3 timeline of the local fleet run.
//!
//! Reads `.cosmon/state/events.jsonl` + molecule `state.json` files, projects
//! them into the replay schema (see [`cosmon_observability::replay`]) and
//! either:
//!
//! - writes a self-contained HTML to a temp file and opens it (default), or
//! - `--port N`: starts a tiny axum server that serves `/` (HTML) and
//!   `/events.json` for the current project, blocking until Ctrl-C.
//!
//! # ADR-016 coherence
//!
//! Read-only projection of on-disk state. Stateless, idempotent. The
//! `--port` server is a session-scoped renderer (not a daemon): it
//! exits on Ctrl-C and owns no state.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use cosmon_observability::replay::{build_events, render_fetch, render_standalone, ReplayMolecule};
use sha2::{Digest, Sha256};

use super::Context;

/// Arguments for `cs replay`.
#[derive(clap::Args)]
pub struct Args {
    /// Start an embedded HTTP server on this port instead of opening a
    /// file. Serves `/` (HTML) and `/events.json` for the current project
    /// until Ctrl-C.
    #[arg(long)]
    pub port: Option<u16>,

    /// Skip launching the browser (standalone-file mode only).
    #[arg(long)]
    pub no_open: bool,

    /// Write the standalone HTML to this path instead of a temp file.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// Execute `cs replay`.
///
/// # Errors
///
/// Propagates I/O errors when reading state or writing the HTML file.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);

    if let Some(port) = args.port {
        return serve(state_dir, port);
    }

    let events = build_events(&state_dir)
        .map_err(|e| anyhow::anyhow!("failed to read replay state: {e}"))?;
    if events.is_empty() {
        eprintln!(
            "cs replay: no molecules or events found in {}",
            state_dir.display()
        );
    }

    let html = render_standalone(&events);
    let out_path = resolve_out_path(args.out.as_ref(), &events);
    std::fs::write(&out_path, html)?;

    if ctx.json {
        let json = serde_json::json!({
            "path": out_path.display().to_string(),
            "molecules": events.len(),
        });
        println!("{}", serde_json::to_string(&json)?);
    } else {
        println!(
            "cs replay: wrote {} ({} molecules)",
            out_path.display(),
            events.len()
        );
    }

    if !args.no_open {
        open_in_browser_path(&out_path);
    }
    Ok(())
}

fn resolve_out_path(explicit: Option<&PathBuf>, events: &[ReplayMolecule]) -> PathBuf {
    if let Some(p) = explicit {
        return p.clone();
    }
    let mut hasher = Sha256::new();
    for e in events {
        hasher.update(e.id.as_bytes());
    }
    let digest = hasher.finalize();
    let hash = hex8(&digest);
    std::env::temp_dir().join(format!("cosmon-replay-{hash}.html"))
}

fn hex8(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(16);
    for b in bytes.iter().take(8) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn open_in_browser_path(path: &std::path::Path) {
    open_target(&path.display().to_string());
}

fn open_target(target: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "start"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(target).spawn();
}

#[derive(Clone)]
struct ServerState {
    state_dir: PathBuf,
}

fn serve(state_dir: PathBuf, port: u16) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let app_state = Arc::new(ServerState { state_dir });
        let app = Router::new()
            .route("/", get(serve_index))
            .route("/events.json", get(serve_events))
            .with_state(app_state);

        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        println!("cs replay: serving on http://{addr}/  (Ctrl-C to stop)");
        open_target(&format!("http://{addr}/"));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })
}

async fn serve_index() -> Html<String> {
    Html(render_fetch("/events.json"))
}

async fn serve_events(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    match build_events(&state.state_dir) {
        Ok(events) => Json(events).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read replay state: {e}"),
        )
            .into_response(),
    }
}
