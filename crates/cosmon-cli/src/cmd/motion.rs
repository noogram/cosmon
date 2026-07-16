// SPDX-License-Identifier: AGPL-3.0-only

//! `cs motion` — "molécules en mouvement" across the local galaxy cluster.
//!
//! A terminal mirror of the `/motion` HTTP endpoint served by `cs-api`.
//! The CLI wraps [`cosmon_api::motion::aggregate_motion`] so the exact
//! same filesystem scan backs every surface (CLI + Mac pilot + iOS pilot).
//!
//! Three modes:
//!
//! - `cs motion` — one-shot colored table (default).
//! - `cs motion --json` — machine-readable NDJSON (one object with every
//!   section; agent-first).
//! - `cs motion --watch` — re-render the table every 3 s (same polling
//!   cadence as the pilots). Ctrl-C to quit.
//!
//! The CLI does **not** open an HTTP connection. It reads the local
//! filesystem under `$HOME/galaxies` (override with `--galaxies-root`),
//! reusing the same aggregation logic as the endpoint — so the two
//! surfaces can never drift.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use colored::Colorize;
use serde_json::Value;

use super::Context;

/// Arguments for the `motion` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Time window for "recent" sections (e.g. `5m`, `15m`, `1h`).
    #[arg(long, default_value = "15m")]
    pub window: String,

    /// Restrict to these galaxies (comma-separated). Default: scan all.
    #[arg(long)]
    pub galaxies: Option<String>,

    /// Restrict to these sections (comma-separated among
    /// `workers,molecules,commits,whispers,sparks`). Default: all.
    #[arg(long)]
    pub include: Option<String>,

    /// Override the galaxies root. Default: `$HOME/galaxies`.
    #[arg(long, value_name = "PATH")]
    pub galaxies_root: Option<PathBuf>,

    /// Re-render every 3 seconds. Ctrl-C to quit.
    #[arg(long)]
    pub watch: bool,

    /// Emit NDJSON instead of the colored table. Overrides the global
    /// `--json` flag so scripts can be explicit.
    #[arg(long)]
    pub json: bool,
}

/// Entry point for `cs motion`.
///
/// # Errors
/// Returns an error if the galaxies root cannot be scanned.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let root = args
        .galaxies_root
        .clone()
        .unwrap_or_else(cosmon_api::default_galaxies_root);
    let json_mode = args.json || ctx.json;

    if args.watch {
        run_watch(&root, args, json_mode)
    } else {
        run_once(&root, args, json_mode)
    }
}

fn run_once(root: &std::path::Path, args: &Args, json_mode: bool) -> anyhow::Result<()> {
    let value = cosmon_api::motion::aggregate_motion(
        root,
        Some(args.window.as_str()),
        args.galaxies.as_deref(),
        args.include.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!("{}", e.message))?;
    if json_mode {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        render_table(&value, &mut io::stdout())?;
    }
    Ok(())
}

fn run_watch(root: &std::path::Path, args: &Args, json_mode: bool) -> anyhow::Result<()> {
    let tick = Duration::from_secs(3);
    loop {
        let value = cosmon_api::motion::aggregate_motion(
            root,
            Some(args.window.as_str()),
            args.galaxies.as_deref(),
            args.include.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!("{}", e.message))?;

        // Clear + home so the table redraws in place without scrolling.
        print!("\x1b[2J\x1b[H");
        if json_mode {
            println!("{}", serde_json::to_string(&value)?);
        } else {
            render_table(&value, &mut io::stdout())?;
        }
        io::stdout().flush()?;
        thread::sleep(tick);
    }
}

fn render_table(value: &Value, out: &mut dyn io::Write) -> io::Result<()> {
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let window = value.get("window").and_then(Value::as_str).unwrap_or("-");
    let scanned: Vec<String> = value
        .get("galaxies_scanned")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    writeln!(
        out,
        "{}  {}  {}",
        "molécules en mouvement".bold(),
        format!("— {window} window").dimmed(),
        format!("as of {timestamp}").dimmed()
    )?;
    if scanned.is_empty() {
        writeln!(out, "{}", "aucune galaxy détectée sous ~/galaxies".yellow())?;
    } else {
        writeln!(out, "{} {}", "galaxies:".dimmed(), scanned.join(", "))?;
    }
    writeln!(out)?;

    render_section(out, value, "workers", "Workers", render_worker_row)?;
    render_section(
        out,
        value,
        "running_molecules",
        "Running molecules",
        render_running_row,
    )?;
    render_section(
        out,
        value,
        "recent_git_commits",
        "Recent git commits",
        render_commit_row,
    )?;
    render_section(
        out,
        value,
        "recent_whispers",
        "Recent whispers",
        render_whisper_row,
    )?;
    render_section(
        out,
        value,
        "recent_sparks",
        "Recent sparks",
        render_spark_row,
    )?;
    Ok(())
}

fn render_section(
    out: &mut dyn io::Write,
    value: &Value,
    key: &str,
    header: &str,
    mut row_fn: impl FnMut(&mut dyn io::Write, &Value) -> io::Result<()>,
) -> io::Result<()> {
    let empty: Vec<Value> = Vec::new();
    let arr = value.get(key).and_then(Value::as_array).unwrap_or(&empty);
    writeln!(out, "{} ({})", header.bold(), arr.len())?;
    if arr.is_empty() {
        writeln!(out, "  {}", "—".dimmed())?;
    } else {
        for row in arr {
            row_fn(out, row)?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn render_worker_row(out: &mut dyn io::Write, v: &Value) -> io::Result<()> {
    let name = v.get("name").and_then(Value::as_str).unwrap_or("?");
    let galaxy = v.get("galaxy").and_then(Value::as_str).unwrap_or("-");
    let molecule = v
        .get("molecule_id")
        .and_then(Value::as_str)
        .unwrap_or("(none)");
    let status = v.get("status").and_then(Value::as_str).unwrap_or("?");
    let heartbeat = v
        .get("last_heartbeat")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let status_color = match status {
        "active" | "healthy" | "running" => status.green(),
        "stopped" | "stale" => status.yellow(),
        "error" | "diverged" => status.red(),
        _ => status.normal(),
    };
    writeln!(
        out,
        "  {} {} {} {} {}",
        name.cyan(),
        format!("[{galaxy}]").dimmed(),
        status_color,
        molecule,
        format!("♥ {heartbeat}").dimmed()
    )
}

fn render_running_row(out: &mut dyn io::Write, v: &Value) -> io::Result<()> {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
    let galaxy = v.get("galaxy").and_then(Value::as_str).unwrap_or("-");
    let step = v.get("current_step").and_then(Value::as_u64);
    let total = v.get("total_steps").and_then(Value::as_u64);
    let last_evolve = v
        .get("last_evolve_at")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let topic = v.get("topic_preview").and_then(Value::as_str).unwrap_or("");
    let step_str = match (step, total) {
        (Some(s), Some(t)) => format!("step {s}/{t}"),
        (Some(s), None) => format!("step {s}"),
        _ => "step ?".to_owned(),
    };
    writeln!(
        out,
        "  {} {} {} {}  {}",
        id.cyan(),
        format!("[{galaxy}]").dimmed(),
        step_str.yellow(),
        format!("@ {last_evolve}").dimmed(),
        topic
    )
}

fn render_commit_row(out: &mut dyn io::Write, v: &Value) -> io::Result<()> {
    let galaxy = v.get("galaxy").and_then(Value::as_str).unwrap_or("-");
    let sha = v.get("sha").and_then(Value::as_str).unwrap_or("?");
    let subject = v.get("subject").and_then(Value::as_str).unwrap_or("");
    let ts = v.get("timestamp").and_then(Value::as_str).unwrap_or("-");
    let author = v.get("author").and_then(Value::as_str).unwrap_or("-");
    writeln!(
        out,
        "  {} {} {} {}  {}",
        sha.yellow(),
        format!("[{galaxy}]").dimmed(),
        format!("@ {ts}").dimmed(),
        format!("<{author}>").dimmed(),
        subject
    )
}

fn render_whisper_row(out: &mut dyn io::Write, v: &Value) -> io::Result<()> {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
    let galaxy = v.get("galaxy").and_then(Value::as_str).unwrap_or("-");
    let sender = v
        .get("sender_nucleon_id")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let received = v.get("received_at").and_then(Value::as_str).unwrap_or("-");
    let body = v.get("body_preview").and_then(Value::as_str).unwrap_or("");
    writeln!(
        out,
        "  {} {} {} {}  {}",
        id.cyan(),
        format!("[{galaxy}]").dimmed(),
        format!("<{sender}>").dimmed(),
        format!("@ {received}").dimmed(),
        body
    )
}

fn render_spark_row(out: &mut dyn io::Write, v: &Value) -> io::Result<()> {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
    let galaxy = v.get("galaxy").and_then(Value::as_str).unwrap_or("-");
    let created = v.get("created_at").and_then(Value::as_str).unwrap_or("-");
    let topic = v.get("topic_preview").and_then(Value::as_str).unwrap_or("");
    writeln!(
        out,
        "  {} {} {}  {}",
        id.cyan(),
        format!("[{galaxy}]").dimmed(),
        format!("@ {created}").dimmed(),
        topic
    )
}
