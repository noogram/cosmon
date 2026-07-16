// SPDX-License-Identifier: AGPL-3.0-only

//! Typed provider errors.
//!
//! Every fallible `complete` method on a concrete provider returns a [`ProviderError`].
//! There is deliberately **no** `anyhow::Error` in the public surface: the
//! cosmon thesis requires that error variants be exhaustively matchable so
//! that policy layers (retry, fallback, user-facing messaging) can dispatch
//! without string-sniffing.
//!
//! The variant set is informed by field heuristics extracted from the Dust
//! codebase, stripped
//! down to the invariants that matter for cosmon.

use std::time::Duration;

use thiserror::Error;

use crate::provider::ProviderId;

/// Transport-layer error — something failed below the provider protocol.
///
/// `#[non_exhaustive]` — future
/// transport classes (TLS verify failure, connection reset…) must not
/// require a major bump.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TransportError {
    /// Underlying HTTP, socket, or subprocess failure.
    #[error("I/O failure: {0}")]
    Io(String),
    /// The provider responded, but the body could not be decoded.
    #[error("protocol decode failure: {0}")]
    Decode(String),
    /// The caller's operation timed out before a response was received.
    #[error("operation timed out")]
    Timeout,
}

/// Errors returned by concrete provider `complete` methods.
///
/// `#[non_exhaustive]` — keeps
/// future provider-class additions non-breaking. The
/// [`Self::is_retryable`] match below carries a `_ => false` fallback
/// so the compiler tolerates future variants in the same crate.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The provider refused the request because a rate limit was hit.
    ///
    /// `retry_after` is the advisory back-off; callers should treat it as a
    /// lower bound and add their own jitter. It is populated from
    /// `Retry-After` when the provider supplies one, and from
    /// [`crate::RateLimitHint::default_cooloff`] otherwise.
    #[error("rate limited by {provider:?}; retry after {retry_after:?}")]
    RateLimited {
        /// Advisory back-off before retry.
        retry_after: Duration,
        /// Which provider rate-limited us.
        provider: ProviderId,
    },

    /// The provider refused the request because the account quota is
    /// exhausted (insufficient credit, suspended account, hard billing
    /// limit reached). Distinct from [`Self::RateLimited`]: a rate-limit
    /// is transient and clears with time, a quota breach is permanent
    /// until an operator action (recharge, plan upgrade, ban lift).
    ///
    /// Surfaced when the provider's error envelope names
    /// `exceeded_current_quota_error` (Moonshot), `insufficient_quota`
    /// (OpenAI), `credit_balance_too_low` (Anthropic), `account_suspended`,
    /// or the message text carries one of the equivalent textual signals
    /// (see `is_quota_signal` in the OpenAI / Anthropic adapter modules).
    ///
    /// Non-retryable by construction: [`Self::is_retryable`] returns
    /// `false`, so a naive retry loop will not pound a suspended endpoint
    /// indefinitely.
    #[error("quota exceeded by {provider:?} (recharge required): {message}")]
    QuotaExceeded {
        /// Which provider rejected the request.
        provider: ProviderId,
        /// Provider-supplied human-readable detail (account-suspended
        /// reason, billing-limit identifier, …). Empty when the
        /// provider only sent a status code with no body.
        message: String,
    },

    /// The provider blocked the model's **output** under a content-filter /
    /// moderation policy (Anthropic *"Output blocked by content filtering
    /// policy"*, OpenAI `finish_reason: "content_filter"`, …).
    ///
    /// **Unrecoverable by re-dispatch.** Distinct from [`Self::RateLimited`]
    /// (clears with time) and from [`Self::QuotaExceeded`] (clears with an
    /// operator recharge): a filtered output clears only when the *task* is
    /// changed so the model stops trying to emit the offending text. Re-running
    /// the identical generation re-trips the identical block — the
    /// pathology that burned ~$8 in a silent retry loop on
    /// `task-20260622-27d3` (a worker trying to LLM-emit the full CC-BY-4.0
    /// legal text). The fix is two-fold: the worker prompt now tells workers
    /// to *fetch* canonical/boilerplate texts rather than generate them
    /// (prevention), and this typed, **non-retryable** variant lets a loop
    /// break-and-escalate to the operator instead of pounding the endpoint
    /// (detection). [`Self::is_retryable`] returns `false`.
    ///
    /// Classified from the provider error envelope by
    /// [`is_content_filter_signal`].
    #[error("{provider:?} blocked output (content filter — unrecoverable by retry): {message}")]
    OutputFiltered {
        /// Which provider blocked the output.
        provider: ProviderId,
        /// Provider-supplied human-readable detail (the filter message),
        /// or the offending finish-reason / status when no message was sent.
        message: String,
    },

    /// The request's prompt+requested completion exceeds the model's context
    /// window. Reported pre-dispatch when the caller has a tokenizer, or
    /// post-dispatch when the provider returns a canonical overflow error.
    #[error("context overflow: model accepts {max_tokens}, request needs {requested}")]
    ContextOverflow {
        /// Maximum tokens the model will accept.
        max_tokens: u32,
        /// Token count computed (or reported) for this request.
        requested: u32,
    },

    /// API key / auth credential rejected by the provider.
    #[error("authentication rejected")]
    AuthInvalid,

    /// Transient 5xx / network / timeout — the caller may retry.
    #[error("transport failed: {0}")]
    TransportFailed(#[source] TransportError),

    /// Provider returned a non-retryable, provider-specific error (e.g.
    /// `invalid_request_error`, `moderation_blocked`). Kept as structured
    /// data so that upper layers can switch on `code` without parsing.
    #[error("{provider:?} rejected request: {code}: {message}")]
    ProviderSpecific {
        /// Which provider reported the error.
        provider: ProviderId,
        /// Provider-native error code.
        code: String,
        /// Human-readable message from the provider.
        message: String,
    },

    /// The request asked for a capability the provider does not advertise
    /// (e.g. vision, tools, streaming). Caught pre-dispatch against
    /// [`crate::Capabilities`].
    #[error("capability not supported by {provider:?}: {capability}")]
    UnsupportedCapability {
        /// Which provider cannot satisfy the request.
        provider: ProviderId,
        /// Short name of the missing capability.
        capability: &'static str,
    },

    /// The requested provider is recognised by [`ProviderId`] but its
    /// adapter was not compiled into this binary — typically because the
    /// associated cargo feature (`llama`, …) was off.
    ///
    /// This is the runtime counterpart to the `#[cfg(feature = "…")]` gate
    /// on the adapter module: persisted state can carry any [`ProviderId`]
    /// regardless of build flags, but trying to instantiate the adapter on
    /// a lean build returns this error instead of panicking. The payload
    /// is the feature name as it appears in `Cargo.toml`.
    #[error("provider feature not compiled into this build: {0}")]
    FeatureNotCompiled(&'static str),
}

impl ProviderError {
    /// Return `true` if a naive retry loop is likely to recover.
    ///
    /// This mirrors the Dust heuristics: 429 and transient 5xx are
    /// retryable; auth / overflow / capability / provider-specific
    /// rejections — and a content-filter [`Self::OutputFiltered`] block —
    /// are not.
    pub fn is_retryable(&self) -> bool {
        // `matches!` fallback (tolnay F7 of delib-20260519-e6db) —
        // the enum is `#[non_exhaustive]`; new variants default to
        // "not retryable" until their semantics are explicitly named.
        // `matches!` carries the equivalent `_ => false` arm and
        // satisfies the clippy `match_like_matches_macro` lint.
        matches!(self, Self::RateLimited { .. } | Self::TransportFailed(_))
    }
}

/// Return `true` when a provider error envelope (code / type field + message
/// text) signals an **output content-filter / moderation block** rather than a
/// transient or quota failure.
///
/// Mirrors [`crate::openai`]'s `is_quota_signal`: the primary signal is the
/// provider-native code/type (`output_filtered`, `content_filter`,
/// `content_policy_violation`, `moderation_blocked`), the secondary signal is
/// the message text — vendors converge on a small set of phrases. The most
/// load-bearing case is Anthropic's *"Output blocked by content filtering
/// policy"*, the message that drove `task-20260622-27d3` into a silent retry
/// loop.
///
/// Case-insensitive on both axes. Producers should classify a match as the
/// non-retryable [`ProviderError::OutputFiltered`] variant.
#[must_use]
pub fn is_content_filter_signal(code: &str, message: &str) -> bool {
    let c = code.to_ascii_lowercase();
    if matches!(
        c.as_str(),
        "output_filtered"
            | "content_filter"
            | "content_filtered"
            | "content_policy_violation"
            | "moderation_blocked"
            | "moderation"
    ) {
        return true;
    }
    let m = message.to_ascii_lowercase();
    m.contains("content filtering policy")
        || m.contains("content filter")
        || m.contains("output blocked")
        || (m.contains("blocked by") && m.contains("filtering"))
        || m.contains("flagged by content moderation")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_is_retryable() {
        let err = ProviderError::RateLimited {
            retry_after: Duration::from_secs(1),
            provider: ProviderId::ClaudeCode,
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn auth_invalid_is_not_retryable() {
        assert!(!ProviderError::AuthInvalid.is_retryable());
    }

    #[test]
    fn context_overflow_is_not_retryable() {
        let err = ProviderError::ContextOverflow {
            max_tokens: 100,
            requested: 200,
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn transport_failure_is_retryable() {
        let err = ProviderError::TransportFailed(TransportError::Timeout);
        assert!(err.is_retryable());
    }

    #[test]
    fn provider_specific_is_not_retryable() {
        let err = ProviderError::ProviderSpecific {
            provider: ProviderId::Ollama,
            code: "invalid_request".into(),
            message: "bad payload".into(),
        };
        assert!(!err.is_retryable());
    }

    /// QuotaExceeded names an operator-action condition (recharge,
    /// suspended account, billing limit). A naive retry loop must not
    /// pound the endpoint — the matter clears only by a human gesture.
    /// See cosmon-provider OpenAI/Anthropic adapter modules for the
    /// classifier that produces this variant.
    #[test]
    fn quota_exceeded_is_not_retryable() {
        let err = ProviderError::QuotaExceeded {
            provider: ProviderId::ClaudeApi,
            message: "credit balance too low".into(),
        };
        assert!(!err.is_retryable());
        assert!(err.to_string().contains("recharge required"));
    }

    /// An output content-filter block clears only when the *task* changes —
    /// neither time (rate-limit) nor a recharge (quota) recovers it. A naive
    /// retry loop re-tripping the identical block is exactly the
    /// `task-20260622-27d3` pathology (~$8 burned). The variant must be
    /// non-retryable so the loop breaks and escalates instead.
    #[test]
    fn output_filtered_is_not_retryable() {
        let err = ProviderError::OutputFiltered {
            provider: ProviderId::ClaudeApi,
            message: "Output blocked by content filtering policy".into(),
        };
        assert!(!err.is_retryable());
        assert!(err.to_string().contains("content filter"));
    }

    #[test]
    fn content_filter_signal_matches_anthropic_message() {
        assert!(is_content_filter_signal(
            "",
            "Output blocked by content filtering policy"
        ));
    }

    #[test]
    fn content_filter_signal_matches_known_codes() {
        assert!(is_content_filter_signal("content_filter", ""));
        assert!(is_content_filter_signal("output_filtered", ""));
        assert!(is_content_filter_signal("MODERATION_BLOCKED", "")); // case-insensitive
    }

    #[test]
    fn content_filter_signal_ignores_unrelated_errors() {
        assert!(!is_content_filter_signal("rate_limit", "too many requests"));
        assert!(!is_content_filter_signal("", "credit balance is too low"));
        assert!(!is_content_filter_signal(
            "invalid_request_error",
            "bad json"
        ));
    }

    #[test]
    fn feature_not_compiled_is_not_retryable() {
        let err = ProviderError::FeatureNotCompiled("llama");
        assert!(!err.is_retryable());
        assert_eq!(
            err.to_string(),
            "provider feature not compiled into this build: llama"
        );
    }
}
