// SPDX-License-Identifier: AGPL-3.0-only

//! Capability advertisement — what a provider can and cannot do.
//!
//! Capabilities are static per-provider metadata. They let callers dispatch
//! (reject a vision-only request against a text-only model, pre-cap
//! `max_tokens`, decide whether to fall back to non-streaming) **without**
//! round-tripping through the network.
//!
//! The `max_context` field in particular drives the pre-request clamp
//! documented in the Dust field heuristics: rather than parsing an overflow
//! error after the fact, callers clamp `max_tokens` before dispatch.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A hint about a provider's rate limits.
///
/// Providers rarely expose exact quotas. This is a *best-effort* advisory
/// used for back-pressure; the authoritative signal is still
/// [`super::ProviderError::RateLimited`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitHint {
    /// Conservative lower bound on requests-per-minute.
    pub requests_per_minute: Option<u32>,
    /// Conservative lower bound on tokens-per-minute.
    pub tokens_per_minute: Option<u32>,
    /// Default cool-off to apply after a 429 when the response carries no
    /// `Retry-After`. The Anthropic and OpenAI heuristics both suggest
    /// starting at ~500ms with exponential backoff.
    pub default_cooloff: Duration,
}

impl Default for RateLimitHint {
    fn default() -> Self {
        Self {
            requests_per_minute: None,
            tokens_per_minute: None,
            default_cooloff: Duration::from_millis(500),
        }
    }
}

/// Static capability advertisement for a concrete provider adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Maximum prompt+completion token budget accepted by the provider.
    pub max_context: u32,
    /// Whether incremental streaming is supported. When `false`, callers
    /// must use the adapter's non-streaming `complete` method.
    pub supports_streaming: bool,
    /// Whether tool/function calling is supported in the provider's
    /// canonical form.
    pub supports_tools: bool,
    /// Whether image/vision inputs are accepted.
    pub supports_vision: bool,
    /// Optional rate-limit hint; `None` means "unknown, treat 429 as the
    /// only signal".
    pub rate_limit_hint: Option<RateLimitHint>,
}

impl Capabilities {
    /// Return `true` if `requested_tokens` fits within [`Self::max_context`].
    ///
    /// This is a purely arithmetic check; it does not query the provider.
    pub fn can_fit(&self, requested_tokens: u32) -> bool {
        requested_tokens <= self.max_context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rate_limit_hint_is_500ms() {
        let hint = RateLimitHint::default();
        assert_eq!(hint.default_cooloff, Duration::from_millis(500));
        assert!(hint.requests_per_minute.is_none());
    }

    #[test]
    fn can_fit_respects_max_context() {
        let caps = Capabilities {
            max_context: 1_000,
            supports_streaming: false,
            supports_tools: false,
            supports_vision: false,
            rate_limit_hint: None,
        };
        assert!(caps.can_fit(999));
        assert!(caps.can_fit(1_000));
        assert!(!caps.can_fit(1_001));
    }
}
