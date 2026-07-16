// SPDX-License-Identifier: AGPL-3.0-only

//! `run` — `do` bracketed by two quota snapshots, so the operator sees
//! the cost THIS run charged against their tenant bucket.
//!
//! PURELY client-side, exactly like [`crate::do_flow`]: `run` composes
//! the same three §8p routes the `do` flow already dials
//! (`POST /v1/molecules`, `POST /v1/molecules/{id}/tackle`,
//! `GET /v1/molecules/{id}`) and brackets them with two reads of the
//! ALREADY-frozen `GET /v1/quota` snapshot. Zero new routes; doctrine
//! §5.1 untouched.
//!
//! # What "attributed cost" means here
//!
//! The only cost surface the frozen v1 API exposes is the leaky-bucket
//! rate-limit snapshot (`GET /v1/quota`). `run` reads it once before the
//! first spend and once after the follow loop terminates, then reports
//! the DELTA: how far the run pushed the caller's bucket level up (and
//! how much head-room it consumed). This is an honest attribution of the
//! quota the run charged to THIS caller — not a dollar figure (the API
//! never vends one) and not a gross request count.
//!
//! The bucket leaks continuously (`leak_per_minute`), so a long-running
//! follow can leak more than the run charged and the level delta can go
//! NEGATIVE. That is not a bug: it is the truthful net figure. The
//! renderer says so in one line rather than hiding it behind a `max(0)`.
//!
//! # Best-effort, never fatal
//!
//! The quota bracket is best-effort, the same discipline as the `do`
//! flow's SSE tail: if either snapshot fails (an older adapter, a
//! transient error), the WORK still ran and its outcome is reported —
//! only the cost line degrades to "unavailable". The run's purpose is to
//! do the work; the cost read is a courtesy on top.

use crate::client::{Client, QuotaResponse};
use crate::do_flow::{run_do, DoOptions, DoOutcome, GuardMemory};
use crate::error::Result;

/// The quota a single `run` charged against the caller's bucket,
/// computed from a before/after pair of [`QuotaResponse`] snapshots.
///
/// `Serialize` so `--json` emits it verbatim; every field is a raw
/// number the caller can re-decide for itself (the same "raw signals,
/// not an opaque verdict" stance as [`crate::client::Liveness`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CostDelta {
    /// Bucket level (fractional) before the first spend.
    pub before_level: f64,
    /// Bucket level (fractional) after the follow loop terminated.
    pub after_level: f64,
    /// `after_level − before_level`. Positive = the run charged more
    /// than the bucket leaked back; negative = the follow outlasted the
    /// charge and the bucket net-drained. The truthful net figure.
    pub level_delta: f64,
    /// `floor(burst_capacity − bucket_level)` before the run — the
    /// `X-RateLimit-Remaining` value at the start.
    pub before_remaining: i64,
    /// Same, after the run.
    pub after_remaining: i64,
    /// `after_remaining − before_remaining`. Negative = the run consumed
    /// head-room; positive = leak returned more than the run spent.
    pub remaining_delta: i64,
    /// Bucket capacity (the `limits.burst_capacity` echoed on the AFTER
    /// snapshot), for rendering the level against its ceiling.
    pub burst_capacity: i64,
}

impl CostDelta {
    /// Render the cost delta as the ASCII block the CLI prints after a
    /// `run`. ASCII-only, same convention as `render_quota_table` — so
    /// it survives `| column -t` across the operator's shells.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        out.push_str("attributed cost (quota delta for this run):\n");
        // Infallible: `writeln!` to a `String` never errors.
        let _ = writeln!(
            out,
            "  bucket level   : {:>7.2} -> {:>7.2}  (delta {:+.2} of {} capacity)",
            self.before_level, self.after_level, self.level_delta, self.burst_capacity,
        );
        let _ = writeln!(
            out,
            "  remaining      : {:>7} -> {:>7}  (delta {:+})",
            self.before_remaining, self.after_remaining, self.remaining_delta,
        );
        out.push_str(
            "  note: the bucket leaks continuously; on a long follow the leak can\n  \
             exceed the charge, so the delta is a net figure, not a request count.",
        );
        out
    }
}

/// Compute the cost a run charged from its before/after quota snapshots.
///
/// Pure — no I/O, no clock. The `after` snapshot supplies the capacity
/// (both snapshots carry the same configured ceiling; reading it off
/// `after` keeps the function total even if the limits block ever
/// evolves mid-flight).
#[must_use]
pub fn attribute(before: &QuotaResponse, after: &QuotaResponse) -> CostDelta {
    CostDelta {
        before_level: before.current.bucket_level,
        after_level: after.current.bucket_level,
        level_delta: after.current.bucket_level - before.current.bucket_level,
        before_remaining: before.remaining,
        after_remaining: after.remaining,
        remaining_delta: after.remaining - before.remaining,
        burst_capacity: after.limits.burst_capacity,
    }
}

/// What one `run` produced: the underlying `do` outcome plus the cost
/// attribution and the raw snapshots that backed it.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The composed nucleate → tackle → follow outcome.
    pub do_outcome: DoOutcome,
    /// The attributed cost, or `None` when either quota snapshot failed
    /// (older adapter / transient error) — the work still ran.
    pub cost: Option<CostDelta>,
}

/// Run the `do` composition bracketed by two quota snapshots.
///
/// The quota reads are best-effort: a failure degrades [`RunOutcome::cost`]
/// to `None` and is otherwise silent — it never fails the run. The
/// inner `do` flow's errors (wire failures, a declined credit guard)
/// propagate unchanged: a run that cannot do the work is a hard error,
/// a run that cannot price the work is not.
///
/// `confirm` and `progress` are the same interactive/observer edges
/// [`run_do`] takes; this wrapper adds nothing to them.
///
/// # Errors
///
/// Propagates any error from the inner [`run_do`] (nucleate / tackle /
/// observe wire errors, or [`crate::error::Error::Auth`] on a declined
/// credit guard). Quota-snapshot errors are swallowed by design.
pub async fn run_with_cost<G, C, P>(
    client: &Client,
    opts: DoOptions,
    guard: &mut G,
    confirm: C,
    progress: P,
) -> Result<RunOutcome>
where
    G: GuardMemory,
    C: FnMut(&str) -> std::io::Result<bool>,
    P: FnMut(&str),
{
    // Snapshot BEFORE the first spend (best-effort).
    let before = client.quota().await.ok();

    let do_outcome = run_do(client, opts, guard, confirm, progress).await?;

    // Snapshot AFTER the follow loop terminated (best-effort).
    let after = client.quota().await.ok();

    let cost = match (before, after) {
        (Some(b), Some(a)) => Some(attribute(&b, &a)),
        _ => None,
    };

    Ok(RunOutcome { do_outcome, cost })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{QuotaCurrent, QuotaLimits};

    fn snap(level: f64, floor: i64, remaining: i64) -> QuotaResponse {
        QuotaResponse {
            request_id: "req-test".into(),
            limits: QuotaLimits {
                burst_capacity: 30,
                leak_per_minute: 10.0,
                leak_per_hour: 600.0,
            },
            current: QuotaCurrent {
                bucket_level: level,
                bucket_level_floor: floor,
            },
            remaining,
            reset_at: "2026-06-25T16:00:00Z".into(),
        }
    }

    #[test]
    fn attribute_reports_a_positive_charge_when_the_bucket_filled() {
        // Two admissions charged, no leak between the snapshots: the
        // bucket rose by 2 and head-room dropped by 2.
        let delta = attribute(&snap(4.0, 4, 26), &snap(6.0, 6, 24));
        assert!((delta.level_delta - 2.0).abs() < 1e-9);
        assert_eq!(delta.remaining_delta, -2);
        assert_eq!(delta.burst_capacity, 30);
    }

    #[test]
    fn attribute_keeps_a_negative_delta_when_leak_outran_the_charge() {
        // A long follow: the bucket net-drained. The delta stays
        // negative — the honest net figure, never floored to zero.
        let delta = attribute(&snap(9.0, 9, 21), &snap(3.0, 3, 27));
        assert!(delta.level_delta < 0.0, "net drain stays negative");
        assert_eq!(delta.remaining_delta, 6);
    }

    #[test]
    fn render_names_the_leak_caveat_and_both_deltas() {
        let block = attribute(&snap(4.0, 4, 26), &snap(6.0, 6, 24)).render();
        assert!(block.contains("attributed cost"));
        assert!(block.contains("bucket level"));
        assert!(block.contains("remaining"));
        assert!(
            block.contains("leaks continuously"),
            "the caveat is load-bearing"
        );
    }
}
