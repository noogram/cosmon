// SPDX-License-Identifier: AGPL-3.0-only

//! V0 stub of an [`LlmBackend`] adapter for the Claude Code subprocess.
//!
//! The hot-path reality today is `cs tackle` spawning a full Claude
//! Code session in a tmux pane via the existing fleet machinery. That
//! pipeline is not a single async function call — it is a long-lived
//! interactive session orchestrated through several layers
//! (`cosmon-cli::cmd::tackle`, `cosmon-transport`, the Claude Code
//! subprocess itself).
//!
//! V0 therefore ships [`AnthropicSubprocess`] as the **placeholder
//! adapter**: it satisfies the [`LlmBackend`] trait surface so the
//! ports + adapters skeleton is wired
//! end-to-end, and the runtime can already accept
//! `Arc<dyn LlmBackend>`. The `complete` implementation deliberately
//! returns [`LlmError::Unavailable`] until V1 wires the actual
//! subprocess-bound completion path. (forgemaster's compromise:
//! *« même avec une seule impl »* — ship the trait + a single stub.)
//!
//! V1 will route `complete` through the existing subprocess
//! machinery, populating `tokens_in` / `tokens_out` from the
//! `claudion` JSONL parser and adding `streaming` once the pane
//! protocol allows incremental delivery.

use cosmon_core::llm::{
    BackendCapabilities, CompletionRequest, CompletionResponse, LlmBackend, LlmError, TenantContext,
};

/// V0 stub adapter that *would* drive a Claude Code subprocess.
///
/// Construction is environment-light: the adapter takes the desired
/// CLI binary name (e.g. `"claude"`) and a default model hint. The
/// real spawn pipeline is gated behind V1 — see module docs.
#[derive(Debug, Clone)]
pub struct AnthropicSubprocess {
    /// CLI binary name — e.g. `"claude"` for the Claude Code CLI.
    cli_bin: String,
    /// Default model hint, surfaced via [`BackendCapabilities`].
    model_hint: Option<String>,
}

impl AnthropicSubprocess {
    /// Construct a V0 stub adapter.
    #[must_use]
    pub fn new(cli_bin: impl Into<String>, model_hint: Option<String>) -> Self {
        Self {
            cli_bin: cli_bin.into(),
            model_hint,
        }
    }

    /// CLI binary name used to spawn the subprocess. Exposed for
    /// adapters that need to introspect the planned spawn target
    /// (e.g. the cs-cli `cs doctor` health check).
    #[must_use]
    pub fn cli_bin(&self) -> &str {
        &self.cli_bin
    }
}

#[async_trait::async_trait]
impl LlmBackend for AnthropicSubprocess {
    fn capabilities(&self) -> BackendCapabilities {
        let mut caps = BackendCapabilities::new("anthropic");
        if let Some(hint) = &self.model_hint {
            caps = caps.with_model_hint(hint.clone());
        }
        // V0 has no streaming — the subprocess pipeline only
        // surfaces the completed turn through tmux capture.
        caps
    }

    async fn complete(
        &self,
        _req: CompletionRequest,
        _ctx: &TenantContext,
    ) -> Result<CompletionResponse, LlmError> {
        // V0 placeholder. The actual completion path lives in
        // `cosmon-cli::cmd::tackle` + `cosmon-transport` and runs as a
        // long-lived tmux session, not a single async call. V1 will
        // refactor that pipeline through this adapter; until then,
        // returning Unavailable is the honest answer — the trait
        // surface exists, the wiring does not yet.
        Err(LlmError::Unavailable(
            "AnthropicSubprocess::complete is a V0 stub; the live path is `cs tackle`".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::auth::{Subject, TenantId};

    #[tokio::test]
    async fn stub_advertises_anthropic_provider() {
        let backend = AnthropicSubprocess::new("claude", Some("claude-opus-4-7".to_string()));
        let caps = backend.capabilities();
        assert_eq!(caps.provider, "anthropic");
        assert_eq!(caps.model_hint.as_deref(), Some("claude-opus-4-7"));
        assert!(!caps.supports_streaming);
    }

    #[tokio::test]
    async fn stub_complete_returns_unavailable() {
        let backend = AnthropicSubprocess::new("claude", None);
        let ctx = TenantContext::new(TenantId::new("tenant-demo").unwrap(), Subject::operator());
        let result = backend
            .complete(CompletionRequest::new("hello"), &ctx)
            .await;
        assert!(matches!(result, Err(LlmError::Unavailable(_))));
    }
}
