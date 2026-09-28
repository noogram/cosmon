// SPDX-License-Identifier: AGPL-3.0-only

use chrono::{DateTime, Utc};
use cosmon_core::plan_observation::{claude_plan, codex_plan, PlanSource};
use cosmon_core::usage::{
    Availability, Freshness, ObservationScope, PlanApplicability, UnavailableReason,
    UtilizationUnit,
};

fn time(s: &str) -> DateTime<Utc> {
    s.parse().expect("fixture time")
}

#[test]
fn source_units_reset_and_overage_are_independent() {
    let now = time("2026-09-28T12:00:00Z");
    let status = claude_plan(
        r#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1790604000},"seven_day":{"used_percentage":11}},"context_window":{"used_percentage":99},"private":"discard"}"#,
        PlanSource::ClaudeStatusLine,
        now,
    );
    let stream = claude_plan(
        r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.42,"resetsAt":1790604000},"seven_day":{"utilization":1.25}}}}"#,
        PlanSource::ClaudeStream,
        now,
    );
    assert_eq!(status.plan.windows.len(), 2);
    assert_eq!(stream.plan.windows.len(), 2);
    assert_eq!(status.plan.windows[0].utilization.get(), 0.42);
    assert_eq!(stream.plan.windows[0].utilization.get(), 0.42);
    assert_eq!(status.plan.windows[1].utilization.get(), 0.11);
    assert_eq!(stream.plan.windows[1].utilization.get(), 1.25);
    assert_eq!(
        status.plan.windows[0]
            .window
            .resets_at
            .map(|t| t.timestamp()),
        Some(1790604000)
    );
    assert_eq!(status.plan.windows[0].window.duration_minutes, Some(300));
    assert_eq!(status.plan.windows[1].window.duration_minutes, Some(10080));
    assert_eq!(
        status.plan.windows[0].source_value.unit,
        UtilizationUnit::Percent
    );
    assert_eq!(
        stream.plan.windows[0].source_value.unit,
        UtilizationUnit::Fraction
    );
    assert_eq!(status.plan.windows[0].provenance.observed_at, None);
    assert_eq!(status.plan.windows[0].freshness, Freshness::Unknown);
    assert!(!serde_json::to_string(&status)
        .expect("encode")
        .contains("discard"));
}

#[test]
fn absent_invalid_and_other_meters_do_not_claim_a_plan() {
    let now = time("2026-09-28T12:00:00Z");
    for raw in [
        "{}",
        r#"{"rate_limits":{"spend_limit":{"used_percentage":77}},"context_window":{"used_percentage":88}}"#,
    ] {
        let sample = claude_plan(raw, PlanSource::ClaudeStatusLine, now);
        assert!(sample.plan.windows.is_empty());
        assert!(matches!(
            sample.plan.applicability,
            PlanApplicability::Unknown {
                reason: UnavailableReason::NotObserved
            }
        ));
        assert_eq!(sample.unavailable_windows.len(), 2);
    }
    for raw in [
        r#"{"rate_limits":{"five_hour":{"used_percentage":-1}}}"#,
        r#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":"bad"}}}"#,
        "garbage",
    ] {
        let sample = claude_plan(raw, PlanSource::ClaudeStatusLine, now);
        assert!(sample.plan.windows.is_empty());
        assert_eq!(
            sample.unavailable_windows[0].reason,
            UnavailableReason::MalformedSource
        );
    }
}

#[test]
fn codex_windows_keep_source_time_and_token_only_tails_do_not_refresh() {
    let raw = concat!(
        "{\"timestamp\":\"2026-09-28T11:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":42,\"window_minutes\":300,\"resets_at\":1790604000},\"secondary\":{\"used_percent\":11,\"window_minutes\":10080}}}}\n",
        "{\"timestamp\":\"2026-09-28T12:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{}},\"rate_limits\":null}}\n"
    );
    let now = time("2026-09-28T12:00:00Z");
    let mut sample = codex_plan(raw, now);
    assert_eq!(sample.plan.windows.len(), 2);
    assert_eq!(sample.plan.windows[1].utilization.get(), 0.11);
    assert_eq!(
        sample.plan.windows[0].provenance.observed_at,
        Some(time("2026-09-28T11:00:00Z"))
    );
    sample.refresh(now, chrono::Duration::minutes(10));
    assert_eq!(sample.plan.windows[0].freshness, Freshness::Stale);
    let later = codex_plan(raw, time("2026-09-29T12:00:00Z"));
    assert_eq!(
        later.plan.windows[0].provenance.observed_at,
        sample.plan.windows[0].provenance.observed_at
    );
    assert!(matches!(
        sample.plan.worker_attributed,
        Availability::Unavailable {
            reason: UnavailableReason::MissingAttributionEvidence
        }
    ));
    assert!(sample
        .plan
        .windows
        .iter()
        .all(|w| w.scope == ObservationScope::Account));
}

#[test]
fn expired_and_missing_windows_do_not_survive_as_current() {
    let now = time("2026-09-28T12:00:00Z");
    let raw = "{\"timestamp\":\"2026-09-28T11:59:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":2,\"resets_at\":1}}}}";
    let mut sample = codex_plan(raw, now);
    sample.refresh(now, chrono::Duration::minutes(10));
    assert_eq!(sample.plan.windows[0].freshness, Freshness::Reset);
    assert_eq!(sample.unavailable_windows.len(), 1);
    let mut unknown = claude_plan(
        r#"{"rate_limits":{"five_hour":{"used_percentage":0}}}"#,
        PlanSource::ClaudeStatusLine,
        now,
    );
    unknown.refresh(
        now + chrono::Duration::hours(1),
        chrono::Duration::minutes(10),
    );
    assert_eq!(unknown.plan.windows[0].freshness, Freshness::Stale);
}

#[test]
fn new_reset_replaces_old_windows_without_subtracting_account_use() {
    let now = time("2026-09-28T12:00:00Z");
    let raw = concat!(
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":99,\"resets_at\":1},\"secondary\":{\"used_percent\":50}}}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"primary\":{\"used_percent\":2,\"resets_at\":1790604000}}}}\n"
    );
    let sample = codex_plan(raw, now);
    assert_eq!(sample.plan.windows.len(), 1);
    assert_eq!(sample.plan.windows[0].utilization.get(), 0.02);
    assert_eq!(sample.unavailable_windows[0].meter, "secondary");
    assert_eq!(sample.plan.windows[0].provenance.observed_at, None);
}

#[test]
fn storage_port_can_be_replaced_without_io() {
    use cosmon_core::{
        id::WorkerId,
        plan_observation::{PlanObservation, PlanObservationStore},
    };
    #[derive(Default)]
    struct MemoryStore(std::cell::RefCell<Vec<(WorkerId, PlanObservation)>>);
    impl PlanObservationStore for MemoryStore {
        type Error = std::convert::Infallible;
        fn save(&self, worker: &WorkerId, sample: &PlanObservation) -> Result<(), Self::Error> {
            self.0.borrow_mut().push((worker.clone(), sample.clone()));
            Ok(())
        }
        fn load(
            &self,
            worker: &WorkerId,
            source: PlanSource,
        ) -> Result<Option<PlanObservation>, Self::Error> {
            Ok(self
                .0
                .borrow()
                .iter()
                .filter(|(id, s)| id == worker && s.source == source)
                .max_by_key(|(_, s)| s.captured_at)
                .map(|(_, s)| s.clone()))
        }
    }
    let store = MemoryStore::default();
    let worker = WorkerId::new("example").expect("worker");
    let sample = claude_plan(
        "{}",
        PlanSource::ClaudeStatusLine,
        time("2026-09-28T12:00:00Z"),
    );
    store.save(&worker, &sample).expect("save");
    assert_eq!(
        store
            .load(&worker, PlanSource::ClaudeStatusLine)
            .expect("load"),
        Some(sample)
    );
}

#[test]
fn codex_distinct_quota_buckets_are_not_overwritten_or_exposed_verbatim() {
    let raw = concat!(
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"limit_id\":\"bucket-alpha\",\"primary\":{\"used_percent\":31}}}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"limit_id\":\"bucket-beta\",\"primary\":{\"used_percent\":63}}}}\n"
    );
    let sample = codex_plan(raw, time("2026-09-28T12:00:00Z"));
    assert_eq!(sample.plan.windows.len(), 2);
    assert_ne!(sample.plan.windows[0].meter, sample.plan.windows[1].meter);
    let mut values: Vec<_> = sample
        .plan
        .windows
        .iter()
        .map(|w| w.utilization.get())
        .collect();
    values.sort_by(f64::total_cmp);
    assert_eq!(values, [0.31, 0.63]);
    let encoded = serde_json::to_string(&sample).expect("encode");
    assert!(!encoded.contains("bucket-alpha"));
    assert!(!encoded.contains("bucket-beta"));
}

#[test]
fn claude_statusline_rejects_out_of_documented_range_but_stream_allows_overage() {
    let now = time("2026-09-28T12:00:00Z");
    let status = claude_plan(
        r#"{"rate_limits":{"five_hour":{"used_percentage":125}}}"#,
        PlanSource::ClaudeStatusLine,
        now,
    );
    assert!(status.plan.windows.is_empty());
    assert_eq!(
        status.unavailable_windows[0].reason,
        UnavailableReason::MalformedSource
    );
    let stream = claude_plan(
        r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":1.25}}}}"#,
        PlanSource::ClaudeStream,
        now,
    );
    assert_eq!(stream.plan.windows[0].utilization.get(), 1.25);
}
