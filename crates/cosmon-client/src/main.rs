// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::unnecessary_debug_formatting,
    clippy::doc_markdown
)]

//! `cs-client` — thin CLI that mirrors the cosmon pilot cycle over HTTP.
//!
//! ```text
//! cs-client nucleate <formula> --var k=v
//! cs-client tackle   <id>
//! cs-client observe  <id>
//! cs-client wait     <id>
//! cs-client done     <id>
//! cs-client fetch    <id>
//! cs-client run      <formula> --var topic="..."   # nucleate → tackle → wait → done → fetch
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use cosmon_client::config::ConfigOverrides;
use cosmon_client::{Client, ClientConfig};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "cs-client", version, about = "Thin cosmon client over HTTPS")]
struct Cli {
    /// Remote cosmon-saas server URL (e.g. https://cosmon-demo.democorp.dev).
    #[arg(long, global = true)]
    server: Option<String>,

    /// API key presented in the `X-API-Key` header.
    #[arg(long, global = true)]
    api_key: Option<String>,

    /// Local directory for downloaded artifacts.
    #[arg(long, global = true)]
    artifacts_dir: Option<PathBuf>,

    /// Emit JSON instead of human-readable output where applicable.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Ping the server's unauthenticated /healthz endpoint.
    Healthz,
    /// Create a molecule from a named formula.
    Nucleate {
        formula: String,
        /// `key=value` pairs repeated as needed.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        /// Optional tag (repeatable).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Optional blocker molecule id (repeatable).
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
    },
    /// Dispatch a worker onto a pending molecule.
    Tackle { id: String },
    /// Inspect a molecule's current state.
    Observe { id: String },
    /// Block until a molecule reaches a terminal state.
    Wait {
        id: String,
        /// Poll interval in seconds.
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// Close the pilot cycle for a completed molecule.
    Done { id: String },
    /// Download the molecule's artifact tarball and unpack it locally.
    Fetch { id: String },
    /// List artifacts available for a molecule (no download).
    List { id: String },
    /// Full pilot cycle: nucleate → tackle → wait → done → fetch.
    Run {
        formula: String,
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("COSMON_CLIENT_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let cfg = ClientConfig::load(ConfigOverrides {
        server: cli.server.clone(),
        api_key: cli.api_key.clone(),
        artifacts_dir: cli.artifacts_dir.clone(),
    })?;
    let client = Client::new(&cfg)?;
    let json = cli.json;

    match cli.cmd {
        Cmd::Healthz => {
            let body = client.healthz().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Cmd::Nucleate {
            formula,
            vars,
            tags,
            blocked_by,
        } => {
            let variables = parse_vars(&vars)?;
            let resp = client
                .nucleate(&formula, &variables, &tags, &blocked_by)
                .await?;
            render_json_or(json, &serde_json::to_value(&resp.extra)?, || {
                if let Some(id) = resp.molecule_id.as_deref() {
                    println!("nucleated: {id}");
                }
            });
        }
        Cmd::Tackle { id } => {
            let body = client.tackle(&id).await?;
            render_json_or(json, &body, || println!("tackled: {id}"));
        }
        Cmd::Observe { id } => {
            let state = client.observe(&id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state.extra)?);
            } else {
                render_state(&state);
            }
        }
        Cmd::Wait { id, interval } => {
            let state = client.wait(&id, Duration::from_secs(interval)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state.extra)?);
            } else {
                render_state(&state);
            }
        }
        Cmd::Done { id } => {
            let body = client.done(&id).await?;
            render_json_or(json, &body, || println!("done: {id}"));
        }
        Cmd::List { id } => {
            let listing = client.list_artifacts(&id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&listing)?);
            } else {
                println!("molecule: {}", listing.molecule_id);
                for entry in &listing.files {
                    println!("  {:<32} {:>8} B", entry.path, entry.bytes);
                }
            }
        }
        Cmd::Fetch { id } => {
            let dest = client.fetch_artifacts(&id, &cfg.artifacts_dir).await?;
            println!("artifacts: {}", dest.display());
        }
        Cmd::Run {
            formula,
            vars,
            tags,
            interval,
        } => {
            run_full_cycle(&client, &cfg, &formula, &vars, &tags, interval, json).await?;
        }
    }
    Ok(())
}

fn parse_vars(pairs: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--var expects KEY=VALUE, got {pair:?}"))?;
        out.insert(k.to_owned(), v.to_owned());
    }
    Ok(out)
}

fn render_json_or(json: bool, body: &serde_json::Value, fallback: impl FnOnce()) {
    if json {
        match serde_json::to_string_pretty(body) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to render JSON: {e}"),
        }
    } else {
        fallback();
    }
}

fn render_state(state: &cosmon_client::MoleculeState) {
    println!(
        "id:      {}\nformula: {}\nstatus:  {}\nstep:    {}/{}\nworker:  {}",
        state.id.as_deref().unwrap_or("?"),
        state.formula.as_deref().unwrap_or("?"),
        state.status.as_deref().unwrap_or("?"),
        state.current_step.map_or("?".into(), |n| n.to_string()),
        state.total_steps.map_or("?".into(), |n| n.to_string()),
        state.worker.as_deref().unwrap_or("-"),
    );
}

async fn run_full_cycle(
    client: &Client,
    cfg: &ClientConfig,
    formula: &str,
    vars: &[String],
    tags: &[String],
    interval: u64,
    json: bool,
) -> anyhow::Result<()> {
    let variables = parse_vars(vars)?;
    let nuc = client.nucleate(formula, &variables, tags, &[]).await?;
    let id = nuc
        .molecule_id
        .or_else(|| {
            nuc.extra
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| anyhow::anyhow!("server did not return a molecule id"))?;
    if !json {
        println!("▶ nucleated {id}");
    }
    client.tackle(&id).await?;
    if !json {
        println!("▶ tackled   {id}");
    }
    let final_state = client.wait(&id, Duration::from_secs(interval)).await?;
    if !json {
        println!(
            "▶ terminal  {id} ({})",
            final_state.status.as_deref().unwrap_or("?")
        );
    }
    client.done(&id).await?;
    if !json {
        println!("▶ done      {id}");
    }
    let dest = client.fetch_artifacts(&id, &cfg.artifacts_dir).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "molecule_id": id,
                "final_status": final_state.status,
                "artifacts_dir": dest,
            })
        );
    } else {
        println!("▶ artifacts {}", dest.display());
    }
    Ok(())
}
