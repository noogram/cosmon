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
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
///
/// # A name that resolves to itself is not a diversity witness
///
/// The name fallback makes this function total, which is right for counting,
/// and useless as an audit **on the branch where it returns the name itself**:
/// there the resolved family IS the seat's own label, so a declaration can
/// never be contradicted by the derivation. A *roster* therefore may not rest
/// on that branch — [`crate::committee::RosterSpec::resolved`] refuses a seat
/// whose name [`endpoint_is_derived`] says nothing can contradict, before this
/// function is ever consulted.
///
/// Note the predicate is *`endpoint_is_derived`*, not *"has a TOML section"*.
/// The two are different properties, and only the first is the one that
/// matters: `codex` has no `[adapters.codex]` section in most projects (the
/// section is optional and only tunes launch mode) yet resolves to family
/// `openai` because the name belongs to a vendor lineage this module knows —
/// a fact about the binary, not a restatement of the label.
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

/// Whether `seat`'s [`EndpointTuple`] is **derived from something that can
/// contradict the seat**, rather than a restatement of the seat's own label.
///
/// # Why this exists, and why "has an `[adapters.<name>]` section" is not it
///
/// [`crate::committee::RosterSpec::resolved`] must refuse a seat whose family
/// nothing can disagree with. The tempting test is *does this adapter have a
/// TOML section?* — and it is the property **next to** the one that matters.
/// [`AdaptersConfig`] is populated only by `[adapters.*]` TOML; there is no
/// built-in injection into it. But `cs tackle` dispatches `codex`,
/// `claude`, `aider` and `opencode` with **no** section at all — for the CLI
/// adapters the section is optional and only tunes launch mode. Measuring the
/// section therefore refuses seats cosmon can really dispatch, and in a galaxy
/// whose only non-generator family is reachable through such an adapter it
/// refuses the sole provider that would have supplied the diversity the gate
/// exists to enforce.
///
/// The property that matters is *resolvability*: is there anything on the
/// record, other than the seat's own label, from which the tuple is derived?
/// There are exactly two such records:
///
/// 1. **The operator's inventory** — `[adapters.<name>]` declaring `base_url`
///    and/or `default_model`. A section declaring *neither* is not a record:
///    resolution falls straight through to the name, so an empty section buys
///    a seat nothing but the appearance of one.
///
///    *Neither* means neither **resolves**, not neither is **spelled**. Until
///    2026-07-28 this test read `base_url.is_some() || default_model.is_some()`
///    — the PRESENCE of a key — while resolution reads its CONTENT
///    (`family_from_model` trims and returns `None` on the empty string). So
///    `default_model = ""` satisfied admission, failed resolution, and fell
///    through to `family_from_name(seat)`: the seat's own label, on exactly the
///    axis this predicate exists to defend. Measured — an `[adapters.aider]`
///    section with no `default_model` was refused as a self-attestation, and the
///    same section with `default_model = ""` was admitted. Two quote marks, and
///    the operator did not even have to lie: only to type nothing where typing
///    nothing looks like configuration.
///
///    The remedy is structural rather than a second emptiness check bolted on
///    here: admission now CALLS `family_from_record` — the very resolver
///    whose answer it is predicting — so the two cannot disagree again. There
///    is one place that answers *is there a record?*, and it is the same place
///    that answers *what does the record resolve to?*
/// 2. **The vendor lineage of the name itself**, via `family_from_name`, and
///    then only when the answer is a named vendor (`is_named_vendor`) rather
///    than the identity fallback. `codex` → `openai` is a fact about which
///    binary cosmon spawns and which weights answer it; it is not the label
///    coming back around, and a seat declaring family `codex` on adapter
///    `codex` is *contradicted* by it. `aider` → `aider` is the identity arm:
///    aider is provider-agnostic, cosmon genuinely does not know what answers,
///    and such a seat must declare `base_url`/`default_model` to be rosterable.
///
/// This is deliberately silent about whether the name can be **dispatched** —
/// that is the registry's question, answered by
/// [`crate::spawn_seam::built_in_adapter_names`] ∪ the TOML inventory, and the
/// caller asks both. A name that is dispatchable but unresolvable, and a name
/// that is neither, fail for different reasons and get different sentences.
#[must_use]
pub fn endpoint_is_derived(adapters: Option<&AdaptersConfig>, seat: &str) -> bool {
    let entry = adapters.and_then(|a| a.entry(seat));
    let base_url = entry.and_then(|e| e.base_url.as_deref());
    let model = entry.and_then(|e| e.default_model.as_deref());
    // The one question, asked once: does the RECORD resolve to a family? Not
    // "is a key present" — that is the property next to it, and the gap between
    // the two is where `default_model = ""` walked through.
    if family_from_record(base_url, model).is_some() {
        return true;
    }
    is_named_vendor(&family_from_name(seat))
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
/// # A declared host is never traded for the section name
///
/// An **unrecognised** host is not the same state as **no** host. It is the
/// proxy-costume case itself — an operator pointing a seat at something this
/// table does not know. Falling back to the adapter-name lineage there would let
/// two seats behind one private proxy read as two families purely because
/// their config sections are spelled differently, which is exactly the
/// name-as-distinctness-axis the invariant forbids (see the module header).
/// So on that branch the resolution order stops at model lineage, then the
/// host itself; the adapter name is consulted only when `base_url` is absent
/// entirely, where there is nothing else on the record to consult.
///
/// The label is **derived, not attested** (see the module header): tier (a)
/// trusts the operator-supplied `base_url`/`model`; tier (b) does not.
#[must_use]
pub fn provider_family(base_url: Option<&str>, model: Option<&str>, adapter_name: &str) -> String {
    // The name is consulted only where the record says nothing — and "says
    // nothing" is decided in exactly one place, which is also the place
    // `endpoint_is_derived` asks.
    family_from_record(base_url, model).unwrap_or_else(|| family_from_name(adapter_name))
}

/// The family the **record** resolves to — `base_url` host + `model` lineage —
/// or `None` when the record says nothing at all.
///
/// # Why this is a separate function, and why it returns `Option`
///
/// It is the single answer to the two questions that must never diverge:
/// *what does this seat resolve to?* ([`provider_family`], which supplies the
/// name fallback on `None`) and *is there anything to resolve?*
/// ([`endpoint_is_derived`], which reads the `None` itself). They were two
/// separate implementations until 2026-07-28, one testing content and one
/// testing presence, and `default_model = ""` was admitted by the second and
/// rejected by the first — a seat whose family could then only ever be its own
/// label, on the axis the roster gate is built to defend.
///
/// A **blank** `base_url` is treated as no `base_url`, for the same reason: an
/// empty string is not a declared host, it is an unfilled field, and returning
/// it as an identity would make `""` a family label. That is distinct from an
/// unrecognised host, which IS a declaration and keeps its own branch below.
fn family_from_record(base_url: Option<&str>, model: Option<&str>) -> Option<String> {
    if let Some(url) = declared(base_url) {
        let host = url.to_ascii_lowercase();
        if host.contains("anthropic") {
            return Some("anthropic".to_string());
        }
        if host.contains("openai.com") {
            return Some("openai".to_string());
        }
        if host.contains("x.ai") {
            return Some("xai".to_string());
        }
        if host.contains("moonshot") {
            return Some("moonshot".to_string());
        }
        if host.contains("deepseek") {
            return Some("deepseek".to_string());
        }
        if host.contains("googleapis") || host.contains("generativelanguage") {
            return Some("google".to_string());
        }
        if is_local_host(&host) {
            // Local endpoint: the vendor is the operator, so family is the
            // model's lineage, not "who hosts it".
            return Some(family_from_model(model).unwrap_or_else(|| "local".to_string()));
        }
        // Unknown host — and this is the branch a proxy-costume actually takes.
        // A DECLARED but unrecognised host is not the same state as no host at
        // all: it is precisely the case ADR-147 exists to defeat, so it must
        // never reach `family_from_name`. Model lineage first (it names the
        // weights, which is what family means), then the host itself, so two
        // seats behind one private proxy collide and two distinct proxies stay
        // distinct. The adapter section name is not consulted on this path.
        if let Some(fam) = family_from_model(model) {
            return Some(fam);
        }
        // NORMALIZED, not the raw lowercased string. The host is being used as
        // an identity here, and an identity with two spellings is two
        // identities: `https://proxy.internal/v1` and the same URL with a
        // trailing slash would collapse in the tuple's `base_url` component
        // (which has always been normalized) and stay distinct in `family`, so
        // one seat contradicted itself and two seats behind one proxy counted
        // as two witnesses again — the exact collapse this branch exists to
        // produce, defeated by a character.
        return Some(normalize_base_url(Some(url)));
    }
    // No base_url on the record — a vendor-default seat. The model lineage is
    // the last thing the RECORD can say; `None` from here is what licenses the
    // caller's name fallback, and what `endpoint_is_derived` reads as "this
    // seat can only ever restate its own label".
    family_from_model(model)
}

/// A field the operator actually **filled in** — `None` for absent, empty, and
/// whitespace-only alike.
///
/// The three are one state (*nothing was declared*) everywhere in this module,
/// and treating them as two is how `default_model = ""` bought a seat the
/// appearance of a record. Applied to `base_url` here; `family_from_model`
/// already trims its own argument, and keeping that check where the string is
/// consumed is what makes both callers inherit it.
fn declared(field: Option<&str>) -> Option<&str> {
    field.map(str::trim).filter(|f| !f.is_empty())
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
    // `declared`, not the bare `Option`: a blank `base_url` is an unfilled
    // field, and returning it here would make `""` a provider kind — the same
    // empty-string-as-identity the family axis was carrying.
    if let Some(url) = declared(base_url) {
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

/// The families this module can name with real confidence — the vendors whose
/// endpoints and model lineages it knows by construction.
///
/// Everything else that [`provider_family`] can return is a *placeholder*, not
/// a vendor: `"local"` (an on-box endpoint that will happily serve any
/// lineage), `"unknown"` (nothing was declared), and the id-is-the-family
/// fallback for an unrecognised model. Those must never be compared against
/// each other, because a disagreement between two placeholders says nothing.
/// Naming the confident set here is what lets
/// [`classify_model_composition`] answer *I did not check* instead of
/// guessing.
fn is_named_vendor(family: &str) -> bool {
    matches!(
        family,
        "anthropic" | "openai" | "xai" | "moonshot" | "deepseek" | "google"
    )
}

/// What is known about the composition of an `(adapter, model)` pair —
/// **including the case where nothing is known**, which is the whole point.
///
/// # Why this type exists
///
/// A model id that resolves is not a model the adapter will accept. Measured
/// 2026-07-28: `cs tackle <seat> --adapter codex` with `ANTHROPIC_MODEL` set in
/// the dispatching shell resolved to `(codex, claude-opus-5)` and was
/// dispatched; codex rejected it at launch with an HTTP 400 —
/// *"The 'claude-opus-5' model is not supported when using Codex with a
/// `ChatGPT` account"* — and the seat then sat mute at a prompt, indistinguishable
/// from a provider refusal. Two earlier seats in the same lineage recorded
/// `{"model":"claude-opus-5","outcome":"available"}` in their own
/// `model-selection.json`: a probe reported a **claude** model available *for a
/// codex seat*. It had measured that the id resolves, never that the pair is
/// legal.
///
/// # Why three variants and not a boolean
///
/// A boolean forces a guess in the case that actually matters. Composition is
/// decidable only when *both* sides resolve to a named vendor; a local
/// endpoint, an undeclared adapter, or an unrecognised model id leaves it
/// genuinely open. [`NotChecked`](Self::NotChecked) is that answer written
/// down, so a caller reports what it did not verify rather than a positive
/// signal it never earned.
///
/// # Why not an allowlist
///
/// There is no table of legal `(adapter, model)` pairs here and there must not
/// be: such a table is wrong the week a vendor ships a model. The verdict is
/// **derived** from the same resolution the diversity floor already uses —
/// [`provider_family`] for the adapter, the model-lineage prefixes for the id —
/// so a new `gpt-…` or `claude-…` needs no edit, and anything unrecognised
/// lands in `NotChecked` rather than in a false refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "verdict")]
pub enum ModelComposition {
    /// Both sides resolved to the same named vendor. The pair is coherent.
    Coherent {
        /// The vendor family both sides agree on.
        family: String,
    },
    /// Both sides resolved to a named vendor and they **disagree** — the
    /// adapter will reject this model. This is the typed refusal a dispatch
    /// should fail closed on.
    Incoherent {
        /// The family the adapter resolves to (from `base_url`, else its name).
        adapter_family: String,
        /// The family the model id resolves to.
        model_family: String,
    },
    /// Nothing was decided, and here is why. Never report this as a positive
    /// signal: it is the honest statement that the pair went unvalidated.
    NotChecked {
        /// What could not be resolved to a named vendor, in plain words.
        reason: String,
    },
}

impl ModelComposition {
    /// Whether this verdict is a refusal a dispatch must fail closed on.
    ///
    /// Deliberately `true` for [`Incoherent`](Self::Incoherent) alone:
    /// `NotChecked` is not a refusal, because refusing on everything unknown
    /// would break every local and self-hosted endpoint cosmon supports.
    #[must_use]
    pub const fn is_refusal(&self) -> bool {
        matches!(self, Self::Incoherent { .. })
    }
}

/// Classify the composition of a `(adapter, model)` pair — the check that was
/// missing when a probe called `(codex, claude-opus-5)` "available".
///
/// The adapter's family is resolved from its `[adapters.<name>].base_url` and,
/// failing that, its name lineage — deliberately **without** consulting its
/// `default_model`, since the question here is about a *different*, pinned
/// model and a configured default would mask the very mismatch being asked
/// about. The model's family is resolved from its id prefix.
///
/// Returns [`ModelComposition::NotChecked`] whenever either side is not a named
/// vendor (a local endpoint, an undeclared adapter, an unrecognised model id),
/// which is the honest answer and never a refusal. See [`ModelComposition`] for
/// why this derives rather than tabulates.
#[must_use]
pub fn classify_model_composition(
    adapters: Option<&AdaptersConfig>,
    adapter_name: &str,
    model: &str,
) -> ModelComposition {
    let base_url = adapters
        .and_then(|a| a.entry(adapter_name))
        .and_then(|e| e.base_url.clone());
    let adapter_family = provider_family(base_url.as_deref(), None, adapter_name);
    let model_family = family_from_model(Some(model)).unwrap_or_else(|| "unknown".to_string());

    if !is_named_vendor(&adapter_family) {
        return ModelComposition::NotChecked {
            reason: format!(
                "adapter '{adapter_name}' resolves to '{adapter_family}', which is not a \
                 named vendor (a local or self-hosted endpoint serves any lineage, and an \
                 undeclared one states nothing) — the pair was NOT validated"
            ),
        };
    }
    if !is_named_vendor(&model_family) {
        return ModelComposition::NotChecked {
            reason: format!(
                "model '{model}' has no recognised vendor lineage — the pair was NOT \
                 validated, only the adapter's family ('{adapter_family}') is known"
            ),
        };
    }
    if adapter_family == model_family {
        ModelComposition::Coherent {
            family: adapter_family,
        }
    } else {
        ModelComposition::Incoherent {
            adapter_family,
            model_family,
        }
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

    /// **A5 falsifier.** A DECLARED but unrecognised host must never be traded
    /// for the adapter section name.
    ///
    /// Two seats behind one private proxy, no model pinned, spelled
    /// differently in `[adapters]`. Under the old fallthrough both reached
    /// `family_from_name` and read as the families `openai` and `anthropic` —
    /// two witnesses out of one endpoint, on the section name alone, which is
    /// the axis the invariant forbids. Restore that fallthrough and this test
    /// goes red on the first assertion.
    #[test]
    fn unknown_declared_host_never_falls_back_to_the_section_name() {
        let a = provider_family(Some("https://proxy.internal/v1"), None, "openai");
        let b = provider_family(Some("https://proxy.internal/v1"), None, "anthropic");
        assert_eq!(
            a, b,
            "two seats on ONE unrecognised host must resolve to ONE family; \
             they differ only in their config section name, which is not an \
             independence axis"
        );
        assert!(
            !a.contains("openai") && !a.contains("anthropic"),
            "family {a:?} was read off the section name, not the endpoint"
        );

        // Distinct unknown hosts still stay distinct — the conservative
        // default is collapse-on-same, not collapse-on-everything.
        assert_ne!(
            provider_family(Some("https://proxy-a.internal/v1"), None, "seat"),
            provider_family(Some("https://proxy-b.internal/v1"), None, "seat"),
        );

        // Model lineage still outranks the host when one is pinned.
        assert_eq!(
            provider_family(
                Some("https://proxy.internal/v1"),
                Some("claude-opus-5"),
                "openai"
            ),
            "anthropic"
        );
    }

    /// **R3-5.** The A5 falsifier above compares two BYTE-IDENTICAL host
    /// strings, so it cannot see that the unknown-host branch returned the raw
    /// lowercased URL instead of the normalized one. Two spellings of one
    /// endpoint — a trailing slash, some padding — then read as two families,
    /// and two seats behind one proxy counted as two witnesses again. The same
    /// collapse the branch was written to produce, defeated by a character.
    ///
    /// Revert the branch to `return host;` and the first assertion goes red.
    #[test]
    fn two_spellings_of_one_unknown_host_are_one_family() {
        assert_eq!(
            provider_family(Some("https://proxy.internal/v1"), None, "a"),
            provider_family(Some("https://proxy.internal/v1/"), None, "b"),
            "a trailing slash is not an error-independence axis"
        );
        assert_eq!(
            provider_family(Some("https://proxy.internal/v1"), None, "a"),
            provider_family(Some("  HTTPS://Proxy.Internal/v1  "), None, "b"),
            "neither is case or padding"
        );
        // The tuple's own base_url component has always been normalized; the
        // family must agree with it rather than carry a second spelling.
        let tuple = resolve_endpoint_tuple(
            Some(&adapters_with(&[(
                "seat",
                Some("https://proxy.internal/v1/"),
                None,
            )])),
            "seat",
        );
        assert_eq!(
            tuple.family, tuple.base_url,
            "on an unrecognised host the family IS the host, so the two \
             components must be spelled identically or one seat contradicts \
             itself"
        );
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

    /// `codex` resolves WITHOUT an `[adapters.codex]` section — the property
    /// the roster gate must measure is resolvability, not section-existence.
    ///
    /// The section is optional for a CLI adapter (it only tunes launch mode),
    /// so a gate keyed on its presence refuses a seat cosmon really dispatches.
    #[test]
    fn a_cli_adapter_with_no_section_is_still_derived() {
        let adapters = adapters_with(&[("claude", None, Some("claude-opus-4-8"))]);
        assert!(
            endpoint_is_derived(Some(&adapters), "codex"),
            "`codex` names the OpenAI CLI; that is a fact about the binary, \
             not a restatement of the label"
        );
        // And the derivation can genuinely CONTRADICT a seat: the resolved
        // family is `openai`, never `codex`. Without this, admitting the seat
        // would just move the self-attestation one step along.
        assert_eq!(
            resolve_endpoint_tuple(Some(&adapters), "codex").family,
            "openai"
        );
    }

    /// A ghost name is NOT derived — the identity fallback is the whole hole.
    #[test]
    fn a_name_of_no_known_lineage_is_not_derived() {
        let adapters = adapters_with(&[("claude", None, Some("claude-opus-4-8"))]);
        assert!(!endpoint_is_derived(Some(&adapters), "ghostseat"));
        // Proof of WHY: the tuple is the label coming back around.
        assert_eq!(
            resolve_endpoint_tuple(Some(&adapters), "ghostseat").family,
            "ghostseat"
        );
    }

    /// A provider-AGNOSTIC CLI is not derived from its name either. `aider`
    /// and `ollama` will serve whatever weights they are pointed at, so cosmon
    /// knows nothing about the endpoint until the operator declares one.
    #[test]
    fn a_provider_agnostic_adapter_is_not_derived_from_its_name() {
        assert!(!endpoint_is_derived(None, "aider"));
        assert!(!endpoint_is_derived(None, "ollama"));
    }

    /// An EMPTY section is not a record. Resolution falls straight through to
    /// the name, so `[adapters.foo]` with neither `base_url` nor
    /// `default_model` buys a seat the appearance of derivation and none of it.
    #[test]
    fn an_empty_section_does_not_make_a_name_derived() {
        let adapters = adapters_with(&[("mystery", None, None)]);
        assert!(!endpoint_is_derived(Some(&adapters), "mystery"));
        // One declared field is enough — that IS a record.
        let declared = adapters_with(&[("mystery", None, Some("qwen3-32b"))]);
        assert!(endpoint_is_derived(Some(&declared), "mystery"));
    }

    // ── F5 — admission asked for a KEY while resolution asked for CONTENT ──

    /// **The falsifier.** `default_model = ""` was admitted by
    /// `endpoint_is_derived` (`is_some()` on the `Option`) and rejected by
    /// `family_from_model` (which trims and returns `None` on the empty
    /// string), so the tuple fell through to `family_from_name(seat)` — the
    /// seat's own label — on exactly the axis the predicate defends.
    ///
    /// Measured before the fix: `[adapters.aider]` with **no**
    /// `default_model` was refused as a self-attestation, and the identical
    /// section with `default_model = ""` was admitted. The operator did not
    /// have to lie; only to type nothing where typing nothing looks like
    /// configuration.
    #[test]
    fn a_blank_declaration_is_not_a_record() {
        let absent = adapters_with(&[("aider", None, None)]);
        let blank = adapters_with(&[("aider", None, Some(""))]);
        let spaces = adapters_with(&[("aider", None, Some("   "))]);

        // The anchor: absence is refused, and always was.
        assert!(!endpoint_is_derived(Some(&absent), "aider"));
        // The defect: two quote marks must not change that answer.
        assert!(
            !endpoint_is_derived(Some(&blank), "aider"),
            "`default_model = \"\"` must refuse exactly as an absent key does — \
             admission tested PRESENCE while resolution tests CONTENT"
        );
        assert!(!endpoint_is_derived(Some(&spaces), "aider"));

        // WHY it must: this is the tuple the admitted seat would have carried.
        assert_eq!(
            resolve_endpoint_tuple(Some(&blank), "aider").family,
            "aider",
            "the blank model resolves to the seat's own label, so `declared == \
             resolved` holds by construction and no roster can be contradicted"
        );

        // A blank `base_url` is the same state arriving through the other key.
        let blank_url = adapters_with(&[("aider", Some(""), None)]);
        assert!(!endpoint_is_derived(Some(&blank_url), "aider"));
        assert_eq!(
            resolve_endpoint_tuple(Some(&blank_url), "aider").provider,
            "aider",
            "an empty string is an unfilled field, never a provider identity"
        );
    }

    /// **The counterweight.** A gate that refuses everything is an outage, not
    /// a fix: a real model string on the same section must still admit, and
    /// must still resolve to something that can contradict the seat.
    #[test]
    fn a_real_declaration_on_the_same_section_still_admits() {
        let real = adapters_with(&[("aider", None, Some("kimi-k2.6"))]);
        assert!(endpoint_is_derived(Some(&real), "aider"));
        assert_eq!(
            resolve_endpoint_tuple(Some(&real), "aider").family,
            "moonshot",
            "the resolved family is the model's lineage, NOT the adapter label \
             — which is what makes a lying roster refusable"
        );

        // And through the URL key, including the whitespace-padded spelling an
        // operator really types.
        let padded = adapters_with(&[("aider", Some("  https://api.deepseek.com  "), None)]);
        assert!(endpoint_is_derived(Some(&padded), "aider"));
        assert_eq!(
            resolve_endpoint_tuple(Some(&padded), "aider").family,
            "deepseek"
        );
    }

    /// The structural half of the fix: admission and resolution may not be two
    /// implementations that can drift apart again. For every shape of record,
    /// `endpoint_is_derived` must agree with *what the resolver actually did* —
    /// derived exactly when the resolved family is not the name fallback.
    #[test]
    fn admission_agrees_with_resolution_on_every_shape_of_record() {
        let cases: &[(Option<&str>, Option<&str>)] = &[
            (None, None),
            (Some(""), None),
            (None, Some("")),
            (Some("   "), Some("  ")),
            (None, Some("claude-opus-5")),
            (Some("https://api.openai.com/v1"), None),
            (Some("http://localhost:11434"), Some("qwen3-32b")),
            (Some("https://proxy.internal/v1"), None),
        ];
        for (base_url, model) in cases {
            let adapters = adapters_with(&[("seat", *base_url, *model)]);
            let derived = endpoint_is_derived(Some(&adapters), "seat");
            // `seat` belongs to no vendor lineage, so the name arm contributes
            // nothing and the record is the only thing that can admit it.
            let resolves = family_from_record(*base_url, *model).is_some();
            assert_eq!(
                derived, resolves,
                "admission and resolution disagreed on (base_url {base_url:?}, \
                 model {model:?}) — that gap IS the defect"
            );
        }
    }
}
