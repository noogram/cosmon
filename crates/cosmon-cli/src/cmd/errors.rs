// SPDX-License-Identifier: AGPL-3.0-only

//! `cs errors` — read-only aggregator over collapsed-molecule signals
//! drawn from `events.jsonl`.
//!
//! Cosmon already records every collapse to `events.jsonl`, but on its own no
//! surface aggregates them — so the operator re-reads logs by hand and the
//! `temp:hot` queue carries failures whose root-cause stays implicit. This
//! command answers, in one line: *what is breaking my fleet right now, and
//! which molecules are affected?*
//!
//! ## Filters
//!
//! ```text
//! cs errors                              top-N collapses, last 7 days
//! cs errors --since 24h                  shorter window
//! cs errors --since 2026-04-01T00:00:00Z absolute RFC-3339 cutoff
//! cs errors --kind worker_crashed        only one CollapseReason variant
//! cs errors --reason "build broke"       substring match on free-form text
//! cs errors --top 20                     more rows
//! cs errors --json                       NDJSON for scripting
//! ```
//!
//! ## Always-on indicators
//!
//! Two indicators are surfaced regardless of filtering:
//!
//! - `% structured` — share of in-window collapses with a typed
//!   `CollapseReason` (anything other than `Other`). Climbing this
//!   number means the collapse vocabulary covers reality and operators
//!   can act without re-parsing prose.
//! - per-variant counts — the distribution that lets a quick eyeball
//!   answer "is one bucket exploding?".
//!
//! Reads only — never writes. The events log is the source of truth.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use cosmon_core::event_v2::{CollapseReason, EventV2};
use cosmon_state::event_log;
use serde::Serialize;

use super::Context;

/// Arguments for `cs errors`.
#[derive(clap::Args)]
pub struct Args {
    /// Time window — accepts a relative duration (`<N>d`, `<N>h`,
    /// `<N>m`, `<N>s`) or an RFC-3339 absolute timestamp. Defaults to
    /// 7 days.
    #[arg(long, default_value = "7d")]
    pub since: String,

    /// Filter to one `CollapseReason` variant. Accepts the on-wire
    /// strings: `worker_crashed`, `gate_failed`, `blocker_stuck`,
    /// `manual_abort`, `resource_exhausted`. Any other value is treated
    /// as a substring match on the free-form `Other` payload.
    #[arg(long, value_name = "VARIANT")]
    pub kind: Option<String>,

    /// Substring match on the free-form `reason` text. Case-sensitive.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,

    /// Maximum number of variant rows to display in the summary.
    #[arg(long, default_value_t = 10)]
    pub top: usize,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    pub ops_dir: Option<PathBuf>,

    /// Emit JSON instead of the tabular summary. The global `--json`
    /// flag also enables this.
    #[arg(long)]
    pub json: bool,
}

/// One observed collapse, before aggregation. Public-shape via
/// `--json` so external tooling can re-aggregate without re-parsing.
#[derive(Debug, Clone, Serialize)]
pub struct CollapseRecord {
    /// Affected molecule.
    pub molecule_id: String,
    /// `CollapseReason` variant or the wire string for `Other`.
    pub kind: String,
    /// `true` iff the variant is one of the five typed shapes (i.e.
    /// not `Other`). Drives the `% structured` indicator.
    pub structured: bool,
    /// Free-form operator-supplied reason text.
    pub reason: String,
    /// Wall-clock UTC timestamp of the collapse event.
    pub timestamp: DateTime<Utc>,
}

/// Per-variant aggregate row.
#[derive(Debug, Clone, Serialize)]
struct KindRow {
    kind: String,
    count: u64,
    structured: bool,
    sample_molecules: Vec<String>,
}

/// JSON-shaped summary for `cs errors --json`. Stable shape — clients
/// can re-render without re-aggregating.
#[derive(Debug, Serialize)]
struct Summary {
    window_since: String,
    total: u64,
    structured: u64,
    other: u64,
    structured_pct: f64,
    by_kind: Vec<KindRow>,
}

/// Execute `cs errors`.
///
/// # Errors
///
/// Returns the underlying I/O error if `events.jsonl` exists but
/// cannot be read; a missing log is treated as "no events yet".
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let cutoff = parse_since(&args.since)?;
    let path = resolve_events_path(args.ops_dir.as_ref());

    let envelopes = if path.exists() {
        event_log::read_all(&path)?
    } else {
        Vec::new()
    };

    let kind_filter = args
        .kind
        .as_deref()
        .map(|s| CollapseReason::from(s.to_owned()));
    let reason_filter = args.reason.as_deref();

    let records = collect_records(&envelopes, cutoff, kind_filter.as_ref(), reason_filter);

    let want_json = ctx.json || args.json;
    let summary = summarise(&records, &args.since, args.top);

    if want_json {
        emit_json(&records, &summary)?;
    } else {
        emit_table(&summary)?;
    }
    Ok(())
}

/// Resolve `events.jsonl` from `--ops-dir` or walk-up discovery.
fn resolve_events_path(ops_dir: Option<&PathBuf>) -> PathBuf {
    let state_dir = cosmon_filestore::resolve_state_dir(ops_dir.map(PathBuf::as_path));
    state_dir.join("events.jsonl")
}

/// Walk every envelope; keep only `MoleculeCollapsed` events that pass
/// the cutoff and the operator filters. Pure function — exposed at
/// crate scope so the unit tests can drive it without I/O.
fn collect_records(
    envelopes: &[cosmon_core::event_v2::Envelope],
    cutoff: DateTime<Utc>,
    kind_filter: Option<&CollapseReason>,
    reason_filter: Option<&str>,
) -> Vec<CollapseRecord> {
    let mut out = Vec::new();
    for env in envelopes {
        if env.timestamp < cutoff {
            continue;
        }
        let EventV2::MoleculeCollapsed {
            molecule_id,
            reason,
            kind,
        } = &env.event
        else {
            continue;
        };
        let resolved_kind = kind
            .clone()
            .unwrap_or_else(|| CollapseReason::Other(reason.clone()));
        if let Some(filter) = kind_filter {
            if !matches_filter(filter, &resolved_kind) {
                continue;
            }
        }
        if let Some(needle) = reason_filter {
            if !reason.contains(needle) {
                continue;
            }
        }
        out.push(CollapseRecord {
            molecule_id: molecule_id.as_str().to_owned(),
            kind: resolved_kind.as_str().to_owned(),
            structured: resolved_kind.is_structured(),
            reason: reason.clone(),
            timestamp: env.timestamp,
        });
    }
    out
}

/// Match a record against an operator-supplied filter. Typed variants
/// match by equality; `Other(needle)` matches by substring against the
/// record's wire string — so `--kind oom` finds every legacy free-form
/// reason mentioning OOM, even when the operator never tagged the
/// collapse.
fn matches_filter(filter: &CollapseReason, observed: &CollapseReason) -> bool {
    match (filter, observed) {
        (CollapseReason::Other(needle), other) => other.as_str().contains(needle.as_str()),
        (a, b) => a == b,
    }
}

/// Aggregate the per-record stream into the structured summary.
fn summarise(records: &[CollapseRecord], window_since: &str, top: usize) -> Summary {
    let mut by_kind: BTreeMap<String, KindRow> = BTreeMap::new();
    let mut structured = 0u64;
    let mut other = 0u64;
    for r in records {
        if r.structured {
            structured += 1;
        } else {
            other += 1;
        }
        let entry = by_kind.entry(r.kind.clone()).or_insert_with(|| KindRow {
            kind: r.kind.clone(),
            count: 0,
            structured: r.structured,
            sample_molecules: Vec::new(),
        });
        entry.count += 1;
        if entry.sample_molecules.len() < 3 && !entry.sample_molecules.contains(&r.molecule_id) {
            entry.sample_molecules.push(r.molecule_id.clone());
        }
    }
    let mut rows: Vec<KindRow> = by_kind.into_values().collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.kind.cmp(&b.kind)));
    rows.truncate(top.max(1));

    let total = structured + other;
    // Counts are bounded by the events.jsonl tail (effectively << 2^52);
    // f64 mantissa precision is not a concern here.
    #[allow(clippy::cast_precision_loss)]
    let structured_pct = if total == 0 {
        0.0
    } else {
        (structured as f64 / total as f64) * 100.0
    };

    Summary {
        window_since: window_since.to_owned(),
        total,
        structured,
        other,
        structured_pct,
        by_kind: rows,
    }
}

/// Write NDJSON: one record per line, then one trailing summary line.
fn emit_json(records: &[CollapseRecord], summary: &Summary) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    for r in records {
        writeln!(out, "{}", serde_json::to_string(r)?)?;
    }
    writeln!(out, "{}", serde_json::to_string(summary)?)?;
    Ok(())
}

/// Write the human-readable tabular view.
fn emit_table(summary: &Summary) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "window: since {}    total: {}    structured: {} ({:.1}%)    other: {}",
        summary.window_since,
        summary.total,
        summary.structured,
        summary.structured_pct,
        summary.other
    )?;
    if summary.total == 0 {
        writeln!(out, "(no collapses in window)")?;
        return Ok(());
    }
    writeln!(out)?;
    writeln!(
        out,
        "{:<22} {:>6}  {:<8}  SAMPLES",
        "KIND", "COUNT", "TYPED"
    )?;
    for row in &summary.by_kind {
        let typed = if row.structured { "yes" } else { "—" };
        let samples = row
            .sample_molecules
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        // Truncate long Other(...) wire strings so the table stays scannable.
        let kind_display = if row.kind.len() > 22 {
            let mut s: String = row.kind.chars().take(21).collect();
            s.push('…');
            s
        } else {
            row.kind.clone()
        };
        writeln!(
            out,
            "{kind_display:<22} {:>6}  {typed:<8}  {samples}",
            row.count
        )?;
    }
    Ok(())
}

/// Parse `--since`. Accepts:
///   - `<N>d`, `<N>h`, `<N>m`, `<N>s` (relative duration → cutoff = now − Δ)
///   - RFC-3339 timestamp (absolute cutoff, e.g. `2026-04-01T00:00:00Z`)
fn parse_since(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("--since cannot be empty");
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("--since: cannot parse leading integer in {s:?}"))?;
    if n < 0 {
        anyhow::bail!("--since: negative window {s:?}");
    }
    let delta = match unit {
        "d" => Duration::days(n),
        "h" => Duration::hours(n),
        "m" => Duration::minutes(n),
        "s" => Duration::seconds(n),
        _ => anyhow::bail!("--since: unknown unit {unit:?} (use d/h/m/s or an RFC-3339 timestamp)"),
    };
    Ok(Utc::now() - delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::{EmitterKind, Envelope, Seq};
    use cosmon_core::id::MoleculeId;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn env(
        ts: DateTime<Utc>,
        molecule: MoleculeId,
        reason: &str,
        kind: Option<CollapseReason>,
    ) -> Envelope {
        Envelope {
            seq: Seq(0),
            mol_seq: None,
            timestamp: ts,
            causal_parent: None,
            quality_band: None,
            emitter_kind: EmitterKind::default(),
            emitter_id: String::new(),
            meta_level: 0,
            event: EventV2::MoleculeCollapsed {
                molecule_id: molecule,
                reason: reason.to_owned(),
                kind,
            },
        }
    }

    #[test]
    fn parse_since_accepts_relative_duration() {
        let now = Utc::now();
        let cutoff = parse_since("7d").unwrap();
        let delta = now.signed_duration_since(cutoff);
        assert!(delta >= Duration::days(7) - Duration::seconds(1));
        assert!(delta <= Duration::days(7) + Duration::seconds(1));
    }

    #[test]
    fn parse_since_accepts_rfc3339() {
        let cutoff = parse_since("2026-04-01T00:00:00Z").unwrap();
        assert_eq!(cutoff.to_rfc3339(), "2026-04-01T00:00:00+00:00");
    }

    #[test]
    fn parse_since_rejects_unknown_unit() {
        assert!(parse_since("7y").is_err());
        assert!(parse_since("").is_err());
    }

    #[test]
    fn collect_records_filters_by_window() {
        let now = Utc::now();
        let envs = vec![
            env(
                now - Duration::days(10),
                mid("task-20260420-aaaa"),
                "old",
                Some(CollapseReason::WorkerCrashed),
            ),
            env(
                now - Duration::hours(1),
                mid("task-20260506-bbbb"),
                "fresh",
                Some(CollapseReason::WorkerCrashed),
            ),
        ];
        let cutoff = now - Duration::days(7);
        let records = collect_records(&envs, cutoff, None, None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].molecule_id, "task-20260506-bbbb");
    }

    #[test]
    fn collect_records_filters_by_kind() {
        let now = Utc::now();
        let envs = vec![
            env(
                now,
                mid("task-20260506-c001"),
                "build broke",
                Some(CollapseReason::GateFailed),
            ),
            env(
                now,
                mid("task-20260506-c002"),
                "oom",
                Some(CollapseReason::WorkerCrashed),
            ),
        ];
        let cutoff = now - Duration::days(1);
        let filter = CollapseReason::GateFailed;
        let records = collect_records(&envs, cutoff, Some(&filter), None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].molecule_id, "task-20260506-c001");
    }

    #[test]
    fn collect_records_other_kind_substring_matches() {
        let now = Utc::now();
        let envs = vec![env(
            now,
            mid("task-20260506-d003"),
            "out of memory mid-bake",
            None, // no typed kind — falls through to Other(reason)
        )];
        let cutoff = now - Duration::days(1);
        let filter = CollapseReason::Other("out of memory".to_owned());
        let records = collect_records(&envs, cutoff, Some(&filter), None);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn collect_records_filters_by_reason_substring() {
        let now = Utc::now();
        let envs = vec![
            env(
                now,
                mid("task-20260506-e001"),
                "build broke at compile",
                Some(CollapseReason::GateFailed),
            ),
            env(
                now,
                mid("task-20260506-e002"),
                "tests timed out",
                Some(CollapseReason::GateFailed),
            ),
        ];
        let cutoff = now - Duration::days(1);
        let records = collect_records(&envs, cutoff, None, Some("compile"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].molecule_id, "task-20260506-e001");
    }

    #[test]
    fn summarise_computes_ifbdd_ratio() {
        let now = Utc::now();
        let records = vec![
            CollapseRecord {
                molecule_id: "task-1".into(),
                kind: "worker_crashed".into(),
                structured: true,
                reason: "oom".into(),
                timestamp: now,
            },
            CollapseRecord {
                molecule_id: "task-2".into(),
                kind: "worker_crashed".into(),
                structured: true,
                reason: "panic".into(),
                timestamp: now,
            },
            CollapseRecord {
                molecule_id: "task-3".into(),
                kind: "weird stuff".into(),
                structured: false,
                reason: "weird stuff".into(),
                timestamp: now,
            },
        ];
        let s = summarise(&records, "7d", 10);
        assert_eq!(s.total, 3);
        assert_eq!(s.structured, 2);
        assert_eq!(s.other, 1);
        assert!((s.structured_pct - 66.66).abs() < 0.1);
        assert_eq!(s.by_kind.len(), 2);
        assert_eq!(s.by_kind[0].kind, "worker_crashed");
        assert_eq!(s.by_kind[0].count, 2);
    }

    #[test]
    fn summarise_handles_empty_window() {
        let s = summarise(&[], "7d", 10);
        assert_eq!(s.total, 0);
        assert!((s.structured_pct - 0.0).abs() < f64::EPSILON);
        assert!(s.by_kind.is_empty());
    }
}
