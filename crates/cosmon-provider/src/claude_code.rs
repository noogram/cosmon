// SPDX-License-Identifier: AGPL-3.0-only

//! Façade provider over the Claude Code CLI in one-shot (`-p`) mode.
//!
//! This adapter is **additive**: it does not touch cosmon's tmux-based worker
//! dispatch path. Long-running workers continue to run via
//! [`cosmon_transport::claude`] exactly as before. This provider only exists
//! so that synthetic tests and non-interactive callers can obtain a
//! completion through the same [`LlmProvider`] surface used by other
//! adapters.
//!
//! The adapter shells out to `claude -p <prompt>`. It assumes the binary is
//! discoverable on `$PATH`; a custom path can be supplied via
//! [`ClaudeCodeProvider::with_binary`].

use std::ffi::OsString;

use tokio::process::Command;

use crate::capabilities::Capabilities;
use crate::error::{ProviderError, TransportError};
use crate::provider::ProviderId;
use crate::request::{CompletionRequest, CompletionResponse, FinishReason, Message, Role, Usage};

/// One-shot Claude Code CLI adapter.
pub struct ClaudeCodeProvider {
    binary: OsString,
    capabilities: Capabilities,
}

impl ClaudeCodeProvider {
    /// Construct an adapter that resolves `claude` on `$PATH`.
    pub fn new() -> Self {
        Self::with_binary("claude")
    }

    /// Construct an adapter pointing at a specific `claude` binary path.
    pub fn with_binary(binary: impl Into<OsString>) -> Self {
        Self {
            binary: binary.into(),
            capabilities: Capabilities {
                // Claude Sonnet 4.6 (1M) ceiling — the CLI binds the actual
                // model selection, we just advertise a generous upper bound.
                max_context: 1_000_000,
                supports_streaming: false,
                supports_tools: true,
                supports_vision: true,
                rate_limit_hint: None,
            },
        }
    }
}

impl Default for ClaudeCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn render_prompt(messages: &[Message]) -> String {
    let mut buf = String::new();
    for m in messages {
        let tag = match m.role {
            Role::System => "SYSTEM",
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        buf.push_str(tag);
        buf.push_str(": ");
        buf.push_str(&m.content);
        buf.push('\n');
    }
    buf
}

impl ClaudeCodeProvider {
    /// Stable identifier for this adapter.
    pub fn id(&self) -> ProviderId {
        ProviderId::ClaudeCode
    }

    /// Static capability advertisement.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Run a completion and return the full response.
    pub async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let prompt = render_prompt(&request.messages);

        let output = Command::new(&self.binary)
            .arg("-p")
            .arg(&prompt)
            .output()
            .await
            .map_err(|e| ProviderError::TransportFailed(TransportError::Io(e.to_string())))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into());
            // An output content-filter block is unrecoverable by re-dispatch;
            // type it so a retry loop breaks + escalates instead of pounding
            // the same blocked generation (task-20260623-80f9).
            if crate::is_content_filter_signal(&code, &stderr) {
                return Err(ProviderError::OutputFiltered {
                    provider: ProviderId::ClaudeCode,
                    message: stderr,
                });
            }
            return Err(ProviderError::ProviderSpecific {
                provider: ProviderId::ClaudeCode,
                code,
                message: stderr,
            });
        }

        let content = String::from_utf8(output.stdout)
            .map_err(|e| ProviderError::TransportFailed(TransportError::Decode(e.to_string())))?;

        Ok(CompletionResponse {
            content,
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_claude_code() {
        let p = ClaudeCodeProvider::new();
        assert_eq!(p.id(), ProviderId::ClaudeCode);
    }

    #[test]
    fn render_prompt_includes_all_roles() {
        let msgs = vec![
            Message::system("you are helpful"),
            Message::user("hi"),
            Message::assistant("hello"),
        ];
        let rendered = render_prompt(&msgs);
        assert!(rendered.contains("SYSTEM: you are helpful"));
        assert!(rendered.contains("USER: hi"));
        assert!(rendered.contains("ASSISTANT: hello"));
    }

    #[tokio::test]
    async fn missing_binary_surfaces_as_transport_failed() {
        let p = ClaudeCodeProvider::with_binary("/does/not/exist/claude-xyzzy");
        let err = p
            .complete(CompletionRequest::new("x", "hi"))
            .await
            .expect_err("should fail");
        assert!(matches!(
            err,
            ProviderError::TransportFailed(TransportError::Io(_))
        ));
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn nonzero_exit_surfaces_as_provider_specific() {
        // `false` is in every POSIX $PATH and exits 1 with empty stderr.
        let p = ClaudeCodeProvider::with_binary("false");
        let err = p
            .complete(CompletionRequest::new("x", "hi"))
            .await
            .expect_err("should fail");
        assert!(matches!(
            err,
            ProviderError::ProviderSpecific {
                provider: ProviderId::ClaudeCode,
                ..
            }
        ));
    }
}
