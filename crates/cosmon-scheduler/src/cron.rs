// SPDX-License-Identifier: AGPL-3.0-only

//! Minimal POSIX 5-field cron parser.
//!
//! ## Scope
//!
//! Supports the subset the scheduler actually needs — the same subset
//! operators routinely write in `launchd` `StartCalendarInterval` plists:
//!
//! | Field     | Range     | Syntax we support                     |
//! |-----------|-----------|---------------------------------------|
//! | minute    | 0–59      | `*`, `N`, `N-M`, `N,M,P`, `*/K`, `N-M/K` |
//! | hour      | 0–23      | same                                  |
//! | day-of-m  | 1–31      | same                                  |
//! | month     | 1–12      | same                                  |
//! | day-of-w  | 0–6 (Sun) | same; `7` accepted as alias for `0`   |
//!
//! Out of scope (by intent, not oversight): named months (`JAN`), named
//! weekdays (`SUN`), `@yearly` macros, Quartz-style seconds/years.
//! Operators who need those can fall back to `interval_seconds` or wait
//! for cron v2. See [idea-20260417-b52d/plan.md §"Re-evaluation
//! criteria"].
//!
//! ## Evaluation semantics
//!
//! A cron patrol fires on tick `T` iff:
//!
//! 1. The wall-clock representation of `T` in the operator's local
//!    timezone matches all five fields, **and**
//! 2. The minute-floor of `last_fired_at` is strictly earlier than the
//!    minute-floor of `T` (or `last_fired_at` is `None`).
//!
//! Rule (2) prevents re-firing within the same minute if the tick
//! drifts or the launchd invocation interval is finer than 60s. There
//! is no catch-up for missed minutes — a tick that finds the match
//! minute already past simply skips; that is the right behavior for
//! "weekly digest at 9am on Sunday" (we do not want a Monday-morning
//! reboot to fire yesterday's digest).

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use thiserror::Error;

/// A parsed 5-field cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    minute: FieldSet,
    hour: FieldSet,
    day_of_month: FieldSet,
    month: FieldSet,
    day_of_week: FieldSet,
}

/// Set of integer values a cron field matches. Backed by a u64 bitfield
/// because every supported range fits in 64 values (0..=59 is the
/// widest). Bit `n` is set iff the field matches value `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FieldSet {
    bits: u64,
}

impl FieldSet {
    fn empty() -> Self {
        Self { bits: 0 }
    }

    fn insert(&mut self, value: u32) {
        if value < 64 {
            self.bits |= 1 << value;
        }
    }

    fn contains(self, value: u32) -> bool {
        value < 64 && (self.bits >> value) & 1 == 1
    }
}

/// Cron parse / validation errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CronError {
    /// The expression did not contain exactly five whitespace-separated fields.
    #[error("cron expression must have 5 fields, got {0}")]
    WrongArity(usize),

    /// A field contained a numeric token that could not be parsed as u32
    /// or fell outside the legal range for its position.
    #[error("cron field #{field_index} '{raw}' invalid: {reason}")]
    BadField {
        /// 0-based field index (0=minute, 4=day-of-week).
        field_index: usize,
        /// The offending raw text.
        raw: String,
        /// Human explanation (e.g. "value 60 out of range 0..=59").
        reason: String,
    },
}

impl CronExpr {
    /// Parse a 5-field POSIX cron expression.
    ///
    /// # Errors
    ///
    /// Returns [`CronError::WrongArity`] if the expression is not five
    /// fields, or [`CronError::BadField`] if any field has invalid
    /// syntax or values outside its range.
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronError::WrongArity(fields.len()));
        }
        Ok(Self {
            minute: parse_field(fields[0], 0, 0, 59)?,
            hour: parse_field(fields[1], 1, 0, 23)?,
            day_of_month: parse_field(fields[2], 2, 1, 31)?,
            month: parse_field(fields[3], 3, 1, 12)?,
            day_of_week: parse_field(fields[4], 4, 0, 7)?,
        })
    }

    /// Returns `true` if the operator's local wall-clock at `now` (a
    /// UTC instant, converted to [`chrono::Local`]) matches all five
    /// fields. This is rule (1) of the fire semantics; rule (2) (no
    /// double-fire within a minute) is enforced by the caller via
    /// [`PatrolState::last_fired_at`](crate::state::PatrolState::last_fired_at).
    #[must_use]
    pub fn matches(&self, now: DateTime<Utc>) -> bool {
        let local = now.with_timezone(&Local);
        // POSIX cron convention: day 0 and day 7 both mean Sunday.
        let dow_sun0 = local.weekday().num_days_from_sunday();
        let dow_matches =
            self.day_of_week.contains(dow_sun0) || (dow_sun0 == 0 && self.day_of_week.contains(7));
        self.minute.contains(local.minute())
            && self.hour.contains(local.hour())
            && self.day_of_month.contains(local.day())
            && self.month.contains(local.month())
            && dow_matches
    }
}

/// Parse one field. `lo..=hi` is the legal range for this position.
fn parse_field(raw: &str, index: usize, lo: u32, hi: u32) -> Result<FieldSet, CronError> {
    let mut set = FieldSet::empty();
    for token in raw.split(',') {
        parse_token(token, lo, hi, &mut set).map_err(|reason| CronError::BadField {
            field_index: index,
            raw: raw.to_owned(),
            reason,
        })?;
    }
    Ok(set)
}

/// Parse one comma-separated token (`5`, `*`, `1-3`, `*/2`, `1-9/2`).
fn parse_token(token: &str, lo: u32, hi: u32, into: &mut FieldSet) -> Result<(), String> {
    let (range_part, step) = match token.split_once('/') {
        Some((r, s)) => {
            let step: u32 = s.parse().map_err(|_| format!("bad step '{s}'"))?;
            if step == 0 {
                return Err(format!("step must be >= 1, got '{s}'"));
            }
            (r, step)
        }
        None => (token, 1),
    };

    let (start, end) = parse_range(range_part, lo, hi)?;

    let mut v = start;
    while v <= end {
        into.insert(v);
        // Guard: if step would overflow the u32 domain we already covered
        // enough; for our 0..=59 worst case this cannot happen.
        v = v.saturating_add(step);
        if step == 0 {
            break;
        }
    }
    Ok(())
}

/// Parse the `range_part` of a token: `*`, `N`, or `N-M`. Returns an
/// inclusive `(start, end)` constrained to `lo..=hi`.
fn parse_range(part: &str, lo: u32, hi: u32) -> Result<(u32, u32), String> {
    if part == "*" {
        return Ok((lo, hi));
    }
    if let Some((a, b)) = part.split_once('-') {
        let start: u32 = a.parse().map_err(|_| format!("bad range start '{a}'"))?;
        let end: u32 = b.parse().map_err(|_| format!("bad range end '{b}'"))?;
        if start < lo || end > hi || start > end {
            return Err(format!("range {start}-{end} not within {lo}..={hi}"));
        }
        return Ok((start, end));
    }
    let v: u32 = part.parse().map_err(|_| format!("bad value '{part}'"))?;
    if v < lo || v > hi {
        return Err(format!("value {v} out of range {lo}..={hi}"));
    }
    Ok((v, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_wildcards() {
        let e = CronExpr::parse("* * * * *").unwrap();
        assert!(e.minute.contains(0));
        assert!(e.minute.contains(59));
        assert!(e.hour.contains(23));
    }

    #[test]
    fn parses_single_values() {
        let e = CronExpr::parse("0 9 * * 0").unwrap();
        assert!(e.minute.contains(0));
        assert!(!e.minute.contains(1));
        assert!(e.hour.contains(9));
        assert!(!e.hour.contains(10));
        assert!(e.day_of_week.contains(0));
    }

    #[test]
    fn parses_ranges_and_lists() {
        let e = CronExpr::parse("1-3,10 * * * *").unwrap();
        for v in [1, 2, 3, 10] {
            assert!(e.minute.contains(v), "missing {v}");
        }
        assert!(!e.minute.contains(4));
    }

    #[test]
    fn parses_steps() {
        let e = CronExpr::parse("*/15 * * * *").unwrap();
        for v in [0, 15, 30, 45] {
            assert!(e.minute.contains(v), "missing {v}");
        }
        assert!(!e.minute.contains(7));
    }

    #[test]
    fn rejects_wrong_arity() {
        let err = CronExpr::parse("0 9 * *").unwrap_err();
        assert_eq!(err, CronError::WrongArity(4));
    }

    #[test]
    fn rejects_out_of_range_minute() {
        let err = CronExpr::parse("60 * * * *").unwrap_err();
        assert!(matches!(err, CronError::BadField { field_index: 0, .. }));
    }

    #[test]
    fn rejects_inverted_range() {
        let err = CronExpr::parse("5-2 * * * *").unwrap_err();
        assert!(matches!(err, CronError::BadField { field_index: 0, .. }));
    }

    #[test]
    fn accepts_7_as_sunday_alias() {
        let e = CronExpr::parse("0 9 * * 7").unwrap();
        // Build a Sunday and check matches.
        let sunday_9am = Local
            .with_ymd_and_hms(2026, 4, 19, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            Local
                .with_ymd_and_hms(2026, 4, 19, 9, 0, 0)
                .unwrap()
                .weekday()
                .num_days_from_sunday(),
            0,
            "2026-04-19 must be Sunday for this test to be meaningful"
        );
        assert!(e.matches(sunday_9am));
    }

    #[test]
    fn matches_exact_minute_hour_and_weekday() {
        // Sunday 9:00 in local time.
        let e = CronExpr::parse("0 9 * * 0").unwrap();

        let sunday_9am_local = Local.with_ymd_and_hms(2026, 4, 19, 9, 0, 0).unwrap();
        assert_eq!(
            sunday_9am_local.weekday().num_days_from_sunday(),
            0,
            "sanity: 2026-04-19 is Sunday"
        );
        assert!(e.matches(sunday_9am_local.with_timezone(&Utc)));

        let sunday_10am_local = Local.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        assert!(!e.matches(sunday_10am_local.with_timezone(&Utc)));

        let monday_9am_local = Local.with_ymd_and_hms(2026, 4, 20, 9, 0, 0).unwrap();
        assert!(!e.matches(monday_9am_local.with_timezone(&Utc)));
    }

    #[test]
    fn matches_every_minute_star() {
        let e = CronExpr::parse("* * * * *").unwrap();
        let t = Utc.with_ymd_and_hms(2026, 4, 18, 12, 34, 56).unwrap();
        assert!(e.matches(t));
    }

    #[test]
    fn matches_every_15_minutes() {
        let e = CronExpr::parse("*/15 * * * *").unwrap();
        let at_0 = Local.with_ymd_and_hms(2026, 4, 18, 12, 0, 0).unwrap();
        let at_15 = Local.with_ymd_and_hms(2026, 4, 18, 12, 15, 0).unwrap();
        let at_7 = Local.with_ymd_and_hms(2026, 4, 18, 12, 7, 0).unwrap();
        assert!(e.matches(at_0.with_timezone(&Utc)));
        assert!(e.matches(at_15.with_timezone(&Utc)));
        assert!(!e.matches(at_7.with_timezone(&Utc)));
    }
}
