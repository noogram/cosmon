// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tail` — live reader over `events.jsonl`, fleet-local by default.
//!
//! Minimum-entropy inter-session primitive: one line per event, newest-last.
//! Uses `notify` for follow-mode (no polling). Format:
//!
//! ```text
//! <ts> | <galaxy> | <mol_id> | <variant> | <summary>
//! ```
//!
//! Default is fleet-local (`.cosmon/state/events.jsonl` via walk-up
//! discovery). `--all-galaxies` opts into cross-galaxy aggregation by
//! scanning `$COSMON_CLUSTER_ROOT` (fallback: `$HOME/galaxies`) — an
//! explicit reach, never implicit (syzygie protocol: citation, not
//! subscription).
//!
//! The upstream `cs events tail` is scoped to `EventV2`-envelope parsing
//! and the single fleet-local log; `cs tail` trades that strictness for
//! multi-galaxy reach and `notify`-driven follow.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::Context;

/// Arguments for the `cs tail` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Scan every project under `$COSMON_CLUSTER_ROOT`.
    /// Opt-in — cross-project reach is never implicit.
    #[arg(long)]
    all_galaxies: bool,

    /// Stay attached and stream new events via `notify`.
    #[arg(short = 'f', long)]
    follow: bool,

    /// Only show events at or after this timestamp.
    /// Accepts ISO-8601 (`2026-04-24T12:00:00Z`) or relative
    /// (`-5m`, `-1h`, `-2d`). `allow_hyphen_values` lets the
    /// relative form be written without `--since=`.
    #[arg(long, allow_hyphen_values = true)]
    since: Option<String>,

    /// Only show events whose `type` tag matches (e.g. `molecule_nucleated`).
    #[arg(long)]
    kind: Option<String>,

    /// Number of most-recent lines to print before follow (like `tail -n`).
    #[arg(short = 'n', long, default_value_t = 20)]
    tail: usize,

    /// Override the cluster root used by `--all-galaxies`.
    #[arg(long)]
    cluster_root: Option<PathBuf>,
}

/// Execute the `tail` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let since = parse_since(args.since.as_deref())?;
    let sources = resolve_sources(ctx, args);

    if sources.is_empty() {
        eprintln!("cs tail: no events.jsonl found.");
        return Ok(());
    }

    // Initial snapshot: collect the last N events across every source,
    // sort by timestamp, apply filters, print.
    let mut buf: Vec<(String, Line)> = Vec::new();
    let mut offsets: Vec<(PathBuf, String, u64)> = Vec::with_capacity(sources.len());
    for (path, galaxy) in &sources {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        offsets.push((path.clone(), galaxy.clone(), content.len() as u64));
        for raw in content.lines().rev().take(args.tail.max(1) * 4) {
            if let Some(line) = Line::parse(raw, galaxy) {
                buf.push((raw.to_owned(), line));
            }
        }
    }
    buf.retain(|(_, l)| accept(l, since.as_ref(), args.kind.as_deref()));
    buf.sort_by_key(|a| a.1.ts);
    let start = buf.len().saturating_sub(args.tail);
    let mut out = std::io::stdout().lock();
    for (raw, line) in &buf[start..] {
        emit(&mut out, line, raw.as_str(), ctx.json)?;
    }
    out.flush()?;

    if !args.follow {
        return Ok(());
    }

    follow(&offsets, since.as_ref(), args.kind.as_deref(), ctx.json)
}

/// A parsed event line, trimmed to the columns we display.
#[derive(Debug, Clone)]
struct Line {
    ts: DateTime<Utc>,
    galaxy: String,
    mol_id: String,
    variant: String,
    summary: String,
}

impl Line {
    /// Parse a single `events.jsonl` row. Unparseable lines return `None`
    /// (the caller silently skips them — the log is append-only and
    /// in-flight writes occasionally produce a truncated last line).
    fn parse(raw: &str, galaxy: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let ts_str = v.get("timestamp").and_then(|t| t.as_str())?;
        let ts = DateTime::parse_from_rfc3339(ts_str)
            .ok()?
            .with_timezone(&Utc);
        let variant = v
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let mol_id = v
            .get("molecule_id")
            .or_else(|| v.get("molecule"))
            .and_then(|m| m.as_str())
            .unwrap_or("-")
            .to_owned();
        Some(Line {
            ts,
            galaxy: galaxy.to_owned(),
            mol_id,
            variant,
            summary: summarize(&v),
        })
    }
}

/// Extract a one-glance summary from an event payload.
///
/// We peek at a small set of well-known fields (`reason`, `formula_id`,
/// `session_name`, `status`, `bytes`, `delta`) rather than serialising the whole
/// value — the goal is a compact, skim-friendly right column, not a
/// replacement for `cs events query`.
fn summarize(v: &serde_json::Value) -> String {
    const KEYS: &[&str] = &[
        "reason",
        "formula_id",
        "session_name",
        "status",
        "role",
        "step",
        "worker_id",
    ];
    let mut parts: Vec<String> = Vec::new();
    for k in KEYS {
        if let Some(val) = v.get(*k) {
            if let Some(s) = val.as_str() {
                parts.push(format!("{k}={s}"));
            } else if let Some(n) = val.as_i64() {
                parts.push(format!("{k}={n}"));
            }
        }
        if parts.len() >= 2 {
            break;
        }
    }
    parts.join(" ")
}

/// Decide whether a parsed line passes the `--since` and `--kind` filters.
fn accept(line: &Line, since: Option<&DateTime<Utc>>, kind: Option<&str>) -> bool {
    if let Some(s) = since {
        if line.ts < *s {
            return false;
        }
    }
    if let Some(k) = kind {
        if line.variant != k {
            return false;
        }
    }
    true
}

/// Emit a line in either column-delimited (default) or JSON (`--json`) form.
fn emit<W: Write>(out: &mut W, line: &Line, raw: &str, json: bool) -> anyhow::Result<()> {
    if json {
        writeln!(out, "{raw}")?;
    } else {
        writeln!(
            out,
            "{} | {} | {} | {} | {}",
            line.ts.format("%Y-%m-%dT%H:%M:%SZ"),
            line.galaxy,
            line.mol_id,
            line.variant,
            line.summary,
        )?;
    }
    Ok(())
}

/// Resolve the set of `(events.jsonl, galaxy_label)` sources to watch.
///
/// Single fleet-local source by default; every galaxy under the cluster
/// root when `--all-galaxies` is set. Gracefully degrades to fleet-local
/// when the cluster root is absent so `cs tail --all-galaxies` never
/// hard-fails in a fresh environment.
fn resolve_sources(ctx: &Context, args: &Args) -> Vec<(PathBuf, String)> {
    let _ = ctx; // reserved for future `--config`-aware resolution.
    if !args.all_galaxies {
        let path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");
        let galaxy = galaxy_label_for(&path);
        return if path.exists() {
            vec![(path, galaxy)]
        } else {
            Vec::new()
        };
    }

    let root = args
        .cluster_root
        .clone()
        .or_else(|| std::env::var_os("COSMON_CLUSTER_ROOT").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("galaxies")))
        .unwrap_or_else(|| PathBuf::from("."));

    let Ok(iter) = std::fs::read_dir(&root) else {
        eprintln!(
            "cs tail: cluster root {} unreachable — falling back to fleet-local",
            root.display()
        );
        return resolve_sources(
            ctx,
            &Args {
                all_galaxies: false,
                follow: args.follow,
                since: args.since.clone(),
                kind: args.kind.clone(),
                tail: args.tail,
                cluster_root: None,
            },
        );
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let candidate = entry
            .path()
            .join(".cosmon")
            .join("state")
            .join("events.jsonl");
        if candidate.exists() {
            let galaxy = entry.file_name().to_string_lossy().into_owned();
            out.push((candidate, galaxy));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Derive the galaxy label for a single fleet-local source.
///
/// Walks up from `events.jsonl` past `state/` and `.cosmon/` to the
/// project root; falls back to `"local"` when the directory shape is
/// unexpected.
fn galaxy_label_for(events_path: &Path) -> String {
    events_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map_or_else(|| "local".to_owned(), |n| n.to_string_lossy().into_owned())
}

/// Block on `notify` events and stream new lines as they land.
///
/// Creates one watcher per source, all forwarding into a shared mpsc
/// channel. When a modify event fires for a known source we re-open
/// the file, seek to the last known offset, and emit every new line
/// that passes the filters.
fn follow(
    sources: &[(PathBuf, String, u64)],
    since: Option<&DateTime<Utc>>,
    kind: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let mut watchers: Vec<RecommendedWatcher> = Vec::with_capacity(sources.len());

    for (path, _, _) in sources {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let tx = tx.clone();
        let target = path.clone();
        let mut watcher: RecommendedWatcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        for p in &ev.paths {
                            if p.ends_with("events.jsonl")
                                && p.file_name() == target.file_name()
                                && p == &target
                            {
                                let _ = tx.send(target.clone());
                                break;
                            }
                        }
                    }
                }
            },
            NotifyConfig::default(),
        )?;
        watcher.watch(parent, RecursiveMode::NonRecursive)?;
        watchers.push(watcher);
    }

    // Per-path cursor into the log. We re-stat and read only the new
    // suffix, so a growing file never forces a full re-scan.
    let mut cursors: std::collections::HashMap<PathBuf, (String, u64)> =
        std::collections::HashMap::with_capacity(sources.len());
    for (path, galaxy, offset) in sources {
        cursors.insert(path.clone(), (galaxy.clone(), *offset));
    }

    let mut out = std::io::stdout().lock();
    let deadline = Instant::now() + Duration::from_secs(u64::MAX / 2);
    while Instant::now() < deadline {
        let Ok(path) = rx.recv() else { break };
        let Some((galaxy, offset)) = cursors.get_mut(&path) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() <= *offset {
            // Truncation / GC — reset cursor so we don't re-emit the whole file.
            *offset = meta.len();
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let start = usize::try_from(*offset).unwrap_or(usize::MAX);
        let suffix: &str = content.get(start..).unwrap_or("");
        for raw in suffix.lines() {
            if raw.trim().is_empty() {
                continue;
            }
            if let Some(line) = Line::parse(raw, galaxy) {
                if accept(&line, since, kind) {
                    emit(&mut out, &line, raw, json)?;
                }
            }
        }
        out.flush()?;
        *offset = content.len() as u64;
    }
    Ok(())
}

/// Parse `--since` into a `DateTime<Utc>`.
///
/// Accepts ISO-8601 (`2026-04-24T12:00:00Z`) and a small relative grammar
/// (`-5m`, `-1h`, `-2d`). Anything else is a hard error — we want silent
/// acceptance of garbage to be a bug, not a feature.
fn parse_since(raw: Option<&str>) -> anyhow::Result<Option<DateTime<Utc>>> {
    let Some(s) = raw else { return Ok(None) };
    if let Some(rest) = s.strip_prefix('-') {
        let (num, unit) = rest.split_at(
            rest.find(|c: char| !c.is_ascii_digit())
                .ok_or_else(|| anyhow::anyhow!("invalid --since: expected unit in `{s}`"))?,
        );
        let n: i64 = num
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid --since: not a number `{num}`"))?;
        let delta = match unit {
            "s" => chrono::Duration::seconds(n),
            "m" => chrono::Duration::minutes(n),
            "h" => chrono::Duration::hours(n),
            "d" => chrono::Duration::days(n),
            _ => anyhow::bail!("invalid --since unit `{unit}` (expected s/m/h/d)"),
        };
        return Ok(Some(Utc::now() - delta));
    }
    DateTime::parse_from_rfc3339(s)
        .map(|dt| Some(dt.with_timezone(&Utc)))
        .map_err(|e| anyhow::anyhow!("invalid --since timestamp `{s}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_handles_relative_and_iso() {
        assert!(parse_since(None).unwrap().is_none());
        assert!(parse_since(Some("-5m")).unwrap().is_some());
        assert!(parse_since(Some("-2h")).unwrap().is_some());
        assert!(parse_since(Some("2026-04-24T12:00:00Z")).unwrap().is_some());
        assert!(parse_since(Some("not-a-date")).is_err());
        assert!(parse_since(Some("-5x")).is_err());
    }

    #[test]
    fn parse_line_extracts_columns() {
        let raw = r#"{"seq":0,"mol_seq":0,"timestamp":"2026-04-20T10:13:37.570883Z","type":"molecule_nucleated","molecule_id":"delib-20260420-74b8","formula_id":"deep-think"}"#;
        let line = Line::parse(raw, "cosmon").expect("parse");
        assert_eq!(line.galaxy, "cosmon");
        assert_eq!(line.mol_id, "delib-20260420-74b8");
        assert_eq!(line.variant, "molecule_nucleated");
        assert!(line.summary.contains("formula_id=deep-think"));
    }

    #[test]
    fn parse_line_tolerates_unknown_shapes() {
        // Missing type tag — still parses, variant falls back to "unknown".
        let raw = r#"{"timestamp":"2026-04-20T10:13:37.570883Z","molecule":"x-1"}"#;
        let line = Line::parse(raw, "g").unwrap();
        assert_eq!(line.mol_id, "x-1");
        assert_eq!(line.variant, "unknown");

        // Truncated line — drop silently.
        assert!(Line::parse(r#"{"timestamp":"not"#, "g").is_none());
    }

    #[test]
    fn accept_filters_by_since_and_kind() {
        let line = Line {
            ts: DateTime::parse_from_rfc3339("2026-04-20T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            galaxy: "g".into(),
            mol_id: "m".into(),
            variant: "molecule_nucleated".into(),
            summary: String::new(),
        };
        assert!(accept(&line, None, None));
        assert!(accept(&line, None, Some("molecule_nucleated")));
        assert!(!accept(&line, None, Some("other_kind")));

        let cutoff = DateTime::parse_from_rfc3339("2026-04-21T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!accept(&line, Some(&cutoff), None));
    }

    #[test]
    fn emit_plain_and_json() {
        let line = Line {
            ts: DateTime::parse_from_rfc3339("2026-04-20T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            galaxy: "cosmon".into(),
            mol_id: "task-1".into(),
            variant: "molecule_completed".into(),
            summary: "reason=ok".into(),
        };
        let mut out = Vec::new();
        emit(&mut out, &line, "raw", false).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("cosmon | task-1 | molecule_completed | reason=ok"));

        let mut out = Vec::new();
        emit(&mut out, &line, "raw", true).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "raw\n");
    }
}
