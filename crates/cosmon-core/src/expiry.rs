// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule TTL and expiry policy (ADR-029).
//!
//! A molecule may carry an absolute `expires_at` deadline and an
//! [`ExpiryPolicy`] describing what happens when that deadline is in the
//! past. Expiry is a *derived* predicate on the clock — not a lifecycle
//! status — and is always evaluated by an external caller (human,
//! `cs expire`, `cs patrol --expire`), never by an autonomous loop.
//!
//! The state store embeds `expires_at` and `expiry_policy` on
//! [`cosmon_state::MoleculeData`]; this module holds the pure types and
//! the transition function so both the CLI and the surface renderers can
//! share one evaluator.
//!
//! ## Invariants
//!
//! - Running (non-terminal, non-pending) molecules never silently collapse:
//!   `ExpiryPolicy::Collapse` degrades to [`ExpiryAction::Warn`] when the
//!   molecule is active. See ADR-029 § Invariants.
//! - [`evaluate_expiry`] is a pure function of `(expires_at, policy, status,
//!   now)` — same inputs always produce the same action. This is what makes
//!   `cs expire` idempotent.

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::molecule::MoleculeStatus;

/// Parse a relative TTL string into a [`chrono::Duration`].
///
/// Accepted grammar: a positive integer followed by a single unit suffix,
/// one of:
/// - `s` — seconds
/// - `m` — minutes
/// - `h` — hours
/// - `d` — days
/// - `w` — weeks
///
/// The grammar is intentionally minimal and case-insensitive. `7d`, `24h`,
/// `2w`, `30m` are all valid. Composite forms (`1h30m`) are rejected —
/// use the largest unit that fits, or supply an absolute `--expires-at`.
///
/// # Errors
/// Returns a descriptive error string if the input is empty, has no unit
/// suffix, uses an unknown unit, or the numeric prefix fails to parse.
pub fn parse_ttl(s: &str) -> Result<Duration, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty TTL".to_owned());
    }
    let (num_part, unit) = trimmed.split_at(
        trimmed
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| format!("missing unit suffix in TTL `{s}` (expected s/m/h/d/w)"))?,
    );
    if num_part.is_empty() {
        return Err(format!("missing numeric prefix in TTL `{s}`"));
    }
    let n: i64 = num_part
        .parse()
        .map_err(|e| format!("invalid TTL number `{num_part}`: {e}"))?;
    if n <= 0 {
        return Err(format!("TTL must be positive, got `{s}`"));
    }
    let dur = match unit.to_ascii_lowercase().as_str() {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        other => return Err(format!("unknown TTL unit `{other}` (expected s/m/h/d/w)")),
    };
    Ok(dur)
}

/// Parse an absolute expiry instant from either RFC3339 or `YYYY-MM-DD`.
///
/// `YYYY-MM-DD` inputs anchor at `23:59:59Z` (end of UTC day) so that a
/// human writing "`--expires-at 2026-07-02`" gets the whole of July 2nd
/// before expiry fires. RFC3339 inputs are used as-is.
///
/// # Errors
/// Returns a descriptive error when neither format parses.
pub fn parse_expires_at(s: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let ndt = date
            .and_hms_opt(23, 59, 59)
            .ok_or_else(|| format!("invalid date `{s}`"))?;
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    Err(format!(
        "invalid --expires-at `{s}` (expected RFC3339 or YYYY-MM-DD)"
    ))
}

/// What to do when a molecule's `expires_at` is in the past.
///
/// Stored on `MoleculeData.expiry_policy`. Marked `#[non_exhaustive]` so
/// new variants can be added without breaking downstream matchers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExpiryPolicy {
    /// Surface a warning badge; no state change. Safe default.
    #[default]
    Warn,
    /// Transition `pending` → `collapsed` with reason "expired (TTL)".
    /// Applies only to non-running molecules; running molecules degrade
    /// to [`Self::Warn`].
    Collapse,
    /// Escalate — surface a high-visibility badge and flag for human
    /// attention. Gentler than [`Self::Collapse`]; no automatic state
    /// transition.
    Escalate,
}

/// The action chosen by [`evaluate_expiry`] for a given molecule at a
/// given instant.
///
/// This is the output of the pure transition function — callers (e.g.
/// `cs expire`) translate it into concrete effects (status change, badge
/// render, event emission).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryAction {
    /// No expiry applies — either no TTL is set or the deadline is in the
    /// future.
    None,
    /// Emit a warning surface badge; do not change status.
    Warn,
    /// Collapse the molecule with reason "expired (TTL)".
    Collapse,
    /// Escalate — high-visibility badge, flag for human attention.
    Escalate,
}

/// Evaluate the expiry action for a molecule.
///
/// Pure — no I/O, no side effects. `now` is injected so tests can pin
/// the clock and so the real CLI can share one `Utc::now()` across a
/// batch of molecules (deterministic sweeps).
///
/// Returns [`ExpiryAction::None`] when `expires_at` is unset or the
/// deadline is still in the future. When expired, the effective action
/// is derived from `policy` (defaulting to [`ExpiryPolicy::Warn`] when
/// unset) subject to the running-molecule degradation rule.
#[must_use]
pub fn evaluate_expiry(
    expires_at: Option<DateTime<Utc>>,
    policy: Option<ExpiryPolicy>,
    status: MoleculeStatus,
    now: DateTime<Utc>,
) -> ExpiryAction {
    let Some(deadline) = expires_at else {
        return ExpiryAction::None;
    };
    if deadline > now {
        return ExpiryAction::None;
    }
    let effective = policy.unwrap_or_default();
    match effective {
        ExpiryPolicy::Warn => ExpiryAction::Warn,
        ExpiryPolicy::Collapse => {
            // Running molecules never silently collapse (ADR-029 § Invariants).
            if is_pending_or_terminal(status) {
                ExpiryAction::Collapse
            } else {
                ExpiryAction::Warn
            }
        }
        ExpiryPolicy::Escalate => ExpiryAction::Escalate,
    }
}

/// Whether a status permits collapse-on-expiry.
///
/// Only `Pending` molecules may be silently collapsed; terminal statuses
/// are already stable (and an already-collapsed molecule is a no-op —
/// idempotence). Active statuses degrade to `Warn`.
fn is_pending_or_terminal(status: MoleculeStatus) -> bool {
    matches!(
        status,
        MoleculeStatus::Pending
            | MoleculeStatus::Completed
            | MoleculeStatus::Collapsed
            | MoleculeStatus::Frozen
    )
}

/// Format a human-readable badge describing a molecule's TTL state.
///
/// Returns `None` when `expires_at` is unset. When the deadline is in the
/// future, emits `📅 YYYY-MM-DD · Nd left` (or `Nh left` / `<1h left`).
/// When the deadline has passed, emits `⚠️ expired Nd ago` (or
/// `Nh ago` / `<1h ago`). The calendar date is taken from the `expires_at`
/// instant in UTC so operators see the same date they specified on the
/// CLI. `now` is injected so callers can share one clock across a batch
/// of molecules and so tests can pin a deterministic instant.
#[must_use]
pub fn format_expiry_badge(
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<String> {
    let deadline = expires_at?;
    let date = deadline.format("%Y-%m-%d");
    if deadline > now {
        let delta = deadline - now;
        let suffix = humanize_duration_future(delta);
        Some(format!("\u{1f4c5} {date} \u{00b7} {suffix}"))
    } else {
        let delta = now - deadline;
        let suffix = humanize_duration_past(delta);
        Some(format!("\u{26a0}\u{fe0f} expired {suffix}"))
    }
}

/// Format a **clock-invariant** expiry badge for persisted surfaces.
///
/// Unlike [`format_expiry_badge`], this takes no `now`: it renders only the
/// absolute deadline date (`📅 YYYY-MM-DD` in UTC), which is a pure function
/// of `expires_at`. Returns `None` when `expires_at` is unset.
///
/// # Why a second badge
///
/// Persisted surfaces (STATUS.md, ISSUES.md, GitHub Issues) are *derived
/// views*: they are hashed for 3-way divergence detection and committed to
/// git. Embedding the wall-clock-relative countdown from [`format_expiry_badge`]
/// (`Nd left` / `expired Nd ago`) makes the surface renderer depend on ambient
/// `Utc::now()`, so a pure clock advance over an **unchanged** store re-renders
/// different bytes. That flipped `cs reconcile --check` from PASS to FAIL on a
/// still store and broke reconcile's "strictly idempotent pure projection"
/// contract (finding F-C7-1, delib-20260711-9928; CLAUDE.md invariants §8).
///
/// The live countdown and the expired warning are *live-view* concerns —
/// evaluated against the clock at read time by the TUI (`cs peek`), `cs expire`,
/// and `cs verify` via [`format_expiry_badge`] / [`evaluate_expiry`] — and are
/// deliberately kept out of the hashable, git-tracked surface bytes.
#[must_use]
pub fn format_expiry_badge_static(expires_at: Option<DateTime<Utc>>) -> Option<String> {
    let deadline = expires_at?;
    Some(format!("\u{1f4c5} {}", deadline.format("%Y-%m-%d")))
}

fn humanize_duration_future(d: Duration) -> String {
    let days = d.num_days();
    if days >= 1 {
        return format!("{days}d left");
    }
    let hours = d.num_hours();
    if hours >= 1 {
        return format!("{hours}h left");
    }
    "<1h left".to_owned()
}

fn humanize_duration_past(d: Duration) -> String {
    let days = d.num_days();
    if days >= 1 {
        return format!("{days}d ago");
    }
    let hours = d.num_hours();
    if hours >= 1 {
        return format!("{hours}h ago");
    }
    "<1h ago".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn policy_serde_roundtrip_snake_case() {
        for (variant, wire) in [
            (ExpiryPolicy::Warn, "\"warn\""),
            (ExpiryPolicy::Collapse, "\"collapse\""),
            (ExpiryPolicy::Escalate, "\"escalate\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, wire);
            let back: ExpiryPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn policy_default_is_warn() {
        assert_eq!(ExpiryPolicy::default(), ExpiryPolicy::Warn);
    }

    #[test]
    fn no_ttl_means_no_action() {
        let action = evaluate_expiry(
            None,
            Some(ExpiryPolicy::Collapse),
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::None);
    }

    #[test]
    fn future_deadline_is_no_action() {
        let action = evaluate_expiry(
            Some(ts("2026-06-30T00:00:00Z")),
            Some(ExpiryPolicy::Collapse),
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::None);
    }

    #[test]
    fn past_deadline_with_warn_policy_warns() {
        let action = evaluate_expiry(
            Some(ts("2026-01-01T00:00:00Z")),
            Some(ExpiryPolicy::Warn),
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::Warn);
    }

    #[test]
    fn past_deadline_with_collapse_collapses_pending() {
        let action = evaluate_expiry(
            Some(ts("2026-01-01T00:00:00Z")),
            Some(ExpiryPolicy::Collapse),
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::Collapse);
    }

    #[test]
    fn past_deadline_with_collapse_degrades_for_active_molecule() {
        // ADR-029 § Invariants: running molecules never silently collapse.
        let action = evaluate_expiry(
            Some(ts("2026-01-01T00:00:00Z")),
            Some(ExpiryPolicy::Collapse),
            MoleculeStatus::Running,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::Warn);
    }

    #[test]
    fn unset_policy_defaults_to_warn() {
        let action = evaluate_expiry(
            Some(ts("2026-01-01T00:00:00Z")),
            None,
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::Warn);
    }

    #[test]
    fn past_deadline_with_escalate_escalates() {
        let action = evaluate_expiry(
            Some(ts("2026-01-01T00:00:00Z")),
            Some(ExpiryPolicy::Escalate),
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        assert_eq!(action, ExpiryAction::Escalate);
    }

    #[test]
    fn parse_ttl_units() {
        assert_eq!(parse_ttl("7d").unwrap(), Duration::days(7));
        assert_eq!(parse_ttl("24h").unwrap(), Duration::hours(24));
        assert_eq!(parse_ttl("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_ttl("2w").unwrap(), Duration::weeks(2));
        assert_eq!(parse_ttl("45s").unwrap(), Duration::seconds(45));
        assert_eq!(parse_ttl(" 7D ").unwrap(), Duration::days(7));
    }

    #[test]
    fn parse_ttl_rejects_bad_inputs() {
        assert!(parse_ttl("").is_err());
        assert!(parse_ttl("7").is_err());
        assert!(parse_ttl("d").is_err());
        assert!(parse_ttl("0d").is_err());
        assert!(parse_ttl("-1d").is_err());
        assert!(parse_ttl("7x").is_err());
        assert!(parse_ttl("1h30m").is_err());
    }

    #[test]
    fn parse_expires_at_accepts_rfc3339_and_date() {
        let a = parse_expires_at("2026-07-02T00:00:00Z").unwrap();
        assert_eq!(a, ts("2026-07-02T00:00:00Z"));
        let b = parse_expires_at("2026-07-02").unwrap();
        assert_eq!(b, ts("2026-07-02T23:59:59Z"));
    }

    #[test]
    fn parse_expires_at_rejects_garbage() {
        assert!(parse_expires_at("yesterday").is_err());
        assert!(parse_expires_at("2026-13-02").is_err());
    }

    #[test]
    fn evaluation_is_idempotent() {
        // Same inputs → same output, run twice.
        let inputs = (
            Some(ts("2026-01-01T00:00:00Z")),
            Some(ExpiryPolicy::Collapse),
            MoleculeStatus::Pending,
            ts("2026-04-12T00:00:00Z"),
        );
        let a = evaluate_expiry(inputs.0, inputs.1, inputs.2, inputs.3);
        let b = evaluate_expiry(inputs.0, inputs.1, inputs.2, inputs.3);
        assert_eq!(a, b);
    }

    #[test]
    fn badge_none_when_unset() {
        assert_eq!(format_expiry_badge(None, ts("2026-04-12T00:00:00Z")), None);
    }

    #[test]
    fn badge_future_days() {
        let b = format_expiry_badge(Some(ts("2026-07-02T23:59:59Z")), ts("2026-04-12T00:00:00Z"));
        assert_eq!(b.as_deref(), Some("\u{1f4c5} 2026-07-02 \u{00b7} 81d left"));
    }

    #[test]
    fn badge_future_hours() {
        let b = format_expiry_badge(Some(ts("2026-04-12T05:00:00Z")), ts("2026-04-12T00:00:00Z"));
        assert_eq!(b.as_deref(), Some("\u{1f4c5} 2026-04-12 \u{00b7} 5h left"));
    }

    #[test]
    fn badge_past_days() {
        let b = format_expiry_badge(Some(ts("2026-04-09T00:00:00Z")), ts("2026-04-12T00:00:00Z"));
        assert_eq!(b.as_deref(), Some("\u{26a0}\u{fe0f} expired 3d ago"));
    }

    #[test]
    fn badge_past_hours() {
        let b = format_expiry_badge(Some(ts("2026-04-11T20:00:00Z")), ts("2026-04-12T00:00:00Z"));
        assert_eq!(b.as_deref(), Some("\u{26a0}\u{fe0f} expired 4h ago"));
    }

    #[test]
    fn static_badge_none_when_unset() {
        assert_eq!(format_expiry_badge_static(None), None);
    }

    #[test]
    fn static_badge_is_absolute_date_only() {
        // The static badge is a pure function of `expires_at`: only the UTC
        // calendar date, no `now`, no countdown — the shape persisted surfaces
        // hash and commit (F-C7-1).
        assert_eq!(
            format_expiry_badge_static(Some(ts("2026-07-02T23:59:59Z"))).as_deref(),
            Some("\u{1f4c5} 2026-07-02")
        );
        // A past deadline renders identically — no `expired … ago` wording,
        // because that verdict is a live-clock concern, not a stored fact.
        let past = format_expiry_badge_static(Some(ts("2026-01-01T00:00:00Z")));
        assert_eq!(past.as_deref(), Some("\u{1f4c5} 2026-01-01"));
        assert!(!past.unwrap().contains("expired"));
    }
}
