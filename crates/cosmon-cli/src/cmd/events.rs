// SPDX-License-Identifier: AGPL-3.0-only

//! `cs events` — query and inspect the `EventV2` event log.
//!
//! Provides subcommands for tailing, filtering, validating, and
//! summarising the append-only `events.jsonl` sensor record that
//! powers post-hoc fleet replay.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::{Envelope, EventV2, Seq};
use cosmon_state::event_log;

use super::Context;

/// Inspect and query the `EventV2` event log.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    sub: Sub,
}

#[derive(clap::Subcommand)]
enum Sub {
    /// Live-stream new events as they arrive.
    Tail(TailArgs),
    /// Filter events by variant and/or time range.
    Query(QueryArgs),
    /// Print per-variant event counts.
    Stats(StatsArgs),
    /// Validate the log for schema and sequencing invariants.
    Validate(ValidateArgs),
}

#[derive(clap::Args)]
struct TailArgs {
    /// Number of most-recent lines to show before following (like `tail -n`).
    #[arg(short = 'n', long, default_value_t = 10)]
    lines: usize,

    /// Follow the log (block and print new events as they appear).
    #[arg(short = 'f', long)]
    follow: bool,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct QueryArgs {
    /// Filter by event type (e.g. `molecule_nucleated`, `energy_tick`).
    #[arg(long)]
    kind: Option<String>,

    /// Only show events at or after this ISO8601 timestamp.
    #[arg(long)]
    since: Option<String>,

    /// Only show events at or before this ISO8601 timestamp.
    #[arg(long)]
    until: Option<String>,

    /// Maximum number of results to return.
    #[arg(long)]
    limit: Option<usize>,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct StatsArgs {
    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ValidateArgs {
    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<PathBuf>,
}

/// Execute the `events` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.sub {
        Sub::Tail(a) => run_tail(ctx, a),
        Sub::Query(a) => run_query(ctx, a),
        Sub::Stats(a) => run_stats(ctx, a),
        Sub::Validate(a) => run_validate(ctx, a),
    }
}

/// Resolve the `events.jsonl` path from the args or walk-up discovery.
fn events_path(ops_dir: Option<&PathBuf>) -> PathBuf {
    let state_dir = cosmon_filestore::resolve_state_dir(ops_dir.map(PathBuf::as_path));
    state_dir.join("events.jsonl")
}

/// Extract the serde `type` tag from an `EventV2` variant for display.
fn event_type_tag(event: &EventV2) -> &'static str {
    match event {
        EventV2::MoleculeNucleated { .. } => "molecule_nucleated",
        EventV2::MoleculeStatusChanged { .. } => "molecule_status_changed",
        EventV2::MoleculeStepCompleted { .. } => "molecule_step_completed",
        EventV2::MoleculeCompleted { .. } => "molecule_completed",
        EventV2::MoleculeCollapsed { .. } => "molecule_collapsed",
        EventV2::MoleculeStuck { .. } => "molecule_stuck",
        EventV2::DecaySpliced { .. } => "decay_spliced",
        EventV2::MergeDispatched { .. } => "merge_dispatched",
        EventV2::MergeCompleted { .. } => "merge_completed",
        EventV2::WorkerSpawned { .. } => "worker_spawned",
        EventV2::WorkerKilled { .. } => "worker_killed",
        EventV2::EnergyTick { .. } => "energy_tick",
        EventV2::WorkerHeartbeat { .. } => "worker_heartbeat",
        EventV2::Expired { .. } => "expired",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// tail
// ---------------------------------------------------------------------------

fn run_tail(_ctx: &Context, args: &TailArgs) -> anyhow::Result<()> {
    let path = events_path(args.ops_dir.as_ref());
    if !path.exists() {
        println!("No events log found at {}", path.display());
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(args.lines);
    let mut out = std::io::stdout().lock();
    for line in &lines[start..] {
        writeln!(out, "{line}")?;
    }

    if args.follow {
        // Simple follow: poll for new content.
        let mut offset = content.len() as u64;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let meta = std::fs::metadata(&path)?;
            let current_len = meta.len();
            if current_len > offset {
                let file = std::fs::File::open(&path)?;
                let mut reader = BufReader::new(file);
                std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(offset))?;
                for line in reader.lines() {
                    let line = line?;
                    if !line.trim().is_empty() {
                        writeln!(out, "{line}")?;
                    }
                }
                out.flush()?;
                offset = current_len;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

fn run_query(ctx: &Context, args: &QueryArgs) -> anyhow::Result<()> {
    let path = events_path(args.ops_dir.as_ref());
    if !path.exists() {
        if ctx.json {
            println!("[]");
        } else {
            println!("No events log found.");
        }
        return Ok(());
    }

    let since: Option<DateTime<Utc>> = args
        .since
        .as_ref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| anyhow::anyhow!("invalid --since timestamp: {e}"))
        })
        .transpose()?;

    let until: Option<DateTime<Utc>> = args
        .until
        .as_ref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| anyhow::anyhow!("invalid --until timestamp: {e}"))
        })
        .transpose()?;

    let envelopes = event_log::read_all(&path)?;
    let mut count = 0usize;
    let limit = args.limit.unwrap_or(usize::MAX);

    for env in &envelopes {
        if count >= limit {
            break;
        }
        // Filter by kind.
        if let Some(ref kind) = args.kind {
            if event_type_tag(&env.event) != kind.as_str() {
                continue;
            }
        }
        // Filter by time range.
        if let Some(ref s) = since {
            if env.timestamp < *s {
                continue;
            }
        }
        if let Some(ref u) = until {
            if env.timestamp > *u {
                continue;
            }
        }

        if ctx.json {
            println!(
                "{}",
                serde_json::to_string(env).unwrap_or_else(|_| "{}".to_owned())
            );
        } else {
            println!(
                "[{}] seq={} type={}",
                env.timestamp.format("%H:%M:%S"),
                env.seq,
                event_type_tag(&env.event),
            );
        }
        count += 1;
    }

    if !ctx.json && count == 0 {
        println!("No matching events.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// stats
// ---------------------------------------------------------------------------

fn run_stats(ctx: &Context, args: &StatsArgs) -> anyhow::Result<()> {
    let path = events_path(args.ops_dir.as_ref());
    if !path.exists() {
        if ctx.json {
            println!("{{}}");
        } else {
            println!("No events log found.");
        }
        return Ok(());
    }

    let envelopes = event_log::read_all(&path)?;
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut total = 0usize;
    let mut first_ts: Option<DateTime<Utc>> = None;
    let mut last_ts: Option<DateTime<Utc>> = None;

    for env in &envelopes {
        let tag = event_type_tag(&env.event);
        *counts.entry(tag).or_insert(0) += 1;
        total += 1;
        if first_ts.is_none() {
            first_ts = Some(env.timestamp);
        }
        last_ts = Some(env.timestamp);
    }

    if ctx.json {
        let out = serde_json::json!({
            "total": total,
            "first_timestamp": first_ts.map(|t| t.to_rfc3339()),
            "last_timestamp": last_ts.map(|t| t.to_rfc3339()),
            "counts": counts,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Event log: {total} total events");
        if let (Some(first), Some(last)) = (first_ts, last_ts) {
            println!(
                "  span: {} → {}",
                first.format("%Y-%m-%d %H:%M:%S UTC"),
                last.format("%Y-%m-%d %H:%M:%S UTC"),
            );
        }
        println!();
        for (tag, count) in &counts {
            println!("  {tag:30} {count:>6}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

fn run_validate(ctx: &Context, args: &ValidateArgs) -> anyhow::Result<()> {
    let path = events_path(args.ops_dir.as_ref());
    if !path.exists() {
        if ctx.json {
            println!(r#"{{"valid":true,"lines":0,"errors":[]}}"#);
        } else {
            println!("No events log found — nothing to validate.");
        }
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let mut errors: Vec<String> = Vec::new();
    let mut line_count = 0usize;
    let mut prev_seq: Option<Seq> = None;

    for (i, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        line_count += 1;
        let line_num = i + 1;

        match Envelope::from_line(line) {
            Ok(env) => {
                // Check monotone sequence.
                if let Some(prev) = prev_seq {
                    if env.seq <= prev {
                        errors.push(format!(
                            "line {line_num}: sequence {env_seq} is not strictly greater than previous {prev}",
                            env_seq = env.seq,
                        ));
                    }
                }
                prev_seq = Some(env.seq);
            }
            Err(e) => {
                errors.push(format!("line {line_num}: parse error: {e}"));
            }
        }
    }

    if ctx.json {
        let out = serde_json::json!({
            "valid": errors.is_empty(),
            "lines": line_count,
            "errors": errors,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if errors.is_empty() {
        println!("Valid: {line_count} lines, all sequences monotone.");
    } else {
        println!("INVALID: {line_count} lines, {} error(s):", errors.len());
        for e in &errors {
            println!("  - {e}");
        }
    }

    if !errors.is_empty() {
        std::process::exit(1);
    }

    Ok(())
}
