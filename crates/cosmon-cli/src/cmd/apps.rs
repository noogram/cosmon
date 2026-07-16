// SPDX-License-Identifier: AGPL-3.0-only

//! `cs apps` — operator view over the cluster's HTTP-on-Tailscale daemons.
//!
//! Subcommands:
//!
//! - `cs apps health` — ping every known daemon's `/v1/health` endpoint
//!   and report green/red. Output mirrors `cs ensemble`'s table style for
//!   visual consistency with the rest of the cosmon TUI.
//!
//! The list of daemons is hardcoded here for v0.1 (verdict, mural,
//! cosmon-app); a future molecule will move it to `cluster.toml`
//! (ADR-066) so new galaxies can register theirs declaratively.

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use serde::Serialize;

use super::Context;

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: AppsCommand,
}

#[derive(Subcommand)]
enum AppsCommand {
    /// Ping the `/v1/health` endpoint of every known cluster daemon.
    Health(HealthArgs),
}

#[derive(ClapArgs)]
pub struct HealthArgs {
    /// Override the default cluster host (Tailscale `MagicDNS` name).
    /// Defaults to `host.example`.
    #[arg(long)]
    host: Option<String>,
    /// Per-request timeout (seconds). Defaults to 2.0.
    #[arg(long, default_value_t = 2.0)]
    timeout: f64,
}

/// Default cluster host. Matches `HTTPTransportConfig.defaultHost` on
/// the Swift side.
const DEFAULT_HOST: &str = "host.example";

/// Hardcoded daemon table. Order matches the human reading order in
/// `docs/runbook/http-on-tailscale.md` §3.
const DAEMONS: &[Daemon<'static>] = &[
    Daemon {
        name: "mural-daemon",
        galaxy: "mailroom",
        port: 8788,
        optional: true,
    },
    Daemon {
        name: "verdict-daemon",
        galaxy: "verdict",
        port: 8789,
        optional: true,
    },
    Daemon {
        name: "cosmon-app",
        galaxy: "cosmon",
        port: 8790,
        optional: true,
    },
];

#[derive(Debug, Clone, Copy)]
struct Daemon<'a> {
    name: &'a str,
    galaxy: &'a str,
    port: u16,
    /// If `optional`, a missing daemon is reported as `idle` rather than
    /// failing the overall command. Every daemon is optional in v0.1.
    optional: bool,
}

#[derive(Debug, Serialize)]
struct HealthRow {
    daemon: String,
    galaxy: String,
    url: String,
    status: String,
    latency_ms: Option<u128>,
    detail: Option<String>,
}

pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    match &args.command {
        AppsCommand::Health(h) => run_health(ctx, h),
    }
}

fn run_health(ctx: &Context, args: &HealthArgs) -> Result<()> {
    let host = args
        .host
        .clone()
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    let timeout = Duration::from_secs_f64(args.timeout.max(0.1));

    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))?;

    let mut rows: Vec<HealthRow> = Vec::with_capacity(DAEMONS.len());
    for d in DAEMONS {
        let url = format!("http://{host}:{port}/v1/health", port = d.port);
        let started = Instant::now();
        match client.get(&url).send() {
            Ok(resp) => {
                let latency = started.elapsed().as_millis();
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                let (slug, detail) = if status == 200 {
                    ("ok", body)
                } else {
                    ("error", body)
                };
                rows.push(HealthRow {
                    daemon: d.name.to_string(),
                    galaxy: d.galaxy.to_string(),
                    url,
                    status: format!("{slug} ({status})"),
                    latency_ms: Some(latency),
                    detail: if detail.is_empty() {
                        None
                    } else {
                        Some(detail)
                    },
                });
            }
            Err(e) => {
                let slug = if d.optional { "idle" } else { "down" };
                rows.push(HealthRow {
                    daemon: d.name.to_string(),
                    galaxy: d.galaxy.to_string(),
                    url,
                    status: slug.to_string(),
                    latency_ms: None,
                    detail: Some(e.to_string()),
                });
            }
        }
    }

    if ctx.json {
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }

    println!(
        "{:<18} {:<14} {:<48} {:<14} {:>8}",
        "DAEMON", "GALAXY", "URL", "STATUS", "LATENCY"
    );
    println!("{}", "-".repeat(108));
    for r in &rows {
        let lat = r
            .latency_ms
            .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms"));
        println!(
            "{:<18} {:<14} {:<48} {:<14} {:>8}",
            r.daemon, r.galaxy, r.url, r.status, lat
        );
        if let Some(d) = &r.detail {
            if !d.trim().is_empty() {
                println!("  └─ {}", truncate(d, 96));
            }
        }
    }

    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out = s.chars().take(n).collect::<String>();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_appends_ellipsis() {
        assert_eq!(truncate("0123456789abcdef", 5), "01234…");
    }

    #[test]
    fn daemons_table_is_non_empty_and_unique_ports() {
        assert!(!DAEMONS.is_empty());
        let mut ports: Vec<u16> = DAEMONS.iter().map(|d| d.port).collect();
        ports.sort_unstable();
        let n = ports.len();
        ports.dedup();
        assert_eq!(ports.len(), n, "daemon ports must be unique");
    }
}
