// SPDX-License-Identifier: AGPL-3.0-only

//! Account allowance, reset and pace projected from recorded provider
//! readings (issue #242).
//!
//! The readings are the shapes the providers write: a Codex rollout
//! `token_count` event with its `rate_limits` object (synthetic ids, numbers
//! chosen so the expected pace is easy to check by hand) and a Claude Code
//! status-line sample. Every test supplies its own clock.

use chrono::{DateTime, Duration, Utc};
use cosmon_core::plan_observation::{claude_plan, codex_plan, PlanSource};
use cosmon_core::usage::{Freshness, NonNegativeFinite, TokenCount, UsageRecord};
use cosmon_observability::account_usage::{
    format_account_window, project_accounts, AccountWindow, Pace, PaceUnknown,
};

/// Window of seven days that started at 2026-11-26T00:00:00Z.
const RESETS_AT: i64 = 1_796_256_000; // 2026-12-03T00:00:00Z

fn time(s: &str) -> DateTime<Utc> {
    s.parse().expect("fixture time")
}

/// One Codex `token_count` event as a rollout records it.
fn codex_line(observed: &str, used_percent: f64, resets_at: Option<i64>, window: &str) -> String {
    let resets = resets_at.map_or_else(|| "null".to_owned(), |r| r.to_string());
    format!(
        r#"{{"timestamp":"{observed}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"reasoning_output_tokens":10,"total_tokens":1050}}}},"rate_limits":{{"limit_id":"codex","limit_name":null,"primary":{{"used_percent":{used_percent},"window_minutes":{window},"resets_at":{resets}}},"secondary":null,"credits":{{"has_credits":false,"unlimited":false,"balance":"0"}},"plan_type":"pro","rate_limit_reached_type":null}}}}}}"#
    )
}

/// A canonical record for `worker` whose plan is the given reading.
fn record(worker: &str, history: &str, plan: cosmon_core::usage::PlanUsage) -> UsageRecord {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../cosmon-core/tests/fixtures/usage/usage_observed_v1.json"
    ))
    .expect("fixture json");
    value["usage"]["subject"]["worker_id"] = worker.into();
    value["usage"]["subject"]["history"]["id"] = history.into();
    let mut record: UsageRecord =
        serde_json::from_value(value["usage"].clone()).expect("fixture record");
    record.plan = plan;
    record
}

fn codex_record(worker: &str, line: &str, captured: DateTime<Utc>) -> UsageRecord {
    record(
        worker,
        &format!("h-{worker}"),
        codex_plan(line, captured).plan,
    )
}

fn only(accounts: &[AccountWindow]) -> &AccountWindow {
    assert_eq!(accounts.len(), 1, "{accounts:#?}");
    &accounts[0]
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn a_codex_reading_ahead_of_even_use_projects_its_exhaustion() {
    // 42 h into the week (25 % of it) the account has used 50 %.
    let observed = "2026-11-27T18:00:00Z";
    let now = time(observed) + Duration::minutes(3);
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, now);
    let account = only(&accounts);

    assert_eq!(account.provider.as_deref(), Some("openai"));
    assert_eq!(account.window_minutes, Some(10_080));
    close(account.used_percent, 50.0);
    close(account.left_percent.expect("left known"), 50.0);
    assert_eq!(account.resets_at.map(|t| t.timestamp()), Some(RESETS_AT));
    // 2026-12-03T00:00Z minus 2026-11-27T18:03Z.
    assert_eq!(account.resets_in_secs, Some(126 * 3600 - 180));
    assert_eq!(account.age_secs, Some(180));
    assert_eq!(account.freshness, Freshness::Fresh);
    let Pace::Known {
        even_use_percent,
        deficit_percent,
        runs_out_at,
        runs_out_in_secs,
    } = &account.pace
    else {
        panic!("pace should be known: {:?}", account.pace);
    };
    close(*even_use_percent, 25.0);
    close(*deficit_percent, 25.0);
    // 50 % in 42 h: the other 50 % lasts 42 h more, before the reset.
    assert_eq!(*runs_out_at, Some(time("2026-11-29T12:00:00Z")));
    assert_eq!(*runs_out_in_secs, Some(42 * 3600 - 180));
    let credits = account.credits.as_ref().expect("credits reported");
    assert_eq!(credits.balance.map(NonNegativeFinite::get), Some(0.0));

    let line = format_account_window(account);
    assert_eq!(
        line,
        "openai 7d: 50% left · resets in 5d 5h · 25% in deficit, runs out in 1d 17h \
         · credits 0 · seen 3m ago",
    );
}

#[test]
fn a_reading_behind_even_use_lasts_until_the_reset() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 10.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed));
    let account = only(&accounts);
    let Pace::Known {
        deficit_percent,
        runs_out_at,
        runs_out_in_secs,
        ..
    } = &account.pace
    else {
        panic!("pace should be known: {:?}", account.pace);
    };
    close(*deficit_percent, -15.0);
    assert_eq!(*runs_out_at, None);
    assert_eq!(*runs_out_in_secs, None);
    assert!(
        format_account_window(account).contains("15% in reserve, lasts until reset"),
        "{}",
        format_account_window(account)
    );
}

#[test]
fn an_exhausted_window_says_so_instead_of_projecting() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 100.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed) + Duration::minutes(1));
    let account = only(&accounts);
    close(account.left_percent.expect("left known"), 0.0);
    let Pace::Known {
        runs_out_in_secs, ..
    } = &account.pace
    else {
        panic!("pace should be known: {:?}", account.pace);
    };
    assert_eq!(*runs_out_in_secs, Some(0));
    assert!(
        format_account_window(account).contains("exhausted"),
        "{}",
        format_account_window(account)
    );
}

#[test]
fn an_old_reading_is_shown_as_stale_with_its_age() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed) + Duration::hours(2));
    let account = only(&accounts);
    assert_eq!(account.freshness, Freshness::Stale);
    assert_eq!(account.age_secs, Some(7200));
    let line = format_account_window(account);
    assert!(line.ends_with("· stale, seen 2h 0m ago"), "{line}");
}

#[test]
fn a_reading_past_its_reset_has_no_allowance_left_to_report() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let now = time("2026-12-03T00:10:00Z");
    let accounts = project_accounts(&records, now);
    let account = only(&accounts);
    assert_eq!(account.freshness, Freshness::Reset);
    // The new window was not observed: its use is unknown, never 0 %.
    assert_eq!(account.left_percent, None);
    assert_eq!(
        account.pace,
        Pace::Unknown {
            reason: PaceUnknown::WindowReset
        }
    );
    assert_eq!(
        format_account_window(account),
        "openai 7d: reset 10m ago, not observed since"
    );
}

#[test]
fn a_window_without_a_reset_time_keeps_its_use_but_not_its_pace() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 50.0, None, "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed));
    let account = only(&accounts);
    close(account.left_percent.expect("left known"), 50.0);
    assert_eq!(account.resets_in_secs, None);
    assert_eq!(
        account.pace,
        Pace::Unknown {
            reason: PaceUnknown::ResetTimeUnknown
        }
    );
    let line = format_account_window(account);
    assert!(line.contains("reset time unknown · pace unknown"), "{line}");
}

#[test]
fn a_claude_status_line_sample_projects_the_weekly_window() {
    // As the status-line hook stores it: no five-hour window, the weekly
    // window at 99 %, the sample's capture time as the observation time.
    let captured = time("2026-11-27T08:50:52Z");
    let sample = claude_plan(
        r#"{"rate_limits":{"seven_day":{"used_percentage":99,"resets_at":1795982400}}}"#,
        PlanSource::ClaudeStatusLine,
        captured,
    );
    let records = [record("w-claude", "h-claude", sample.plan)];
    let accounts = project_accounts(&records, captured + Duration::minutes(1));
    let account = only(&accounts);
    assert_eq!(account.provider.as_deref(), Some("anthropic"));
    assert_eq!(account.window_minutes, Some(10_080));
    close(account.left_percent.expect("left known"), 1.0);
    assert_eq!(account.observed_at, Some(captured));
    assert_eq!(account.credits, None);
    let line = format_account_window(account);
    assert!(
        line.starts_with("anthropic 7d: 1% left · resets in 2d 11h"),
        "{line}"
    );
    assert!(line.contains("in deficit, runs out in"), "{line}");
    assert!(!line.contains("credits"), "{line}");
}

#[test]
fn workers_on_one_account_share_one_figure_and_split_their_tokens() {
    let earlier = "2026-11-27T17:00:00Z";
    let later = "2026-11-27T18:00:00Z";
    let mut first = codex_record(
        "w-one",
        &codex_line(earlier, 48.0, Some(RESETS_AT), "10080"),
        time(earlier),
    );
    let mut second = codex_record(
        "w-two",
        &codex_line(later, 50.0, Some(RESETS_AT), "10080"),
        time(later),
    );
    // Uncached input plus output: 300 for the first worker, 100 for the second.
    first.tokens.input_tokens = TokenCount::Measured { tokens: 1_000 };
    first.tokens.cached_input_tokens = TokenCount::Measured { tokens: 800 };
    first.tokens.output_tokens = TokenCount::Measured { tokens: 100 };
    second.tokens.input_tokens = TokenCount::Measured { tokens: 500 };
    second.tokens.cached_input_tokens = TokenCount::Measured { tokens: 450 };
    second.tokens.output_tokens = TokenCount::Measured { tokens: 50 };

    let accounts = project_accounts(&[first, second], time(later));
    let account = only(&accounts);
    // The newest reading speaks for the account.
    close(account.used_percent, 50.0);
    assert!(!account.conflicting_readings);
    let shares: Vec<(&str, Option<u64>, Option<f64>)> = account
        .workers
        .iter()
        .map(|w| (w.worker_id.as_str(), w.tokens, w.share_percent))
        .collect();
    assert_eq!(
        shares,
        vec![
            ("w-one", Some(300), Some(75.0)),
            ("w-two", Some(100), Some(25.0)),
        ]
    );
}

#[test]
fn a_worker_with_unknown_tokens_makes_every_share_unknown() {
    let observed = "2026-11-27T18:00:00Z";
    let line = codex_line(observed, 50.0, Some(RESETS_AT), "10080");
    let known = codex_record("w-one", &line, time(observed));
    let mut unknown = codex_record("w-two", &line, time(observed));
    unknown.tokens.output_tokens = TokenCount::Unavailable {
        reason: cosmon_core::usage::UnavailableReason::NotObserved,
    };
    let accounts = project_accounts(&[known, unknown], time(observed));
    let account = only(&accounts);
    assert!(
        account.workers.iter().all(|w| w.share_percent.is_none()),
        "{:?}",
        account.workers
    );
    assert_eq!(account.workers[1].tokens, None);
}

#[test]
fn readings_that_go_backwards_are_flagged_rather_than_merged_silently() {
    // Use within one window never decreases. A newer, lower reading under the
    // same reset time means two accounts share it, and the figure is unsafe.
    let earlier = "2026-11-27T17:00:00Z";
    let later = "2026-11-27T18:00:00Z";
    let records = [
        codex_record(
            "w-one",
            &codex_line(earlier, 60.0, Some(RESETS_AT), "10080"),
            time(earlier),
        ),
        codex_record(
            "w-two",
            &codex_line(later, 20.0, Some(RESETS_AT), "10080"),
            time(later),
        ),
    ];
    let accounts = project_accounts(&records, time(later));
    let account = only(&accounts);
    assert!(account.conflicting_readings);
    assert!(
        format_account_window(account).contains("readings disagree"),
        "{}",
        format_account_window(account)
    );
}

#[test]
fn different_windows_stay_separate_accounts() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [
        codex_record(
            "w-one",
            &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
            time(observed),
        ),
        codex_record(
            "w-two",
            &codex_line(observed, 30.0, Some(RESETS_AT + 3600), "10080"),
            time(observed),
        ),
    ];
    assert_eq!(project_accounts(&records, time(observed)).len(), 2);
}

#[test]
fn no_rate_limit_reading_is_no_account_not_a_zero_one() {
    let observed = "2026-11-27T18:00:00Z";
    let token_only = r#"{"timestamp":"2026-11-27T18:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0,"total_tokens":2}},"rate_limits":null}}"#;
    let records = [codex_record("w-one", token_only, time(observed))];
    assert_eq!(project_accounts(&records, time(observed)).len(), 0);
}

/// A vanishingly small use projects an exhaustion far past any calendar; it
/// must read as lasting until the reset, not overflow the clock arithmetic.
#[test]
fn a_negligible_use_lasts_until_reset_without_overflowing() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 1e-12, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed));
    let Pace::Known { runs_out_at, .. } = &only(&accounts).pace else {
        panic!("pace should be known");
    };
    assert_eq!(*runs_out_at, None);
}

/// The window length is provider data: an absurd one leaves the pace
/// unknown instead of overflowing the clock arithmetic.
#[test]
fn an_absurd_window_length_leaves_the_pace_unknown() {
    let observed = "2026-11-27T18:00:00Z";
    for window in ["9000000000000000", "18446744073709551615"] {
        let records = [codex_record(
            "w-codex",
            &codex_line(observed, 50.0, Some(RESETS_AT), window),
            time(observed),
        )];
        let accounts = project_accounts(&records, time(observed));
        assert_eq!(
            only(&accounts).pace,
            Pace::Unknown {
                reason: PaceUnknown::WindowLengthUnknown
            },
            "window {window}"
        );
    }
}

/// A reading timed before its window opened, or after it closed, cannot
/// place itself in the window: its pace is unknown, not extrapolated.
#[test]
fn a_reading_outside_its_window_has_no_pace() {
    // RESETS_AT minus seven days is 2026-11-26T00:00:00Z.
    for observed in ["2026-11-25T23:00:00Z", "2026-11-26T00:00:00Z"] {
        let records = [codex_record(
            "w-codex",
            &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
            time(observed),
        )];
        let accounts = project_accounts(&records, time(observed));
        assert_eq!(
            only(&accounts).pace,
            Pace::Unknown {
                reason: PaceUnknown::ObservationOutsideWindow
            },
            "{observed}"
        );
    }
    // The last second of the window is still inside it.
    let observed = "2026-12-02T23:59:59Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed));
    assert!(
        matches!(only(&accounts).pace, Pace::Known { .. }),
        "{:?}",
        only(&accounts).pace
    );
}

/// The window's last instant is the reset itself: a reading made there is
/// inside (even use 100 %), one made after it is outside. Both are read from
/// a clock that has not yet reached the reset, as a skewed clock would.
#[test]
fn the_reset_instant_closes_the_window() {
    let at_reset = "2026-12-03T00:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(at_reset, 50.0, Some(RESETS_AT), "10080"),
        time(at_reset),
    )];
    let accounts = project_accounts(&records, time(at_reset) - Duration::minutes(1));
    let Pace::Known {
        even_use_percent, ..
    } = only(&accounts).pace
    else {
        panic!("pace should be known: {:?}", only(&accounts).pace);
    };
    close(even_use_percent, 100.0);

    let after = "2026-12-03T01:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(after, 50.0, Some(RESETS_AT), "10080"),
        time(after),
    )];
    let accounts = project_accounts(&records, time(at_reset) - Duration::minutes(1));
    assert_eq!(
        only(&accounts).pace,
        Pace::Unknown {
            reason: PaceUnknown::ObservationOutsideWindow
        }
    );
}

/// A reading timestamped after the reader's clock (a skewed clock) has no
/// defensible age class: it is unknown, neither fresh nor stale.
#[test]
fn a_reading_from_the_future_has_unknown_freshness() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 50.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed) - Duration::minutes(5));
    assert_eq!(only(&accounts).freshness, Freshness::Unknown);
    // Exactly the stale threshold is still fresh; one second more is stale.
    let at = |secs| {
        let accounts = project_accounts(&records, time(observed) + Duration::seconds(secs));
        only(&accounts).freshness
    };
    assert_eq!(at(600), Freshness::Fresh);
    assert_eq!(at(601), Freshness::Stale);
    assert_eq!(at(0), Freshness::Fresh);
}

/// Zero use runs out never: it lasts until the reset, fully in reserve.
#[test]
fn zero_use_lasts_until_reset() {
    let observed = "2026-11-27T18:00:00Z";
    let records = [codex_record(
        "w-codex",
        &codex_line(observed, 0.0, Some(RESETS_AT), "10080"),
        time(observed),
    )];
    let accounts = project_accounts(&records, time(observed));
    let Pace::Known {
        deficit_percent,
        runs_out_at,
        ..
    } = &only(&accounts).pace
    else {
        panic!("pace should be known");
    };
    close(*deficit_percent, -25.0);
    assert_eq!(*runs_out_at, None);
}
