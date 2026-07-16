// SPDX-License-Identifier: AGPL-3.0-only

//! The [`ProviderId`] discriminant — the persisted enum routers, quota
//! accounting, and audit logs match on.
//!
//! Concrete adapter implementations live in sibling modules:
//! - [`crate::claude_code::ClaudeCodeProvider`] wraps the existing tmux
//!   integration; cosmon's default dispatch path.
//! - [`crate::claude_api::ClaudeApiProvider`] (feature `http`) talks to the
//!   Anthropic HTTP API directly.
//! - [`crate::ollama::OllamaProvider`] (feature `http`) targets a local
//!   Ollama daemon; useful for testing cosmon's cognitive scaffolding against
//!   deliberately weaker models.
//! - `crate::llama::LlamaProvider` (feature `llama`) drives a local GGUF
//!   model through the in-process `cosmon-llama` FFI wrapper — no HTTP, no
//!   daemon, no network at inference time.
//!
//! Each adapter exposes `id` / `capabilities` / `complete` as inherent
//! methods. The historical `LlmProvider` trait was deleted as speculative
//! scaffolding — see chronicle `2026-05-19-w6-speculative-rip.md`.

use serde::{Deserialize, Serialize};

/// Stable identifier for a concrete provider adapter.
///
/// This is the discriminant upper layers (routing, quota accounting, audit
/// logs) match on. New variants are additions, not renames — renaming a
/// variant is a breaking change for stored state.
///
/// `#[non_exhaustive]` — adding a new
/// provider variant (e.g. Mistral, Gemini, custom self-hosted) must not
/// require a major bump on the crate.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    /// Tmux-paste façade over the Claude Code CLI.
    ClaudeCode,
    /// Direct Anthropic HTTP API.
    ClaudeApi,
    /// Local Ollama HTTP daemon.
    Ollama,
    /// Direct Google Gemini HTTP API (`generativelanguage.googleapis.com`).
    ///
    /// Serialises as `"gemini"`. Like [`Self::ClaudeApi`] and [`Self::Ollama`]
    /// the concrete adapter is gated behind the `http` cargo feature, but the
    /// discriminant is unconditional so persisted state round-trips on any
    /// build. The `http` feature is on by default, so no
    /// [`Self::ensure_compiled`] guard is needed (only the default-off `llama`
    /// adapter requires one).
    Gemini,
    /// In-process llama.cpp inference via the `cosmon-llama` safe wrapper.
    ///
    /// Always present in the enum (persisted state cannot diverge by build
    /// flag — that would be a deserialisation soundness bug). Whether the
    /// concrete adapter compiles is gated by the `llama` cargo feature; see
    /// [`Self::ensure_compiled`] for the runtime check.
    ///
    /// **Wire format.** Variant name is `LlamaCpp` (canonical persisted form;
    /// the bare `llama` collides with the
    /// Meta model family). `#[serde(alias = "llama")]` accepts the legacy
    /// `"llama"` token for backward-compat reading of `state.json` files
    /// written before the rename. Serialisation always emits `"llama_cpp"`.
    #[serde(alias = "llama")]
    LlamaCpp,
}

impl ProviderId {
    /// Confirm that this provider id was compiled into the current binary,
    /// or return [`crate::ProviderError::FeatureNotCompiled`] otherwise.
    ///
    /// The enum is intentionally unconditional so persisted state can be
    /// deserialised on any build; this method is the runtime dispatch
    /// boundary that turns a "feature missing" mismatch into a typed error
    /// instead of a panic or compile error.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ProviderError::FeatureNotCompiled`] when the
    /// variant's adapter was not compiled into this build.
    pub fn ensure_compiled(self) -> Result<Self, crate::error::ProviderError> {
        // The in-process llama.cpp adapter was removed in the pre-publication
        // scope trim (ADR-126); the `LlamaCpp` variant survives only as a
        // state-deserialisation contract, so it is never compiled into any
        // build and always reports `FeatureNotCompiled`.
        if matches!(self, Self::LlamaCpp) {
            return Err(crate::error::ProviderError::FeatureNotCompiled("llama"));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cpp_variant_serialises_as_snake_case() {
        let id = ProviderId::LlamaCpp;
        let serialised = serde_json::to_string(&id).expect("serialise");
        // Canonical wire form is `"llama_cpp"` (snake_case of `LlamaCpp`).
        // The legacy `"llama"` token is read-only — accepted via serde
        // alias, never emitted.
        assert_eq!(serialised, "\"llama_cpp\"");
    }

    #[test]
    fn llama_cpp_variant_deserialises_regardless_of_feature_flag() {
        // Persisted state must round-trip on every build (default or
        // `--features llama`). Without this guarantee, a binary built
        // without the feature would crash when reading a
        // `ProviderId::LlamaCpp` stored by another build — the soundness
        // bug tolnay rule #6 names.
        let id: ProviderId = serde_json::from_str("\"llama_cpp\"")
            .expect("deserialise `\"llama_cpp\"` on any build");
        assert_eq!(id, ProviderId::LlamaCpp);
    }

    /// `state.json` files written before
    /// the rename carry `"llama"`. The serde alias must accept the
    /// legacy token on read so cosmon does not lose history when the
    /// canonical name flips.
    #[test]
    fn llama_cpp_variant_accepts_legacy_llama_alias_on_read() {
        let id: ProviderId = serde_json::from_str("\"llama\"")
            .expect("legacy `\"llama\"` token must still parse via serde alias");
        assert_eq!(id, ProviderId::LlamaCpp);
    }

    #[test]
    fn ensure_compiled_rejects_llama_cpp() {
        // The in-process llama adapter was removed (ADR-126); `LlamaCpp`
        // survives only as a state-deserialisation contract and is never
        // compiled, so `ensure_compiled` must always reject it.
        let err = ProviderId::LlamaCpp
            .ensure_compiled()
            .expect_err("must reject LlamaCpp — adapter removed");
        match err {
            crate::error::ProviderError::FeatureNotCompiled(name) => {
                assert_eq!(name, "llama");
            }
            other => panic!("expected FeatureNotCompiled, got {other:?}"),
        }
    }

    #[test]
    fn ensure_compiled_accepts_unconditional_variants() {
        assert_eq!(
            ProviderId::ClaudeCode
                .ensure_compiled()
                .expect("claude_code"),
            ProviderId::ClaudeCode
        );
    }

    #[test]
    fn gemini_variant_round_trips_as_snake_case() {
        let id = ProviderId::Gemini;
        let serialised = serde_json::to_string(&id).expect("serialise");
        assert_eq!(serialised, "\"gemini\"");
        let back: ProviderId = serde_json::from_str(&serialised).expect("deserialise");
        assert_eq!(back, ProviderId::Gemini);
    }

    /// `Gemini` rides the default `http` feature like `ClaudeApi`/`Ollama`,
    /// so `ensure_compiled` is an unconditional pass — no feature guard.
    #[test]
    fn ensure_compiled_accepts_gemini() {
        assert_eq!(
            ProviderId::Gemini.ensure_compiled().expect("gemini"),
            ProviderId::Gemini
        );
    }
}
