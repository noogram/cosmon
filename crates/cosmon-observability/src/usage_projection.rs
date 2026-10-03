// SPDX-License-Identifier: AGPL-3.0-only

//! Pure projections over canonical usage records.
//!
//! The helpers in this module are shared by human and machine-facing
//! observability surfaces. They deduplicate cumulative histories before
//! addition and retain plan windows as observations: account percentages are
//! never summed.

use std::collections::BTreeMap;

use cosmon_core::usage::{
    ApiEquivalent, Availability, Freshness, ObservationScope, PlanWindowObservation,
    PricingCoverage, TokenCount, UsageHistory, UsageRecord,
};

/// Coverage-aware API-equivalent subtotal across distinct usage histories.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ApiEquivalentSummary {
    /// Sum of available API-equivalent estimates, absent when none are known.
    pub amount_usd: Option<f64>,
    /// Distinct histories represented by an available estimate.
    pub priced_histories: usize,
    /// All distinct histories in the input.
    pub total_histories: usize,
    /// True when every represented history has complete pricing coverage.
    pub complete: bool,
}

/// Stable key for a cumulative record.
fn history_key(record: &UsageRecord) -> String {
    match &record.subject.history {
        UsageHistory::Known { id } => format!("history:{id}"),
        UsageHistory::LegacyUnknown => match &record.observation_id {
            cosmon_core::usage::UsageObservationId::Known { id } => {
                format!("observation:{id}")
            }
            cosmon_core::usage::UsageObservationId::LegacyUnknown => {
                format!("legacy-worker:{}", record.subject.worker_id)
            }
            _ => format!("future-worker:{}", record.subject.worker_id),
        },
        _ => format!("future-worker:{}", record.subject.worker_id),
    }
}

fn measured_total(record: &UsageRecord) -> u64 {
    record
        .tokens
        .input_tokens
        .measured()
        .unwrap_or_default()
        .saturating_add(record.tokens.output_tokens.measured().unwrap_or_default())
}

fn observation_time(record: &UsageRecord) -> Option<chrono::DateTime<chrono::Utc>> {
    record
        .tokens
        .provenance
        .observed_at
        .or(record.tokens.provenance.captured_at)
}

/// How many token categories a record measures.
///
/// A cumulative history can lose a category: once one sample omits it, the
/// running total stops being a total and the category turns unavailable.
fn measured_categories(record: &UsageRecord) -> usize {
    let tokens = &record.tokens;
    [
        tokens.input_tokens,
        tokens.cached_input_tokens,
        tokens.cache_write_tokens,
        tokens.output_tokens,
        tokens.reasoning_output_tokens,
    ]
    .iter()
    .filter(|count| count.measured().is_some())
    .count()
}

/// Whether `record` is a later observation of the same history that knows
/// strictly less than `current`.
///
/// Both records must carry a source time and the newer one must have lost a
/// measured category. A smaller total alone is not enough: that is what an
/// out-of-order replay of an old record looks like, and it must not win.
fn is_later_degradation(record: &UsageRecord, current: &UsageRecord) -> bool {
    match (observation_time(record), observation_time(current)) {
        (Some(new), Some(old)) => {
            new > old && measured_categories(record) < measured_categories(current)
        }
        _ => false,
    }
}

/// Keep one latest cumulative record per history in stable key order.
///
/// A larger measured total wins. Equal totals use the newest supplied source
/// time, which lets a plan-only refresh replace stale window metadata without
/// adding the cumulative tokens again. A later record that lost a measured
/// category also wins even with a smaller total: its totals are the honest ones
/// once a gap has made an earlier category unknowable, and keeping the earlier
/// record would present a partial sum as complete.
#[must_use]
pub fn deduplicate_histories<'a>(
    records: impl IntoIterator<Item = &'a UsageRecord>,
) -> Vec<&'a UsageRecord> {
    let mut by_history: BTreeMap<String, &'a UsageRecord> = BTreeMap::new();
    for record in records {
        let key = history_key(record);
        let replace = by_history.get(&key).is_none_or(|current| {
            is_later_degradation(record, current)
                || (!is_later_degradation(current, record)
                    && measured_total(record) > measured_total(current))
                || (measured_total(record) == measured_total(current)
                    && observation_time(record) > observation_time(current))
        });
        if replace {
            by_history.insert(key, record);
        }
    }
    by_history.into_values().collect()
}

/// Sum API-equivalent estimates over distinct histories while retaining
/// coverage. All-unavailable input yields `amount_usd: None`, never zero.
#[must_use]
pub fn summarize_api_equivalent(records: &[UsageRecord]) -> ApiEquivalentSummary {
    let records = deduplicate_histories(records);
    let mut amount = None::<f64>;
    let mut priced = 0;
    let mut complete = !records.is_empty();
    for record in &records {
        match &record.api_equivalent {
            ApiEquivalent::Estimated {
                amount_usd,
                coverage,
                ..
            } => {
                amount = Some(amount.unwrap_or_default() + amount_usd.get());
                priced += 1;
                complete &= matches!(coverage, PricingCoverage::Complete);
            }
            _ => complete = false,
        }
    }
    ApiEquivalentSummary {
        amount_usd: amount,
        priced_histories: priced,
        total_histories: records.len(),
        complete,
    }
}

fn window_key(window: &PlanWindowObservation) -> String {
    format!(
        "{}|{}|{:?}|{:?}|{:?}|{:?}|{}",
        window.provenance.provider.as_deref().unwrap_or("unknown"),
        window.meter,
        window.window.id,
        window.window.duration_minutes,
        window.window.resets_at,
        window.scope,
        window.utilization.get(),
    )
}

/// Return distinct plan-window observations without adding utilization.
///
/// Only byte-equivalent semantic readings collapse. With no private account
/// correlation key, two different values remain separate observations.
#[must_use]
pub fn distinct_plan_windows(records: &[UsageRecord]) -> Vec<&PlanWindowObservation> {
    let mut windows = BTreeMap::new();
    for record in deduplicate_histories(records) {
        for window in &record.plan.windows {
            windows.entry(window_key(window)).or_insert(window);
        }
    }
    windows.into_values().collect()
}

/// Format the coverage-aware monetary subtotal for a human surface.
#[must_use]
pub fn format_api_equivalent(records: &[UsageRecord]) -> String {
    let summary = summarize_api_equivalent(records);
    match summary.amount_usd {
        None => "API equiv. unavailable".to_owned(),
        Some(amount) if summary.complete => format!("API equiv. ${amount:.2} complete"),
        Some(amount) => format!(
            "API equiv. ${amount:.2} partial ({}/{})",
            summary.priced_histories, summary.total_histories
        ),
    }
}

/// Format one account/worker plan window without changing its scope.
#[must_use]
pub fn format_plan_window(window: &PlanWindowObservation) -> String {
    let scope = match window.scope {
        ObservationScope::Account => "account",
        ObservationScope::Worker => "worker",
        ObservationScope::UsageHistory => "history",
        _ => "scope unknown",
    };
    let duration = window.window.duration_minutes.map_or_else(
        || window.meter.clone(),
        |minutes| {
            if minutes % 10_080 == 0 {
                format!("{}d", minutes / 1_440)
            } else if minutes % 60 == 0 {
                format!("{}h", minutes / 60)
            } else {
                format!("{minutes}m")
            }
        },
    );
    if window.freshness == Freshness::Reset {
        return format!("{scope} {duration} reset");
    }
    let status = if window.freshness == Freshness::Stale {
        " stale"
    } else {
        ""
    };
    format!(
        "{scope} {duration} used {:.0}%{status}",
        window.utilization.get() * 100.0
    )
}

/// State whether any record establishes worker-attributed plan use.
#[must_use]
pub fn format_worker_plan(records: &[UsageRecord]) -> String {
    let available = deduplicate_histories(records)
        .into_iter()
        .find_map(|record| {
            if let Availability::Available { value } = &record.plan.worker_attributed {
                Some(value.utilization.get())
            } else {
                None
            }
        });
    available.map_or_else(
        || "worker plan use unavailable".to_owned(),
        |value| format!("worker plan use {:.0}%", value * 100.0),
    )
}

/// Sum one measured token category over distinct cumulative histories.
#[must_use]
pub fn sum_token_category(
    records: &[UsageRecord],
    select: impl Fn(&UsageRecord) -> TokenCount,
) -> u64 {
    deduplicate_histories(records)
        .into_iter()
        .filter_map(|record| select(record).measured())
        .fold(0_u64, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> UsageRecord {
        serde_json::from_str(include_str!(
            "../../cosmon-core/tests/fixtures/usage/usage_observed_v1.json"
        ))
        .and_then(|value: serde_json::Value| serde_json::from_value(value["usage"].clone()))
        .unwrap()
    }

    #[test]
    fn repeated_cumulative_history_is_not_added_twice() {
        let first = fixture();
        let mut later = first.clone();
        later.tokens.input_tokens = TokenCount::Measured { tokens: 1_300 };
        if let ApiEquivalent::Estimated { amount_usd, .. } = &mut later.api_equivalent {
            *amount_usd = cosmon_core::usage::NonNegativeFinite::new(1.50, "fixture").unwrap();
        }
        let summary = summarize_api_equivalent(&[first, later]);
        assert_eq!(summary.amount_usd, Some(1.50));
        assert_eq!(summary.priced_histories, 1);
        assert_eq!(summary.total_histories, 1);
    }

    #[test]
    fn a_later_record_that_lost_a_category_replaces_the_fuller_earlier_one() {
        use chrono::{TimeZone, Utc};
        let mut earlier = fixture();
        // `observed_at` outranks `captured_at`; clear it so the capture times decide.
        earlier.tokens.provenance.observed_at = None;
        earlier.tokens.input_tokens = TokenCount::Measured { tokens: 900 };
        earlier.tokens.provenance.captured_at =
            Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        let mut later = earlier.clone();
        later.tokens.input_tokens = TokenCount::Unavailable {
            reason: cosmon_core::usage::UnavailableReason::NotObserved,
        };
        later.tokens.provenance.captured_at =
            Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 1, 0).unwrap());

        let kept = deduplicate_histories([&earlier, &later]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].tokens.input_tokens, later.tokens.input_tokens);

        // An out-of-order replay of the older, fuller record must not win back.
        let kept = deduplicate_histories([&later, &earlier]);
        assert_eq!(kept[0].tokens.input_tokens, later.tokens.input_tokens);
    }

    #[test]
    fn all_unavailable_is_not_a_zero_dollar_total() {
        let mut record = fixture();
        record.api_equivalent = ApiEquivalent::Unavailable {
            reason: cosmon_core::usage::UnavailableReason::NotObserved,
        };
        let summary = summarize_api_equivalent(&[record]);
        assert_eq!(summary.amount_usd, None);
        assert_eq!(format_api_equivalent(&[]), "API equiv. unavailable");
    }

    #[test]
    fn mixed_known_and_unknown_histories_publish_coverage() {
        let known = fixture();
        let mut unknown = fixture();
        unknown.subject.history = UsageHistory::Known {
            id: "unpriced-history".to_owned(),
        };
        unknown.api_equivalent = ApiEquivalent::Unavailable {
            reason: cosmon_core::usage::UnavailableReason::MissingPricingCategory,
        };
        let records = [known, unknown];

        let summary = summarize_api_equivalent(&records);
        assert_eq!(summary.amount_usd, Some(1.25));
        assert_eq!(summary.priced_histories, 1);
        assert_eq!(summary.total_histories, 2);
        assert!(!summary.complete);
        assert_eq!(
            format_api_equivalent(&records),
            "API equiv. $1.25 partial (1/2)"
        );
    }

    #[test]
    fn account_windows_are_deduplicated_but_never_summed() {
        let first = fixture();
        let mut duplicate = first.clone();
        duplicate.subject.history = UsageHistory::Known {
            id: "another-history".to_owned(),
        };
        duplicate.subject.worker_id = cosmon_core::id::WorkerId::new("other-worker").unwrap();
        let records = [first, duplicate];
        let windows = distinct_plan_windows(&records);
        assert_eq!(
            windows.len(),
            1,
            "identical account reading should appear once"
        );
        assert_eq!(format_plan_window(windows[0]), "account 5h used 42%");
        assert_eq!(
            format_worker_plan(&[fixture()]),
            "worker plan use unavailable"
        );

        let mut changed = fixture();
        changed.subject.history = UsageHistory::Known {
            id: "third-history".to_owned(),
        };
        changed.plan.windows[0].utilization =
            cosmon_core::usage::Utilization::from_fraction(0.55).unwrap();
        let readings = [fixture(), changed];
        let labels = distinct_plan_windows(&readings)
            .into_iter()
            .map(format_plan_window)
            .collect::<Vec<_>>();
        assert_eq!(labels, ["account 5h used 42%", "account 5h used 55%"]);
    }
}
