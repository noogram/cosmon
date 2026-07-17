// SPDX-License-Identifier: AGPL-3.0-only

//! Multi-LLM provider adapters for cosmon.
//!
//! Each adapter is a concrete struct with inherent `id` / `capabilities` /
//! `complete` methods. There is no shared trait — the original
//! `LlmProvider` port abstraction was deleted
//! after a kill-switch grep showed zero non-test live callers.
//! Concrete dispatch in `cs tackle` is a `match adapter.as_str()`,
//! so the abstraction earned nothing today.
//!
//! - [`ProviderId`] remains the persisted discriminant (matched on for
//!   routing, quota accounting, audit logs).
//! - `claude_code`, [`claude_api`], [`gemini`], [`ollama`], `llama`
//!   (feature-gated) are the concrete adapters.
//! - `degradation` answers *"which cosmon verb-classes can I trust this
//!   backend with?"* — pure query methods on [`Capabilities`], the
//!   executable twin of the ADR-118 graceful-degradation matrix.
//!
//! See `docs/adr/043-provider-abstraction.md` for the original rationale,
//! `docs/adr/118-llmport-doctrine-and-degradation-matrix.md` for the
//! LLMPort doctrine and the degradation matrix,
//! internal field-heuristic notes for error/capability
//! design, and chronicle `2026-05-19-w6-speculative-rip.md` for the rip.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod capabilities;
mod degradation;
mod error;
mod provider;
mod request;
mod secret;

// tolnay F19 (delib-20260519-e6db): module demoted from `pub mod`
// to private with an explicit `pub use` re-export below. Frees the
// module path as an internal renaming dimension — only the type is
// part of the public surface.
mod claude_code;

#[cfg(feature = "http")]
pub mod anthropic;
#[cfg(feature = "http")]
pub mod claude_api;
#[cfg(feature = "http")]
pub mod gemini;
#[cfg(feature = "http")]
pub mod ollama;
#[cfg(feature = "http")]
pub mod openai;

pub use capabilities::{Capabilities, RateLimitHint};
pub use degradation::{reliability_at, DegradationTier, Reliability, VerbClass};
pub use error::{is_content_filter_signal, ProviderError, TransportError};
pub use provider::ProviderId;
pub use request::{
    CompletionRequest, CompletionResponse, FinishReason, GrammarFormat, Message, Role, Usage,
};
pub use secret::Secret;

pub use claude_code::ClaudeCodeProvider;

#[cfg(feature = "http")]
pub use anthropic::AnthropicProvider;
#[cfg(feature = "http")]
pub use claude_api::ClaudeApiProvider;
#[cfg(feature = "http")]
pub use gemini::GeminiProvider;
#[cfg(feature = "http")]
pub use ollama::OllamaProvider;
#[cfg(feature = "http")]
pub use openai::OpenAIProvider;
