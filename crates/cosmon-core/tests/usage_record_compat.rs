// SPDX-License-Identifier: AGPL-3.0-only

use cosmon_core::event_v2::{Envelope, EventV2};
use cosmon_core::id::WorkerId;
use cosmon_core::usage::{
    decode_usage_record, ApiEquivalent, LegacyEnergyBudget, NonNegativeFinite, ObservationScope,
    PlanApplicability, PricingBasis, PricingCoverage, TokenCount, UnavailableReason, UsageRecord,
};

const NEW_EVENT: &str = include_str!("fixtures/usage/usage_observed_v1.json");
const LEGACY_TICK: &str = include_str!("fixtures/usage/energy_tick_legacy_zero.json");
const LEGACY_SUBSCRIPTION: &str =
    include_str!("fixtures/usage/energy_budget_legacy_subscription.json");

fn worker() -> WorkerId {
    WorkerId::new("quartz").unwrap()
}

#[test]
fn full_usage_event_roundtrips_both_independent_components() {
    let envelope = Envelope::from_line(NEW_EVENT.trim()).unwrap();
    let EventV2::UsageObserved { usage } = &envelope.event else {
        panic!("fixture must decode as usage_observed");
    };

    assert!(matches!(
        usage.api_equivalent,
        ApiEquivalent::Estimated {
            coverage: PricingCoverage::Partial { .. },
            ..
        }
    ));
    assert_eq!(usage.plan.windows.len(), 1);
    assert_eq!(usage.plan.windows[0].scope, ObservationScope::Account);
    assert!(matches!(
        usage.plan.worker_attributed,
        cosmon_core::usage::Availability::Unavailable {
            reason: UnavailableReason::MissingAttributionEvidence
        }
    ));

    let encoded = serde_json::to_string(&envelope).unwrap();
    let decoded = Envelope::from_line(&encoded).unwrap();
    assert_eq!(decoded, envelope);
}

#[test]
fn legacy_zero_tick_is_not_promoted_to_measured_zero_dollars() {
    let envelope = Envelope::from_line(LEGACY_TICK.trim()).unwrap();
    let usage = envelope.usage_record().unwrap();

    assert_eq!(
        usage.tokens.input_tokens,
        TokenCount::Measured { tokens: 900 }
    );
    assert_eq!(
        usage.tokens.output_tokens,
        TokenCount::Measured { tokens: 100 }
    );
    assert!(matches!(
        usage.api_equivalent,
        ApiEquivalent::Unavailable {
            reason: UnavailableReason::LegacyZeroAmbiguous
        }
    ));
}

#[test]
fn legacy_subscription_keeps_price_and_attribution_unknown() {
    let legacy: LegacyEnergyBudget = serde_json::from_str(LEGACY_SUBSCRIPTION.trim()).unwrap();
    let usage = UsageRecord::from_legacy_budget(worker(), legacy).unwrap();

    assert!(matches!(
        usage.api_equivalent,
        ApiEquivalent::Unavailable {
            reason: UnavailableReason::NotObserved
        }
    ));
    assert_eq!(usage.plan.windows.len(), 1);
    assert_eq!(usage.plan.windows[0].scope, ObservationScope::Unknown);
    assert!(matches!(
        usage.plan.applicability,
        PlanApplicability::Applicable { .. }
    ));
}

#[test]
fn legacy_reference_estimate_cannot_acquire_a_rate_card() {
    let json = r#"{
        "input_tokens": 10,
        "output_tokens": 5,
        "cost": {"kind":"reference_usd", "usd":0.25},
        "context_window": null
    }"#;
    let usage = decode_usage_record(json, worker()).unwrap();
    let ApiEquivalent::Estimated {
        coverage, basis, ..
    } = usage.api_equivalent
    else {
        panic!("legacy reference amount must remain an estimate");
    };
    assert_eq!(coverage, PricingCoverage::LegacyUnknown);
    assert_eq!(basis, PricingBasis::LegacyUnknown);
}

#[test]
fn current_decoder_rejects_negative_or_inconsistent_numeric_values() {
    let negative_usd = NEW_EVENT.replace("\"amount_usd\":1.25", "\"amount_usd\":-1.0");
    assert!(Envelope::from_line(&negative_usd).is_err());

    let inconsistent = NEW_EVENT.replace("\"value\":42.0", "\"value\":41.0");
    assert!(Envelope::from_line(&inconsistent).is_err());

    let invalid_subset = NEW_EVENT.replace("\"tokens\":200", "\"tokens\":1201");
    assert!(Envelope::from_line(&invalid_subset).is_err());

    assert!(NonNegativeFinite::new(f64::NAN, "fixture").is_err());
    assert!(NonNegativeFinite::new(f64::INFINITY, "fixture").is_err());
}

#[test]
fn plan_overage_is_preserved_instead_of_clamped() {
    let overage = NEW_EVENT
        .replace("\"utilization\":0.42", "\"utilization\":1.25")
        .replace("\"value\":42.0", "\"value\":125.0");
    let envelope = Envelope::from_line(&overage).unwrap();
    let usage = envelope.usage_record().unwrap();
    assert_eq!(usage.plan.windows[0].utilization.get(), 1.25);
}

#[test]
fn future_usage_schema_fails_closed() {
    let future = NEW_EVENT.replace("\"schema_version\":1", "\"schema_version\":2");
    assert!(Envelope::from_line(&future).is_err());
}
