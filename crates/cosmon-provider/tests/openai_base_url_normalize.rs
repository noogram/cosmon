// SPDX-License-Identifier: AGPL-3.0-only

//! GAP #5 — `/v1` suffix normalization on `OpenAIProvider::with_base_url`.
//!
//! Smoke-test source: an internal chronicle.
//! The xAI doc
//! publishes `base_url = https://api.x.ai/v1`; the agent loop appends
//! `/v1/chat/completions` to whatever `base_url` carries, so a naive
//! concat yields `…/v1/v1/chat/completions` and a 404 the operator
//! reads as "wrong key / wrong model". The constructor therefore
//! strips a trailing `/v1` (with or without slash) and warns.
//!
//! These assertions pin the structural invariant: three semantically
//! equivalent inputs MUST produce one identical resolved chat URL.

use cosmon_provider::openai::OpenAIProvider;

/// Reconstruct the URL the agent loop emits, mirroring the format string
/// in `run_agent_loop`. Kept verbatim so the test catches drift if either
/// the loop or the constructor changes shape.
fn resolved_chat_url(provider: &OpenAIProvider) -> String {
    format!(
        "{}/v1/chat/completions",
        provider.base_url().trim_end_matches('/')
    )
}

#[test]
fn host_root_form_is_canonical() {
    let p = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai");
    assert_eq!(p.base_url(), "https://api.x.ai");
    assert_eq!(
        resolved_chat_url(&p),
        "https://api.x.ai/v1/chat/completions"
    );
}

#[test]
fn trailing_v1_is_stripped() {
    let p = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai/v1");
    assert_eq!(
        p.base_url(),
        "https://api.x.ai",
        "constructor must strip trailing /v1 to avoid …/v1/v1/chat/completions"
    );
    assert_eq!(
        resolved_chat_url(&p),
        "https://api.x.ai/v1/chat/completions"
    );
}

#[test]
fn trailing_v1_with_slash_is_stripped() {
    let p = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai/v1/");
    assert_eq!(p.base_url(), "https://api.x.ai");
    assert_eq!(
        resolved_chat_url(&p),
        "https://api.x.ai/v1/chat/completions"
    );
}

#[test]
fn three_input_shapes_produce_identical_resolved_url() {
    let host_root = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai");
    let with_v1 = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai/v1");
    let with_v1_slash = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai/v1/");

    let a = resolved_chat_url(&host_root);
    let b = resolved_chat_url(&with_v1);
    let c = resolved_chat_url(&with_v1_slash);

    assert_eq!(a, b, "host-root vs /v1 must resolve to the same URL");
    assert_eq!(b, c, "/v1 vs /v1/ must resolve to the same URL");
    assert_eq!(a, "https://api.x.ai/v1/chat/completions");
}

#[test]
fn trailing_slash_only_is_stripped() {
    // No /v1 to strip — just the trailing slash. Verifies normalize_base_url
    // does not regress the simple case.
    let p = OpenAIProvider::with_base_url("k", "grok-2", "https://api.x.ai/");
    assert_eq!(p.base_url(), "https://api.x.ai");
}

#[test]
fn default_constructor_is_unaffected() {
    // DEFAULT_BASE_URL = "https://api.openai.com" — no /v1 suffix, no warn.
    let p = OpenAIProvider::new("k", "gpt-4o-mini");
    assert_eq!(p.base_url(), "https://api.openai.com");
    assert_eq!(
        resolved_chat_url(&p),
        "https://api.openai.com/v1/chat/completions"
    );
}

#[test]
fn moonshot_v1_form_is_also_stripped() {
    // Kimi/Moonshot doc shape mirrors xAI's — exercise the same trap path.
    let p = OpenAIProvider::with_base_url("k", "kimi-k1.5", "https://api.moonshot.ai/v1");
    assert_eq!(p.base_url(), "https://api.moonshot.ai");
    assert_eq!(
        resolved_chat_url(&p),
        "https://api.moonshot.ai/v1/chat/completions"
    );
}
