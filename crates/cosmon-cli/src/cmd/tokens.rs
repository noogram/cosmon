// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tokens` — read-only aggregator over the token-meter NDJSON sink.
//!
//! Reads `<state_dir>/instrumentation/tokens.jsonl` directly with no
//! pre-aggregation, no caching, and no write paths — this is purely
//! a window onto the consumption fact stream.
//!
//! ```text
//! cs tokens                            top-N tenants this week
//! cs tokens --tenant tenant_auditor             per-kind breakdown for tenant_auditor
//! cs tokens --molecule task-…-d1fa     per-molecule API token totals
//! cs tokens --since 7d --json          NDJSON stream for scripting
//! ```
//!
//! Without this command the token-usage signal would be tied to ad-hoc
//! `jq` recipes and to operator memory. `cs tokens` is the smallest
//! reading surface that keeps the measurements actionable.
//!
//! This is observability, not billing. Billing decisions await the
//! token-usage signal.

use std::collections::BTreeMap;
use std::io::Write;

#[cfg(test)]
use chrono::DateTime;
use chrono::{Duration, Utc};
use cosmon_state::token_meter::{read_token_ndjson, resolve_token_path, TokenUsage};
use serde::Serialize;

use super::Context;

/// Arguments for `cs tokens`.
#[derive(clap::Args)]
pub struct Args {
    /// Filter to one tenant. When set, the command renders a per-kind
    /// breakdown for that tenant instead of the cross-tenant top-N.
    #[arg(long)]
    pub tenant: Option<String>,

    /// Filter to one molecule. When set, the command renders the summed
    /// API token totals (in / out / cost / invocations) for that
    /// `molecule_id` — the same per-molecule fold surfaced by
    /// `cs observe <id>`. Takes precedence over `--tenant`.
    #[arg(long)]
    pub molecule: Option<String>,

    /// Time window — accepts `<N>d` (days), `<N>h` (hours),
    /// `<N>m` (minutes), or `<N>s` (seconds). Defaults to 7 days.
    #[arg(long, default_value = "7d")]
    pub since: String,

    /// Limit on the number of rows in the top-N view. Ignored when
    /// `--tenant` is set.
    #[arg(long, default_value_t = 10)]
    pub top: usize,

    /// Emit raw NDJSON of the matching events (one per line) instead
    /// of a tabular summary. The global `--json` flag also enables
    /// this; the explicit flag remains for ergonomic discoverability.
    #[arg(long)]
    pub json: bool,
}

/// Per-tenant aggregate row, the stable JSON shape exposed by
/// `--json` summaries.
#[derive(Debug, Serialize)]
struct TenantRow {
    tenant: String,
    invocations: u64,
    tokens_in: u64,
    tokens_out: u64,
    cost_micros_estimated: u64,
}

/// Per-kind aggregate row (one tenant view).
#[derive(Debug, Serialize)]
struct KindRow {
    kind: String,
    invocations: u64,
    tokens_in: u64,
    tokens_out: u64,
    cost_micros_estimated: u64,
}

/// Per-molecule aggregate row (one molecule view).
#[derive(Debug, Serialize)]
struct MoleculeRow {
    molecule: String,
    invocations: u64,
    tokens_in: u64,
    tokens_out: u64,
    cost_micros_estimated: u64,
}

/// Execute `cs tokens`.
///
/// # Errors
///
/// Returns the underlying I/O error if the NDJSON sink exists but
/// cannot be read; a missing sink is treated as "no events yet" and
/// renders an empty summary.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let since = parse_since(&args.since)?;
    let cutoff = Utc::now() - since;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let path = resolve_token_path(&state_dir);
    let events = read_token_ndjson(&path)?;
    let in_window: Vec<TokenUsage> = events
        .into_iter()
        .filter(|e| e.timestamp >= cutoff)
        .collect();

    let want_json = ctx.json || args.json;

    match (args.molecule.as_deref(), args.tenant.as_deref()) {
        (Some(molecule), _) => render_per_molecule(molecule, &in_window, want_json),
        (None, Some(tenant)) => render_per_kind(tenant, &in_window, want_json),
        (None, None) => render_top_tenants(&in_window, args.top, want_json),
    }
}

/// Render the summed API token totals for a single molecule.
fn render_per_molecule(molecule: &str, events: &[TokenUsage], json: bool) -> anyhow::Result<()> {
    use cosmon_state::token_meter::MoleculeTokenTotals;

    let mut totals = MoleculeTokenTotals::default();
    let mut invocations: u64 = 0;
    for ev in events.iter().filter(|e| e.molecule_id.as_str() == molecule) {
        totals.tokens_in = totals.tokens_in.saturating_add(ev.tokens_in);
        totals.tokens_out = totals.tokens_out.saturating_add(ev.tokens_out);
        totals.cost_micros_estimated = totals
            .cost_micros_estimated
            .saturating_add(ev.cost_micros_estimated);
        invocations = invocations.saturating_add(1);
    }
    totals.invocations = invocations;

    if json {
        let row = MoleculeRow {
            molecule: molecule.to_owned(),
            invocations: totals.invocations,
            tokens_in: totals.tokens_in,
            tokens_out: totals.tokens_out,
            cost_micros_estimated: totals.cost_micros_estimated,
        };
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", serde_json::to_string(&row)?)?;
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    writeln!(out, "molecule: {molecule}")?;
    if totals.invocations == 0 {
        writeln!(out, "(no token usage events for this molecule in window)")?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<10} {:>12} {:>12} {:>12} {:>16}",
        "INVOC", "TOKENS_IN", "TOKENS_OUT", "TOKENS_TOTAL", "COST_MICROS_EST"
    )?;
    writeln!(
        out,
        "{:<10} {:>12} {:>12} {:>12} {:>16}",
        totals.invocations,
        totals.tokens_in,
        totals.tokens_out,
        totals.total_tokens(),
        totals.cost_micros_estimated
    )?;
    Ok(())
}

fn render_top_tenants(events: &[TokenUsage], top: usize, json: bool) -> anyhow::Result<()> {
    let mut by_tenant: BTreeMap<String, TenantRow> = BTreeMap::new();
    for ev in events {
        let entry = by_tenant
            .entry(ev.tenant.as_str().to_owned())
            .or_insert_with(|| TenantRow {
                tenant: ev.tenant.as_str().to_owned(),
                invocations: 0,
                tokens_in: 0,
                tokens_out: 0,
                cost_micros_estimated: 0,
            });
        entry.invocations += 1;
        entry.tokens_in += ev.tokens_in;
        entry.tokens_out += ev.tokens_out;
        entry.cost_micros_estimated += ev.cost_micros_estimated;
    }
    let mut rows: Vec<TenantRow> = by_tenant.into_values().collect();
    rows.sort_by(|a, b| {
        b.cost_micros_estimated
            .cmp(&a.cost_micros_estimated)
            .then_with(|| b.tokens_in.cmp(&a.tokens_in))
    });
    rows.truncate(top.max(1));

    if json {
        let mut out = std::io::stdout().lock();
        for row in &rows {
            let line = serde_json::to_string(row)?;
            writeln!(out, "{line}")?;
        }
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    if rows.is_empty() {
        writeln!(out, "(no token usage events in window)")?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<24} {:>10} {:>12} {:>12} {:>16}",
        "TENANT", "INVOC", "TOKENS_IN", "TOKENS_OUT", "COST_MICROS_EST"
    )?;
    for r in &rows {
        writeln!(
            out,
            "{:<24} {:>10} {:>12} {:>12} {:>16}",
            r.tenant, r.invocations, r.tokens_in, r.tokens_out, r.cost_micros_estimated
        )?;
    }
    Ok(())
}

fn render_per_kind(tenant: &str, events: &[TokenUsage], json: bool) -> anyhow::Result<()> {
    let mut by_kind: BTreeMap<String, KindRow> = BTreeMap::new();
    for ev in events.iter().filter(|e| e.tenant.as_str() == tenant) {
        let key = ev
            .kind
            .map_or_else(|| "unknown".to_owned(), |k| k.to_string());
        let entry = by_kind.entry(key.clone()).or_insert_with(|| KindRow {
            kind: key,
            invocations: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_micros_estimated: 0,
        });
        entry.invocations += 1;
        entry.tokens_in += ev.tokens_in;
        entry.tokens_out += ev.tokens_out;
        entry.cost_micros_estimated += ev.cost_micros_estimated;
    }
    let mut rows: Vec<KindRow> = by_kind.into_values().collect();
    rows.sort_by_key(|x| std::cmp::Reverse(x.invocations));

    if json {
        let mut out = std::io::stdout().lock();
        for row in &rows {
            let line = serde_json::to_string(row)?;
            writeln!(out, "{line}")?;
        }
        return Ok(());
    }

    let mut out = std::io::stdout().lock();
    writeln!(out, "tenant: {tenant}")?;
    if rows.is_empty() {
        writeln!(out, "(no token usage events for this tenant in window)")?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<14} {:>10} {:>12} {:>12} {:>16}",
        "KIND", "INVOC", "TOKENS_IN", "TOKENS_OUT", "COST_MICROS_EST"
    )?;
    for r in &rows {
        writeln!(
            out,
            "{:<14} {:>10} {:>12} {:>12} {:>16}",
            r.kind, r.invocations, r.tokens_in, r.tokens_out, r.cost_micros_estimated
        )?;
    }
    Ok(())
}

/// Parse a `--since` string. Accepts `<N>d`, `<N>h`, `<N>m`, `<N>s`,
/// where `N` is a positive integer. Returns the corresponding
/// `chrono::Duration`.
fn parse_since(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("--since cannot be empty");
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("--since: cannot parse leading integer in {s:?}"))?;
    if n < 0 {
        anyhow::bail!("--since: negative window {s:?}");
    }
    match unit {
        "d" => Ok(Duration::days(n)),
        "h" => Ok(Duration::hours(n)),
        "m" => Ok(Duration::minutes(n)),
        "s" => Ok(Duration::seconds(n)),
        _ => anyhow::bail!("--since: unknown unit {unit:?} (use d/h/m/s)"),
    }
}

/// Returns the parsed cutoff point given a `--since` string. Useful
/// for unit tests that need to reason about the window boundary.
#[cfg(test)]
fn cutoff_from(now: DateTime<Utc>, since: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(now - parse_since(since)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_accepts_days_hours_minutes_seconds() {
        assert_eq!(parse_since("7d").unwrap(), Duration::days(7));
        assert_eq!(parse_since("12h").unwrap(), Duration::hours(12));
        assert_eq!(parse_since("90m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_since("60s").unwrap(), Duration::seconds(60));
    }

    #[test]
    fn parse_since_rejects_unknown_unit() {
        assert!(parse_since("7y").is_err());
        assert!(parse_since("").is_err());
    }

    #[test]
    fn cutoff_is_in_the_past() {
        let now = Utc::now();
        let c = cutoff_from(now, "1h").unwrap();
        assert!(c < now);
    }
}
