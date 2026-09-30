// SPDX-License-Identifier: AGPL-3.0-only

use cosmon_core::price_manifest::{bundled_price_manifest, value_model_segments};
use cosmon_core::usage::{
    ApiEquivalent, ModelUsageSegment, ObservationProvenance, ObservationScope, PricingCoverage,
    TokenCount, UnavailableReason,
};

fn measured(tokens: u64) -> TokenCount {
    TokenCount::Measured { tokens }
}

fn segment(
    model: &str,
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
) -> ModelUsageSegment {
    ModelUsageSegment {
        model: model.to_owned(),
        input_tokens: measured(input),
        cached_input_tokens: measured(cached),
        cache_write_tokens: measured(cache_write),
        cache_write_5m_tokens: None,
        cache_write_1h_tokens: None,
        output_tokens: measured(output),
        reasoning_output_tokens: measured(reasoning),
    }
}

fn provenance() -> ObservationProvenance {
    ObservationProvenance {
        source: "independent_test_fixture".to_owned(),
        provider: Some("openai".to_owned()),
        observed_at: None,
        captured_at: None,
        scope: ObservationScope::UsageHistory,
    }
}

fn estimated(value: ApiEquivalent) -> (f64, PricingCoverage, String) {
    match value {
        ApiEquivalent::Estimated {
            amount_usd,
            coverage,
            basis,
            ..
        } => {
            let revision = match basis {
                cosmon_core::usage::PricingBasis::RateCard { revision, .. } => revision,
                cosmon_core::usage::PricingBasis::LegacyUnknown => {
                    panic!("new valuation must name its rate card")
                }
                _ => panic!("unexpected future pricing basis"),
            };
            (amount_usd.get(), coverage, revision)
        }
        ApiEquivalent::Unavailable { reason } => panic!("unexpected unavailable: {reason:?}"),
        _ => panic!("unexpected future API-equivalent variant"),
    }
}

#[test]
fn current_manifest_matches_independent_gpt_5_6_sol_standard_arithmetic() {
    let manifest = bundled_price_manifest().expect("bundled manifest must validate");
    let card = manifest.current_card().expect("current revision exists");
    let usage = segment("gpt-5.6-sol", 1_000_000, 800_000, 0, 100_000, 60_000);

    let (amount, coverage, revision) =
        estimated(value_model_segments(card, &[usage], true, provenance()));

    // Independent oracle from the official Standard short-context rates:
    // 200k fresh * $4 + 800k cached * $0.40 + 100k output * $20.
    let expected = 0.80 + 0.32 + 2.00;
    assert!((amount - expected).abs() < 1e-12);
    assert_eq!(
        coverage,
        PricingCoverage::Partial {
            missing: vec!["gpt-5.6-sol:context_length_unknown".to_owned()]
        }
    );
    assert_eq!(revision, "standard-2026-09-30-sonnet-5-5");
}

#[test]
fn current_sonnet_5_5_rates_are_independently_valued_and_preserve_history() {
    let manifest = bundled_price_manifest().expect("bundled manifest must validate");
    let current = manifest.current_card().expect("current revision exists");
    let rate = current
        .rate_for("claude-sonnet-5-5")
        .expect("current model has an exact rate");
    assert_eq!(
        rate.source_url,
        "https://platform.claude.com/docs/en/models/sonnet-5-5/overview"
    );
    assert_eq!(current.verified_at().to_string(), "2026-09-30");
    assert!(current.rate_for("claude-sonnet-5").is_some());
    assert!(manifest
        .card("standard-2026-09-28-codex")
        .expect("earlier card remains available")
        .rate_for("claude-sonnet-5-5")
        .is_none());

    let mut usage = segment(
        "claude-sonnet-5-5",
        3_000_000,
        1_000_000,
        1_000_000,
        100_000,
        0,
    );
    usage.cache_write_5m_tokens = Some(400_000);
    usage.cache_write_1h_tokens = Some(600_000);
    let (amount, coverage, revision) =
        estimated(value_model_segments(current, &[usage], true, provenance()));
    // Published rates: fresh $2, read $0.20, 5m write $2.50,
    // 1h write $4, output $10 per million tokens.
    assert!((amount - 6.6).abs() < 1e-12);
    assert_eq!(coverage, PricingCoverage::Complete);
    assert_eq!(revision, "standard-2026-09-30-sonnet-5-5");
}

#[test]
fn a_limited_context_rate_cannot_claim_complete_without_request_length() {
    let manifest = bundled_price_manifest().unwrap();
    let card = manifest.current_card().unwrap();
    let usage = segment("gpt-5.5", 5_000_000, 0, 0, 0, 0);
    let (_, coverage, _) = estimated(value_model_segments(card, &[usage], true, provenance()));
    assert_eq!(
        coverage,
        PricingCoverage::Partial {
            missing: vec!["gpt-5.5:context_length_unknown".to_owned()]
        }
    );
}

#[test]
fn cache_write_duration_split_uses_each_rate_without_double_charging_input() {
    let manifest = bundled_price_manifest().unwrap();
    let card = manifest.current_card().unwrap();
    let mut usage = segment("claude-opus-5-5", 1_000_000, 0, 1_000_000, 0, 0);
    usage.cache_write_5m_tokens = Some(400_000);
    usage.cache_write_1h_tokens = Some(600_000);
    let (amount, coverage, _) = estimated(value_model_segments(card, &[usage], true, provenance()));
    assert!((amount - 6.8).abs() < 1e-12);
    assert_eq!(coverage, PricingCoverage::Complete);
}

#[test]
fn reasoning_is_an_output_subset_not_a_second_charge() {
    let manifest = bundled_price_manifest().unwrap();
    let card = manifest.current_card().unwrap();
    let without_reasoning = segment("gpt-5.3-codex", 1_000, 800, 0, 100, 0);
    let with_reasoning = segment("gpt-5.3-codex", 1_000, 800, 0, 100, 60);

    let (base, _, _) = estimated(value_model_segments(
        card,
        &[without_reasoning],
        true,
        provenance(),
    ));
    let (reasoning, _, _) = estimated(value_model_segments(
        card,
        &[with_reasoning],
        true,
        provenance(),
    ));
    assert!((base - reasoning).abs() < 1e-12);
}

#[test]
fn mixed_known_and_unknown_models_keep_the_known_subtotal() {
    let manifest = bundled_price_manifest().unwrap();
    let card = manifest.current_card().unwrap();
    let known = segment("gpt-5.3-codex", 1_000_000, 0, 0, 0, 0);
    let unknown = segment("gpt-7-hypothetical", 1_000_000, 0, 0, 0, 0);

    let (amount, coverage, _) = estimated(value_model_segments(
        card,
        &[known, unknown],
        true,
        provenance(),
    ));
    assert!((amount - 1.75).abs() < 1e-12);
    assert_eq!(
        coverage,
        PricingCoverage::Partial {
            missing: vec!["model:gpt-7-hypothetical".to_owned()]
        }
    );
}

#[test]
fn wholly_unknown_usage_is_unavailable_not_free() {
    let manifest = bundled_price_manifest().unwrap();
    let card = manifest.current_card().unwrap();
    let unknown = segment("gpt-7-hypothetical", 1_000_000, 0, 0, 0, 0);

    assert_eq!(
        value_model_segments(card, &[unknown], true, provenance()),
        ApiEquivalent::Unavailable {
            reason: UnavailableReason::MissingPricingCategory
        }
    );
}

#[test]
fn claude_cache_write_without_duration_is_an_explicit_partial() {
    let manifest = bundled_price_manifest().unwrap();
    let card = manifest.current_card().unwrap();
    let usage = segment("claude-opus-4-6", 2_000_000, 500_000, 500_000, 100_000, 0);

    let (amount, coverage, _) = estimated(value_model_segments(card, &[usage], true, provenance()));
    // 1M fresh * $5 + 500k cache read * $0.50 + 100k output * $25.
    assert!((amount - 7.75).abs() < 1e-12);
    assert_eq!(
        coverage,
        PricingCoverage::Partial {
            missing: vec!["claude-opus-4-6:cache_write_duration".to_owned()]
        }
    );
}

#[test]
fn historical_revision_is_selectable_and_does_not_gain_new_models() {
    let manifest = bundled_price_manifest().unwrap();
    let historical = manifest
        .card("standard-2026-07-19")
        .expect("historical card remains embedded");
    assert!(historical.rate_for("gpt-5-codex").is_some());
    assert!(historical.rate_for("gpt-5.3-codex").is_none());
    assert!(manifest
        .current_card()
        .unwrap()
        .rate_for("gpt-5.3-codex")
        .is_some());
    assert!(manifest
        .current_card()
        .unwrap()
        .rate_for("gpt-5.6-sol")
        .is_some());
}

#[test]
fn exact_model_matching_never_guesses_an_alias() {
    let manifest = bundled_price_manifest().unwrap();
    let current = manifest.current_card().unwrap();
    assert!(current.rate_for("claude-opus-4-6").is_some());
    assert!(current.rate_for("opus-4-6").is_none());
    assert!(current.rate_for("CLAUDE-OPUS-4-6").is_none());
    assert!(current.rate_for("gpt-5.6-sol").is_some());
    assert!(current.rate_for("gpt-5.6-sol-preview").is_none());
}
