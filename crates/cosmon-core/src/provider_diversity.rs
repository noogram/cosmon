// SPDX-License-Identifier: AGPL-3.0-only

//! Provider-family diversity — the tier-(a) resolved-endpoint floor for
//! cross-provider reading committees (ADR-147, C3).
//!
//! # Why this module exists
//!
//! ADR-147 promotes **provider-family error-independence** to a constitutional
//! invariant: a reading committee convened on root/security-stake work must
//! include ≥1 reader whose *resolved* endpoint differs from the generator's
//! family, because a Claude auditing a Claude is channel-independent yet
//! **error-correlated** (it shares weights, so it shares blind spots — an echo,
//! not a witness). The invariant is enforced in two tiers; this module is
//! **tier (a)**, the cheap decidable floor.
//!
//! Tier (a) is a pure, config-level check: it resolves each committee seat (an
//! adapter name) to its endpoint identity tuple `(provider, base_url,
//! model-family)` and asserts the seats resolve to **distinct** tuples, at
//! least [`min_distinct_provider_endpoints`] of them. Two seats that collapse
//! to the same tuple redden `cs reconcile --check`.
//!
//! [`min_distinct_provider_endpoints`]:
//!   crate::config::ProviderRequirementSet::min_distinct_provider_endpoints
//!
//! # What tier (a) does and does not buy (the §8b ceiling)
//!
//! Tier (a) makes the **trivial collapse** — two seats pointing at the same URL,
//! or the same model behind two labels — *visible and attributable*. It does
//! **not** verify that the `model-family` string the config implies matches the
//! weights actually answering at `base_url`: the family label here is
//! **derived from operator config** (`base_url` host + `model` prefix), not an
//! attested fact. A motivated proxy-costume (an operator who points a seat at a
//! Claude-compatible endpoint and lies about it) survives tier (a). Binding
//! family to an attested token is tier (b) — `SameFamilyRefusal`, an ADR-grade
//! follow-on (ADR-147 §Tier (b)). Everything here inherits the
//! `docs/architectural-invariants.md` §8b trace-visibility ceiling: the lint is
//! a CI dry-run, bypassable by `--no-verify`. It makes a mono-family committee
//! **loud, not impossible.**
//!
//! # The `adapter` component is the *resolved provider*, never the seat name
//!
//! ADR-147 is emphatic: *distinctness is measured on the resolved endpoint,
//! never on the declared adapter name.* If the tuple carried the config-section
//! name, two differently-named seats fronting the same endpoint would always
//! read as "distinct" — exactly the proxy-costume the invariant forbids. So the
//! tuple's first component ([`EndpointTuple::provider`]) is resolved from
//! `base_url` / `model`, not copied from the `[adapters.<name>]` key.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{AdaptersConfig, ProviderBiasConfig};

/// The resolved endpoint identity of one committee seat — the tuple tier (a)
/// measures distinctness on.
///
/// Every component is **resolved**, not declared: `provider` and `family` come
/// from `base_url` + `model`, never from the `[adapters.<name>]` section name
/// (ADR-147). Two seats are the same endpoint iff their whole tuple is equal;
/// that is the trivial-collapse the tier-(a) floor detects.
///
/// `Ord` + `Hash` are derived so the tuples can be collected into a
/// `BTreeSet`/`BTreeMap` for deterministic distinct-counting and stable
/// diagnostic ordering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EndpointTuple {
    /// The resolved **provider kind** (`"openai"`, `"anthropic"`, `"xai"`,
    /// `"local"`, `"unknown"`, …) — derived from the `base_url` host, falling
    /// back to the model / adapter-name lineage. **Not** the config-section
    /// name: the whole point of ADR-147 is that a seat *named* `openai` may
    /// resolve to any provider, so the name cannot be the distinctness key.
    pub provider: String,

    /// The normalized base URL the seat POSTs against (lowercased, trailing
    /// slash trimmed). Empty string means "the provider's vendor default" — two
    /// vendor-default seats of the same provider therefore share this
    /// component and collapse to one tuple, which is the correct
    /// error-independence verdict.
    pub base_url: String,

    /// The resolved **model-family** label (`"anthropic"`, `"openai"`,
    /// `"qwen"`, `"llama"`, …) — the load-bearing error-independence axis. It is
    /// *derived config, not a verified fact* until tier (b) lands (ADR-147).
    pub family: String,
}

/// Resolve one committee seat (an adapter name) to its [`EndpointTuple`] using
/// the project's `[adapters]` inventory.
///
/// A seat with no matching `[adapters.<name>]` entry resolves from the bare
/// name alone (the built-in `claude` / `openai` / `anthropic` names carry a
/// known family). The resolution never fails — an unknown seat resolves to a
/// `"unknown"` provider so it still participates in the distinctness count
/// rather than being silently dropped.
#[must_use]
pub fn resolve_endpoint_tuple(adapters: Option<&AdaptersConfig>, seat: &str) -> EndpointTuple {
    let entry = adapters.and_then(|a| a.entry(seat));
    let base_url = entry.and_then(|e| e.base_url.clone());
    let model = entry.and_then(|e| e.default_model.clone());
    EndpointTuple {
        provider: provider_kind(base_url.as_deref(), model.as_deref(), seat),
        base_url: normalize_base_url(base_url.as_deref()),
        family: provider_family(base_url.as_deref(), model.as_deref(), seat),
    }
}

/// Resolve the **model-family** label from `base_url` host + `model` prefix,
/// falling back to the adapter name.
///
/// The `base_url` host is the strongest signal because it is where a
/// proxy-costume reveals itself: an `[adapters.openai]` seat with
/// `base_url = "https://api.anthropic.com"` resolves to family `"anthropic"`,
/// not `"openai"`. On a local endpoint the *vendor* is meaningless, so the
/// family is taken from the model lineage (`qwen`, `llama`, …). Unknown ids
/// resolve to the trimmed, lowercased id itself, so distinct unknown models
/// stay distinct and identical ones collapse — an honest, conservative default.
///
/// The label is **derived, not attested** (see the module header): tier (a)
/// trusts the operator-supplied `base_url`/`model`; tier (b) does not.
#[must_use]
pub fn provider_family(base_url: Option<&str>, model: Option<&str>, adapter_name: &str) -> String {
    if let Some(url) = base_url {
        let host = url.to_ascii_lowercase();
        if host.contains("anthropic") {
            return "anthropic".to_string();
        }
        if host.contains("openai.com") {
            return "openai".to_string();
        }
        if host.contains("x.ai") {
            return "xai".to_string();
        }
        if host.contains("moonshot") {
            return "moonshot".to_string();
        }
        if host.contains("deepseek") {
            return "deepseek".to_string();
        }
        if host.contains("googleapis") || host.contains("generativelanguage") {
            return "google".to_string();
        }
        if is_local_host(&host) {
            // Local endpoint: the vendor is the operator, so family is the
            // model's lineage, not "who hosts it".
            if let Some(fam) = family_from_model(model) {
                return fam;
            }
            return "local".to_string();
        }
        // Unknown host: fall through to model / name lineage.
    }
    if let Some(fam) = family_from_model(model) {
        return fam;
    }
    family_from_name(adapter_name)
}

/// Resolve the **provider kind** (the tuple's first component) from `base_url`
/// host, falling back to the adapter-name lineage.
///
/// Coarser than [`provider_family`]: it names *who answers the HTTP request*
/// (`openai` / `anthropic` / `local` / a raw host), whereas the family names
/// *which weights*. They diverge only for local / self-hosted endpoints, where
/// the provider is `"local"` but the family is the model lineage.
///
/// Crucially it derives from `base_url` → `model` → adapter name **in that
/// order** — the seat name is only the last resort. If provider were read off
/// the config-section name, two vendor-default seats named `gpt-fast` and
/// `gpt-slow` would read as *distinct* providers even though both are `openai`,
/// exactly the name-as-distinctness-axis the invariant forbids.
#[must_use]
fn provider_kind(base_url: Option<&str>, model: Option<&str>, adapter_name: &str) -> String {
    if let Some(url) = base_url {
        let host = url.to_ascii_lowercase();
        if host.contains("anthropic") {
            return "anthropic".to_string();
        }
        if host.contains("openai.com") {
            return "openai".to_string();
        }
        if host.contains("x.ai") {
            return "xai".to_string();
        }
        if host.contains("moonshot") {
            return "moonshot".to_string();
        }
        if host.contains("deepseek") {
            return "deepseek".to_string();
        }
        if host.contains("googleapis") || host.contains("generativelanguage") {
            return "google".to_string();
        }
        if is_local_host(&host) {
            return "local".to_string();
        }
        // A non-empty but unrecognised host is its own provider kind: keep the
        // raw host so two seats on the same private proxy still collide.
        return host;
    }
    // No base_url — a vendor-default seat. Derive the provider from the model
    // lineage, NEVER the seat name (only the name as final fallback).
    if let Some(fam) = family_from_model(model) {
        return fam;
    }
    family_from_name(adapter_name)
}

/// `true` for a `base_url` host that is loopback / on-box.
fn is_local_host(host: &str) -> bool {
    host.contains("localhost")
        || host.contains("127.0.0.1")
        || host.contains("0.0.0.0")
        || host.contains("[::1]")
}

/// Map a model id to its canonical family label by prefix, or `None` when the
/// id is empty. Unknown ids resolve (via the caller) to the id itself.
fn family_from_model(model: Option<&str>) -> Option<String> {
    let m = model?.trim().to_ascii_lowercase();
    if m.is_empty() {
        return None;
    }
    let fam = if m.starts_with("claude") {
        "anthropic"
    } else if m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        "openai"
    } else if m.starts_with("grok") {
        "xai"
    } else if m.starts_with("moonshot") || m.starts_with("kimi") {
        "moonshot"
    } else if m.starts_with("deepseek") {
        "deepseek"
    } else if m.starts_with("gemini") {
        "google"
    } else if m.starts_with("qwen") {
        "qwen"
    } else if m.starts_with("llama") {
        "llama"
    } else if m.starts_with("mistral") || m.starts_with("mixtral") {
        "mistral"
    } else {
        // Unknown lineage: the id *is* the family label, so distinct unknown
        // models stay distinct and identical ones collapse.
        return Some(m);
    };
    Some(fam.to_string())
}

/// Map a bare adapter name to a family label — the last-resort resolution when
/// neither `base_url` nor `model` is declared.
fn family_from_name(adapter_name: &str) -> String {
    let n = adapter_name.trim().to_ascii_lowercase();
    match n.as_str() {
        "claude" | "anthropic" => "anthropic".to_string(),
        "openai" | "codex" => "openai".to_string(),
        "xai" | "grok" => "xai".to_string(),
        "moonshot" | "kimi" => "moonshot".to_string(),
        "deepseek" => "deepseek".to_string(),
        "gemini" | "google" => "google".to_string(),
        "" => "unknown".to_string(),
        other => other.to_string(),
    }
}

/// Normalize a `base_url` for use as a tuple component: lowercased with any
/// trailing `/` trimmed. `None` → empty string (the vendor default).
fn normalize_base_url(base_url: Option<&str>) -> String {
    base_url
        .map(|u| u.trim().trim_end_matches('/').to_ascii_lowercase())
        .unwrap_or_default()
}

/// Compute the tier-(a) requirement-downgrade violations for a committee
/// baseline against the project `[adapters]` inventory.
///
/// This is the pure kernel of the `cs reconcile --check`
/// `check_no_profile_requirement_downgrade` lint. It returns one
/// human-readable message per violation, empty when the effective committee is
/// diverse enough (or when no committee is declared — the opt-in default).
///
/// It compares **requirement-ids + resolved endpoint tuples, never config
/// section names** (ADR-147). Two classes of violation are reported:
///
/// 1. **Endpoint collision** — two distinct seats resolve to the *same*
///    `(provider, base_url, family)` tuple. The committee names N readers but
///    delivers fewer than N independent endpoints; the surplus seats are an
///    echo. Reported whenever it happens, because a declared reader that
///    collapses onto another is a silent diversity *downgrade* achieved through
///    the `[adapters]` layer (the proxy-costume base-url override), not through
///    editing the — add-only — committee baseline.
/// 2. **Floor shortfall** — the effective
///    [`min_distinct_provider_endpoints`] floor exceeds the number of distinct
///    resolved tuples the committee actually delivers.
///
/// [`min_distinct_provider_endpoints`]:
///   crate::config::ProviderRequirementSet::min_distinct_provider_endpoints
///
/// The effective requirement-set is `baseline ∪ ⋃ profiles`
/// ([`ProviderBiasConfig::effective`]) — the monotone union that makes a
/// *downgrade* inexpressible in the type. This function checks that the union's
/// *resolved* consequence still meets its own floor; the type guarantees the
/// declared numbers never drop, and this guarantees the config the numbers
/// resolve against does not quietly undo them.
#[must_use]
pub fn requirement_downgrade_violations(
    bias: &ProviderBiasConfig,
    adapters: Option<&AdaptersConfig>,
) -> Vec<String> {
    let effective = bias.effective();

    // The committee seats are the union of the effective readers and
    // falsifiers — a name that is both is one seat.
    let mut seats: Vec<String> = effective
        .additional_readers
        .iter()
        .chain(effective.additional_falsifiers.iter())
        .cloned()
        .collect();
    seats.sort();
    seats.dedup();

    if seats.is_empty() && effective.min_distinct_provider_endpoints.is_none() {
        // Nothing declared — byte-identical to a galaxy that never opted in.
        return Vec::new();
    }

    // Resolve each seat and group seats by resolved tuple (BTreeMap → stable
    // ordering in diagnostics).
    let mut by_tuple: BTreeMap<EndpointTuple, Vec<String>> = BTreeMap::new();
    for seat in &seats {
        let tuple = resolve_endpoint_tuple(adapters, seat);
        by_tuple.entry(tuple).or_default().push(seat.clone());
    }

    let mut violations = Vec::new();

    // (1) Endpoint collisions — two distinct seats on one resolved tuple.
    for (tuple, members) in &by_tuple {
        if members.len() > 1 {
            violations.push(format!(
                "committee seats {members:?} resolve to the SAME endpoint \
                 (provider={:?}, base_url={:?}, family={:?}) — they are an echo, \
                 not independent readers (resolved-endpoint collapse; \
                 add-only baseline was not lowered, the [adapters] base_url \
                 override was)",
                tuple.provider, tuple.base_url, tuple.family,
            ));
        }
    }

    // (2) Floor shortfall — fewer distinct resolved endpoints than required.
    let distinct = by_tuple.len();
    if let Some(min) = effective.min_distinct_provider_endpoints {
        let min = min as usize;
        if distinct < min {
            let families: std::collections::BTreeSet<&str> =
                by_tuple.keys().map(|t| t.family.as_str()).collect();
            violations.push(format!(
                "committee resolves to {distinct} distinct provider endpoint(s) \
                 (families {families:?}), below the required floor of {min} \
                 (min_distinct_provider_endpoints); a mono-/under-family committee \
                 is error-correlated (ADR-147 tier a)"
            ));
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AdapterEntry, ProviderBiasConfig, ProviderRequirementSet};

    fn adapters_with(entries: &[(&str, Option<&str>, Option<&str>)]) -> AdaptersConfig {
        let mut cfg = AdaptersConfig::default();
        for (name, base_url, model) in entries {
            cfg.entries.insert(
                (*name).to_string(),
                AdapterEntry {
                    base_url: base_url.map(str::to_string),
                    default_model: model.map(str::to_string),
                    ..AdapterEntry::default()
                },
            );
        }
        cfg
    }

    #[test]
    fn family_resolves_from_base_url_host_over_name() {
        // An `openai`-named seat pointed at Anthropic resolves to anthropic —
        // the proxy-costume unmasked (ADR-147).
        assert_eq!(
            provider_family(Some("https://api.anthropic.com"), Some("gpt-4o"), "openai"),
            "anthropic"
        );
    }

    #[test]
    fn family_resolves_from_model_prefix_when_no_base_url() {
        assert_eq!(
            provider_family(None, Some("claude-opus-4-8"), "openai"),
            "anthropic"
        );
        assert_eq!(
            provider_family(None, Some("gpt-4o-mini"), "seatx"),
            "openai"
        );
        assert_eq!(provider_family(None, Some("grok-2"), "seatx"), "xai");
    }

    #[test]
    fn family_falls_back_to_name_then_unknown() {
        assert_eq!(provider_family(None, None, "claude"), "anthropic");
        assert_eq!(provider_family(None, None, "mystery"), "mystery");
    }

    #[test]
    fn local_endpoint_family_is_model_lineage_not_vendor() {
        assert_eq!(
            provider_family(Some("http://localhost:8000"), Some("qwen3-8b"), "openai"),
            "qwen"
        );
    }

    #[test]
    fn two_vendor_default_openai_seats_collide() {
        let adapters = adapters_with(&[
            ("gpt-fast", None, Some("gpt-4o-mini")),
            ("gpt-slow", None, Some("gpt-4o")),
        ]);
        let bias = ProviderBiasConfig {
            baseline: ProviderRequirementSet {
                additional_readers: vec!["gpt-fast".into(), "gpt-slow".into()],
                min_distinct_provider_endpoints: Some(2),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = requirement_downgrade_violations(&bias, Some(&adapters));
        // Both a collision AND a floor shortfall (1 distinct endpoint, floor 2).
        assert_eq!(
            v.len(),
            2,
            "expected collision + floor shortfall, got {v:?}"
        );
        assert!(v.iter().any(|m| m.contains("SAME endpoint")));
        assert!(v.iter().any(|m| m.contains("below the required floor")));
    }

    #[test]
    fn distinct_providers_pass_the_floor() {
        let adapters = adapters_with(&[
            ("claude", None, Some("claude-opus-4-8")),
            ("openai", None, Some("gpt-4o")),
        ]);
        let bias = ProviderBiasConfig {
            baseline: ProviderRequirementSet {
                additional_readers: vec!["claude".into()],
                additional_falsifiers: vec!["openai".into()],
                min_distinct_provider_endpoints: Some(2),
            },
            ..Default::default()
        };
        assert!(requirement_downgrade_violations(&bias, Some(&adapters)).is_empty());
    }

    #[test]
    fn absent_committee_is_no_op() {
        let bias = ProviderBiasConfig::default();
        assert!(requirement_downgrade_violations(&bias, None).is_empty());
    }

    #[test]
    fn floor_alone_reddens_when_committee_too_small() {
        // A single-seat committee cannot meet a floor of 2.
        let adapters = adapters_with(&[("claude", None, Some("claude-opus-4-8"))]);
        let bias = ProviderBiasConfig {
            baseline: ProviderRequirementSet {
                additional_readers: vec!["claude".into()],
                min_distinct_provider_endpoints: Some(2),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = requirement_downgrade_violations(&bias, Some(&adapters));
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("below the required floor"));
    }
}
