// SPDX-License-Identifier: AGPL-3.0-only

//! LLM backend port — the trait every concrete model adapter implements.
//!
//! This module is the **port** in the hexagonal sense: it is the
//! domain-side contract for "talk to a language model". Concrete
//! adapters live outside `cosmon-core`:
//!
//! * `cosmon_bridge_claude::AnthropicSubprocess` — wraps the Claude
//!   Code subprocess (V0).
//! * Future `OllamaHttp`, `MlxNative`, etc. — wired in V1+.
//!
//! `cosmon-core` keeps the contract zero-I/O. The trait is async via
//! `async-trait` so it stays object-safe; the actual runtime
//! (Tokio/async-std) is an adapter concern.
//!
//! # Why every type here is `#[non_exhaustive]`
//!
//! These are V0 shapes drafted to keep V1's billing/streaming/tool-use
//! roadmap compatible *without* a major bump. Adding new fields
//! (`tokens_cached`, `cost_micros`, `tool_call`, …) becomes a minor
//! version. Pattern matches in dependent crates must already use a
//! `..` rest-pattern or `#[non_exhaustive]` will fail-fast on the
//! first ignored field — exactly the safety net we want.
//!
//! # Roadmap
//!
//! V0 ships the trait + a single adapter (`AnthropicSubprocess`).
//! V1 widens [`BackendCapabilities`] with cost vectors, adds a
//! streaming variant, threads [`crate::auth::TenantApiKey`] through
//! [`TenantContext`]. V2 introduces tool-use slots. The
//! `#[non_exhaustive]` markers keep every step a minor bump.
//!
//! The split between credentials (per-tenant BYOK keys) and billing
//! (cost vectors on the response) is deliberate: each can land in its
//! own minor version without forcing the other, so the port stays
//! forward-compatible with a future multi-tenant billing roadmap.

use crate::auth::{Subject, TenantApiKey, TenantId};

/// Capability advertised by a concrete backend, no I/O required.
///
/// Returned by [`LlmBackend::capabilities`] so the runtime can route
/// requests (e.g. *"give me a backend that supports streaming"*)
/// without paying for a network round-trip per probe.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    /// Provider tag — `"anthropic"`, `"ollama"`, `"mlx"`, …
    pub provider: String,
    /// Default model identifier, if the backend has one.
    ///
    /// `model_hint` (the *suggestion* shape) rather than `model`
    /// (the *imperative* shape) — every call-site is free to override
    /// per-request via [`CompletionRequest::model_hint`]. Same
    /// discipline as a `default` knob, not a forced rail.
    pub model_hint: Option<String>,
    /// Whether the backend can emit incremental token stream chunks.
    ///
    /// V0 backends today all set this to `false`; V1 will introduce a
    /// streaming variant of [`LlmBackend::complete`].
    pub supports_streaming: bool,
}

impl BackendCapabilities {
    /// Construct an advertisement — the canonical entry point for
    /// adapters defined outside `cosmon-core` (the struct itself is
    /// `#[non_exhaustive]` and rejects struct-literal construction).
    #[must_use]
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model_hint: None,
            supports_streaming: false,
        }
    }

    /// Builder-style override for the default model hint.
    #[must_use]
    pub fn with_model_hint(mut self, hint: impl Into<String>) -> Self {
        self.model_hint = Some(hint.into());
        self
    }

    /// Builder-style override for the streaming flag.
    #[must_use]
    pub fn with_streaming(mut self, supports: bool) -> Self {
        self.supports_streaming = supports;
        self
    }
}

/// Per-request execution context — tenant identity + optional BYOK.
///
/// A separate type from [`Subject`] because the tenant axis and the
/// principal axis decouple in V1+: a single `Subject` may execute
/// requests on behalf of several tenants (an admin viewing another
/// tenant's data), and the BYOK key follows the tenant, not the
/// caller.
///
/// V0: `tenant` mirrors the `Subject`'s id (mono-tenant). `api_key`
/// is `None` until BYOK lands. The shape is already wired so the
/// `LlmBackend::complete` signature does not need to change in V1.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct TenantContext {
    /// The tenant on whose behalf the request is executed.
    pub tenant: TenantId,
    /// The principal asking for the work to be done.
    pub subject: Subject,
    /// Per-tenant LLM credential, if BYOK is active. `None` in V0.
    pub api_key: Option<TenantApiKey>,
}

impl TenantContext {
    /// Construct a V0 mono-tenant context — no BYOK.
    #[must_use]
    pub fn new(tenant: TenantId, subject: Subject) -> Self {
        Self {
            tenant,
            subject,
            api_key: None,
        }
    }

    /// Attach a BYOK credential — V1 flow, gated by feature work.
    #[must_use]
    pub fn with_api_key(mut self, key: TenantApiKey) -> Self {
        self.api_key = Some(key);
        self
    }
}

/// A request issued to a backend.
///
/// V0 carries the bare prompt string. V1 will add `messages`,
/// `system`, `tools`, `max_tokens`, etc. — every addition stays a
/// minor bump because the struct is `#[non_exhaustive]`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// The user prompt — opaque to cosmon-core.
    pub prompt: String,
    /// Optional model override, defaulting to
    /// [`BackendCapabilities::model_hint`] when unset.
    pub model_hint: Option<String>,
}

impl CompletionRequest {
    /// Construct a request with the bare prompt, no model override.
    #[must_use]
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model_hint: None,
        }
    }

    /// Override the model hint for this single request.
    #[must_use]
    pub fn with_model_hint(mut self, model: impl Into<String>) -> Self {
        self.model_hint = Some(model.into());
        self
    }
}

/// A response from a backend.
///
/// V0 fields are the bare minimum the runtime needs: the text, plus
/// the in/out token counters used by the energy aggregator
/// (`{state_dir}/log/energy.jsonl`). V1 will add `cost_micros`,
/// `tokens_cached`, etc. — a minor bump.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// The assistant text.
    pub text: String,
    /// Input tokens charged to the request.
    pub tokens_in: u64,
    /// Output tokens charged to the request.
    pub tokens_out: u64,
}

/// Errors raised by a concrete [`LlmBackend`] implementation.
///
/// The variants are intentionally *transport-shaped* enough to map
/// onto HTTP-ish surfaces (timeout, transient, permanent) but stay
/// provider-agnostic. Adapters surface their own provider error
/// detail via the wrapped `String`.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The backend is unreachable or otherwise refusing requests.
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    /// The request did not complete within the allotted budget.
    #[error("backend timeout: {0}")]
    Timeout(String),

    /// The backend rejected the request as malformed.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// An I/O failure (subprocess, network, filesystem) wrapping the
    /// underlying error message.
    #[error("backend I/O error: {0}")]
    Io(String),
}

/// Trait implemented by every concrete LLM adapter.
///
/// Object-safe via `async-trait` — call sites can take
/// `Arc<dyn LlmBackend>` so the runtime can swap adapters without
/// pulling generic monomorphisation through every layer.
///
/// # `capabilities`
///
/// Pure (no I/O): returns the static advertisement so the runtime
/// can route requests without paying for a probe.
///
/// # `complete`
///
/// The hot-path entry. Takes a [`CompletionRequest`] plus a
/// [`TenantContext`] (V1+ multi-tenant ready). Returns
/// [`CompletionResponse`] or [`LlmError`].
#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    /// Static capability advertisement — no I/O.
    fn capabilities(&self) -> BackendCapabilities;

    /// Execute one request against the backend.
    async fn complete(
        &self,
        req: CompletionRequest,
        ctx: &TenantContext,
    ) -> Result<CompletionResponse, LlmError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_carries_provider_and_streaming_flag() {
        let caps = BackendCapabilities {
            provider: "anthropic".to_string(),
            model_hint: Some("claude-opus-4-7".to_string()),
            supports_streaming: false,
        };
        assert_eq!(caps.provider, "anthropic");
        assert!(!caps.supports_streaming);
    }

    #[test]
    fn completion_request_default_has_no_model_override() {
        let r = CompletionRequest::new("hello");
        assert_eq!(r.prompt, "hello");
        assert!(r.model_hint.is_none());
    }

    #[test]
    fn completion_request_with_model_hint() {
        let r = CompletionRequest::new("hello").with_model_hint("claude-haiku-4-5");
        assert_eq!(r.model_hint.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn tenant_context_v0_has_no_api_key() {
        let ctx = TenantContext::new(TenantId::new("tenant-demo").unwrap(), Subject::operator());
        assert!(ctx.api_key.is_none());
    }

    #[test]
    fn tenant_context_with_byok_attaches_key() {
        let ctx = TenantContext::new(TenantId::new("tenant-demo").unwrap(), Subject::operator())
            .with_api_key(TenantApiKey::new("sk-x".to_string()));
        assert!(ctx.api_key.is_some());
    }

    #[test]
    fn llm_error_display_redacts_nothing_sensitive() {
        let e = LlmError::Unavailable("connection refused".to_string());
        assert_eq!(format!("{e}"), "backend unavailable: connection refused");
    }
}
