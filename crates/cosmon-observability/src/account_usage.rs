// SPDX-License-Identifier: AGPL-3.0-only

//! Provider account allowance, reset and pace (issue #242).
//!
//! Workers carry the rate-limit windows their provider reported
//! ([`PlanWindowObservation`], scope `account`). This module folds those
//! readings into one entry per account window and answers the questions an
//! operator asks while a fleet runs: how much of the window is left, when it
//! resets, whether the current pace exhausts it first, how old the reading
//! is, and which workers drew on it.
//!
//! It is pure: the caller supplies the records and the clock, so `cs peek`
//! (TUI, `--no-tui`, `--json`) and `cs pulse --json` publish the same figures
//! from the same state.
//!
//! # Account identity is inferred
//!
//! No reading names the account it came from. Two readings belong to the
//! same account window when they agree on provider, meter, window length and
//! reset instant. Use within one window never decreases, so a newer reading
//! lower than an older one under the same key means two accounts share that
//! reset instant; the entry is then flagged
//! [`AccountWindow::conflicting_readings`] instead of silently picking one.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use cosmon_core::usage::{Freshness, PlanCredits, PlanWindowObservation, UsageRecord};
use serde::Serialize;

use crate::usage_projection::deduplicate_histories;

/// Age beyond which an account reading is shown as stale. It matches the
/// maximum age the energy readers apply when they classify a sample.
pub const STALE_AFTER_SECS: i64 = 600;

/// Tolerance, in percentage points, before a lower newer reading counts as a
/// contradiction rather than rounding in the provider's percentage.
const CONFLICT_TOLERANCE_PERCENT: f64 = 1.0;

/// What the per-worker share is computed from, published beside the shares
/// so a consumer never mistakes it for a provider-attributed quota.
pub const SHARE_BASIS: &str = "uncached_input_plus_output_tokens";

/// Why the pace of a window cannot be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceUnknown {
    /// The provider did not report the window's length.
    WindowLengthUnknown,
    /// The provider did not report when the window resets.
    ResetTimeUnknown,
    /// The reading carries no time, so its place in the window is unknown.
    ObservationTimeUnknown,
    /// The reading's time does not fall inside the window it describes.
    ObservationOutsideWindow,
    /// The window has reset since the reading; the new one is unobserved.
    WindowReset,
}

/// Use measured against an even spread of the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Pace {
    /// Pace computed from the reading's place in its window.
    Known {
        /// Share of the window elapsed at the reading, in percent: the use an
        /// even pace would have reached.
        even_use_percent: f64,
        /// Use minus even use, in percentage points. Positive means ahead of
        /// an even pace (in deficit), negative means behind it (in reserve).
        deficit_percent: f64,
        /// When the allowance runs out at the average rate so far, if that
        /// happens before the reset. `None` means it lasts until the reset.
        runs_out_at: Option<DateTime<Utc>>,
        /// Seconds from now to [`Self::Known::runs_out_at`], never negative.
        runs_out_in_secs: Option<i64>,
    },
    /// Pace cannot be computed from this reading.
    Unknown {
        /// What is missing.
        reason: PaceUnknown,
    },
}

/// One worker's part of the use observed on an account window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkerShare {
    /// Worker whose usage records carry this window.
    pub worker_id: String,
    /// The worker's tokens on [`SHARE_BASIS`]; `None` when a category is
    /// unmeasured.
    pub tokens: Option<u64>,
    /// The worker's share of the window's workers' tokens, in percent.
    /// `None` whenever any worker's tokens are unknown, because a share of a
    /// partial total would overstate the known workers.
    pub share_percent: Option<f64>,
}

/// One provider account window, as `cs peek` and `cs pulse` publish it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountWindow {
    /// Provider that reported the window (`openai`, `anthropic`).
    pub provider: Option<String>,
    /// Provider meter, as normalized by the reader (Codex meters carry a
    /// hashed bucket prefix, never the provider's label).
    pub meter: String,
    /// Window length in minutes, when reported.
    pub window_minutes: Option<u64>,
    /// When the window resets, when reported.
    pub resets_at: Option<DateTime<Utc>>,
    /// Seconds from now to the reset, negative once it has passed; `None`
    /// when the reset time is unknown.
    pub resets_in_secs: Option<i64>,
    /// Use reported by the newest reading, in percent (overage above 100 is
    /// kept).
    pub used_percent: f64,
    /// Allowance left, in percent; `None` once the window has reset, because
    /// the new window's use has not been observed.
    pub left_percent: Option<f64>,
    /// When the newest reading was made (source time, else capture time).
    pub observed_at: Option<DateTime<Utc>>,
    /// Seconds since [`Self::observed_at`].
    pub age_secs: Option<i64>,
    /// Freshness reclassified against the supplied clock and
    /// [`STALE_AFTER_SECS`].
    pub freshness: Freshness,
    /// Pace of use within the window.
    pub pace: Pace,
    /// Credits reported with the newest reading; `None` means unknown.
    pub credits: Option<PlanCredits>,
    /// Whether readings under this key contradict each other (see the module
    /// documentation).
    pub conflicting_readings: bool,
    /// What [`WorkerShare::tokens`] counts.
    pub share_basis: &'static str,
    /// Workers whose readings carry this window, sorted by id.
    pub workers: Vec<WorkerShare>,
}

/// A reading with the record it came from.
struct Reading<'a> {
    window: &'a PlanWindowObservation,
    record: &'a UsageRecord,
    at: Option<DateTime<Utc>>,
}

fn reading_time(window: &PlanWindowObservation) -> Option<DateTime<Utc>> {
    window
        .provenance
        .observed_at
        .or(window.provenance.captured_at)
}

fn percent(window: &PlanWindowObservation) -> f64 {
    window.utilization.get() * 100.0
}

/// Uncached input plus output: what a record contributes to a worker share.
fn basis_tokens(record: &UsageRecord) -> Option<u64> {
    let input = record.tokens.input_tokens.measured()?;
    let cached = record.tokens.cached_input_tokens.measured()?;
    let output = record.tokens.output_tokens.measured()?;
    input.checked_sub(cached)?.checked_add(output)
}

/// Fold usage records into one entry per account window, against `now`.
///
/// Cumulative histories are deduplicated first, so a history observed twice
/// counts once. A record without account windows contributes nothing: no
/// reading is no account, never an account at zero.
#[must_use]
pub fn project_accounts(records: &[UsageRecord], now: DateTime<Utc>) -> Vec<AccountWindow> {
    type Key = (String, String, Option<u64>, Option<DateTime<Utc>>);
    let mut groups: BTreeMap<Key, Vec<Reading<'_>>> = BTreeMap::new();
    for record in deduplicate_histories(records) {
        for window in &record.plan.windows {
            if window.scope != cosmon_core::usage::ObservationScope::Account {
                continue;
            }
            let key = (
                window.provenance.provider.clone().unwrap_or_default(),
                window.meter.clone(),
                window.window.duration_minutes,
                window.window.resets_at,
            );
            groups.entry(key).or_default().push(Reading {
                window,
                record,
                at: reading_time(window),
            });
        }
    }
    groups
        .into_values()
        .filter_map(|readings| account_window(&readings, now))
        .collect()
}

fn account_window(readings: &[Reading<'_>], now: DateTime<Utc>) -> Option<AccountWindow> {
    let newest = readings.iter().max_by(|a, b| {
        a.at.cmp(&b.at)
            .then_with(|| percent(a.window).total_cmp(&percent(b.window)))
    })?;
    let window = newest.window;
    let used_percent = percent(window);
    let resets_at = window.window.resets_at;
    let freshness = freshness(newest.at, resets_at, now);
    let reset = freshness == Freshness::Reset;
    Some(AccountWindow {
        provider: window.provenance.provider.clone(),
        meter: window.meter.clone(),
        window_minutes: window.window.duration_minutes,
        resets_at,
        resets_in_secs: resets_at.map(|t| t.signed_duration_since(now).num_seconds()),
        used_percent,
        left_percent: (!reset).then_some(100.0 - used_percent),
        observed_at: newest.at,
        age_secs: newest
            .at
            .map(|t| now.signed_duration_since(t).num_seconds()),
        freshness,
        pace: if reset {
            Pace::Unknown {
                reason: PaceUnknown::WindowReset,
            }
        } else {
            pace(window, newest.at, now)
        },
        credits: newest.record.plan.credits.clone(),
        conflicting_readings: conflicting(readings),
        share_basis: SHARE_BASIS,
        workers: shares(readings),
    })
}

fn freshness(
    at: Option<DateTime<Utc>>,
    resets_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Freshness {
    if resets_at.is_some_and(|t| t <= now) {
        return Freshness::Reset;
    }
    match at {
        Some(t) if t > now => Freshness::Unknown,
        Some(t) if now.signed_duration_since(t).num_seconds() > STALE_AFTER_SECS => {
            Freshness::Stale
        }
        Some(_) => Freshness::Fresh,
        None => Freshness::Unknown,
    }
}

#[allow(clippy::cast_precision_loss)] // durations of days in seconds are exact in f64
fn pace(window: &PlanWindowObservation, at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Pace {
    let unknown = |reason| Pace::Unknown { reason };
    // The length is provider data; one the clock cannot represent is as
    // good as no length at all.
    let Some(length) = window
        .window
        .duration_minutes
        .and_then(|m| i64::try_from(m).ok())
        .and_then(Duration::try_minutes)
    else {
        return unknown(PaceUnknown::WindowLengthUnknown);
    };
    let Some(resets_at) = window.window.resets_at else {
        return unknown(PaceUnknown::ResetTimeUnknown);
    };
    let Some(at) = at else {
        return unknown(PaceUnknown::ObservationTimeUnknown);
    };
    let Some(start) = resets_at.checked_sub_signed(length) else {
        return unknown(PaceUnknown::WindowLengthUnknown);
    };
    let elapsed = at.signed_duration_since(start);
    if elapsed <= Duration::zero() || elapsed > length {
        return unknown(PaceUnknown::ObservationOutsideWindow);
    }
    let elapsed_secs = elapsed.num_seconds() as f64;
    let even_use_percent = elapsed_secs / length.num_seconds() as f64 * 100.0;
    let used = window.utilization.get();
    // Time to use the remainder at the average rate so far.
    let runs_out_at = if used >= 1.0 {
        Some(at)
    } else if used > 0.0 {
        let remaining_secs = (1.0 - used) / used * elapsed_secs;
        // Compare in seconds first: a tiny use projects past any calendar
        // instant, and only a time before the reset is ever turned into one.
        let until_reset_secs = resets_at.signed_duration_since(at).num_seconds() as f64;
        #[allow(clippy::cast_possible_truncation)] // below `until_reset_secs`, an i64
        let exhausted = (remaining_secs < until_reset_secs)
            .then(|| at + Duration::seconds(remaining_secs.round() as i64));
        exhausted.filter(|t| *t < resets_at)
    } else {
        None
    };
    Pace::Known {
        even_use_percent,
        deficit_percent: used * 100.0 - even_use_percent,
        runs_out_at,
        runs_out_in_secs: runs_out_at.map(|t| t.signed_duration_since(now).num_seconds().max(0)),
    }
}

/// Whether some reading is lower than an older one beyond rounding.
fn conflicting(readings: &[Reading<'_>]) -> bool {
    let mut ordered: Vec<&Reading<'_>> = readings.iter().collect();
    ordered.sort_by_key(|r| r.at);
    let mut highest = f64::NEG_INFINITY;
    for reading in ordered {
        let value = percent(reading.window);
        if value + CONFLICT_TOLERANCE_PERCENT < highest {
            return true;
        }
        highest = highest.max(value);
    }
    false
}

#[allow(clippy::cast_precision_loss)] // token totals far below 2^52
fn shares(readings: &[Reading<'_>]) -> Vec<WorkerShare> {
    let mut by_worker: BTreeMap<String, Option<u64>> = BTreeMap::new();
    let mut counted: Vec<&UsageRecord> = Vec::new();
    for reading in readings {
        let record = reading.record;
        let tokens = by_worker
            .entry(record.subject.worker_id.to_string())
            .or_insert(Some(0));
        // A record repeating one window must still count its tokens once.
        if counted.iter().any(|seen| std::ptr::eq(*seen, record)) {
            continue;
        }
        counted.push(record);
        *tokens = tokens
            .zip(basis_tokens(record))
            .and_then(|(a, b)| a.checked_add(b));
    }
    let total = by_worker
        .values()
        .try_fold(0_u64, |sum, tokens| sum.checked_add((*tokens)?));
    by_worker
        .into_iter()
        .map(|(worker_id, tokens)| WorkerShare {
            share_percent: total
                .filter(|total| *total > 0)
                .zip(tokens)
                .map(|(total, tokens)| tokens as f64 / total as f64 * 100.0),
            worker_id,
            tokens,
        })
        .collect()
}

/// Human duration: `5d 5h`, `2h 0m`, `3m`, `<1m`.
#[must_use]
pub fn format_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let (days, hours, minutes) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "<1m".to_owned()
    }
}

/// Short window label: `7d`, `5h`, or the meter without its bucket prefix.
#[must_use]
pub fn window_label(account: &AccountWindow) -> String {
    match account.window_minutes {
        Some(minutes) if minutes % 1_440 == 0 => format!("{}d", minutes / 1_440),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None => account
            .meter
            .rsplit(':')
            .next()
            .unwrap_or(&account.meter)
            .to_owned(),
    }
}

fn format_credits(credits: &PlanCredits) -> Option<String> {
    if credits.unlimited == Some(true) {
        return Some("credits unlimited".to_owned());
    }
    credits
        .balance
        .map(|balance| format!("credits {}", balance.get()))
}

/// One line per account window, as `cs peek --no-tui` and the TUI show it:
///
/// ```text
/// openai 7d: 50% left · resets in 5d 5h · 25% in deficit, runs out in 1d 17h · credits 0 · seen 3m ago
/// ```
#[must_use]
pub fn format_account_window(account: &AccountWindow) -> String {
    let provider = account.provider.as_deref().unwrap_or("provider unknown");
    let label = window_label(account);
    if account.freshness == Freshness::Reset {
        let since = account.resets_in_secs.map_or_else(
            || "reset".to_owned(),
            |secs| format!("reset {} ago", format_duration_secs(-secs)),
        );
        return format!("{provider} {label}: {since}, not observed since");
    }
    let mut parts = vec![format!(
        "{provider} {label}: {:.0}% left",
        account.left_percent.unwrap_or(100.0 - account.used_percent)
    )];
    parts.push(account.resets_in_secs.map_or_else(
        || "reset time unknown".to_owned(),
        |secs| format!("resets in {}", format_duration_secs(secs)),
    ));
    parts.push(match &account.pace {
        Pace::Known {
            deficit_percent,
            runs_out_in_secs,
            ..
        } => {
            let side = if *deficit_percent > 0.0 {
                format!("{deficit_percent:.0}% in deficit")
            } else {
                format!("{:.0}% in reserve", -deficit_percent)
            };
            match runs_out_in_secs {
                Some(0) => format!("{side}, exhausted"),
                Some(secs) => format!("{side}, runs out in {}", format_duration_secs(*secs)),
                None => format!("{side}, lasts until reset"),
            }
        }
        Pace::Unknown { .. } => "pace unknown".to_owned(),
    });
    if let Some(credits) = account.credits.as_ref().and_then(format_credits) {
        parts.push(credits);
    }
    if account.conflicting_readings {
        parts.push("readings disagree (several accounts?)".to_owned());
    }
    let seen = account.age_secs.map_or_else(
        || "observation time unknown".to_owned(),
        |age| format!("seen {} ago", format_duration_secs(age)),
    );
    parts.push(if account.freshness == Freshness::Stale {
        format!("stale, {seen}")
    } else {
        seen
    });
    parts.join(" · ")
}

/// The worker shares of an account window as one line, e.g.
/// `workers: w-one 75% · w-two 25% (uncached input + output tokens)`.
/// Empty when the window has no worker.
#[must_use]
pub fn format_worker_shares(account: &AccountWindow) -> String {
    if account.workers.is_empty() {
        return String::new();
    }
    let shares: Vec<String> = account
        .workers
        .iter()
        .map(|w| {
            w.share_percent.map_or_else(
                || format!("{} share unknown", w.worker_id),
                |share| format!("{} {share:.0}%", w.worker_id),
            )
        })
        .collect();
    format!(
        "workers: {} (uncached input + output tokens)",
        shares.join(" · ")
    )
}
