// SPDX-License-Identifier: AGPL-3.0-only

//! Direct Anthropic HTTP API adapter.
//!
//! This adapter is **opt-in**: the default dispatch path remains Claude Code
//! (tmux-paste). [`ClaudeApiProvider`] exists for callers that want a
//! programmatic, non-interactive completion — synthetic tests, off-cluster
//! experiments, or benchmarking cosmon's scaffolding without the tmux
//! harness.
//!
//! Field-heuristic notes:
//!
//! - Auth header is `x-api-key`, not bearer.
//! - 429 carries `retry-after` in seconds.
//! - Context overflow is prevented by capping `max_tokens` against
//!   [`Capabilities::max_context`]; it is not reported as a separate error.
//! - 5xx is retryable, 4xx (other than 429) is not.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capabilities::{Capabilities, RateLimitHint};
use crate::error::{ProviderError, TransportError};
use crate::provider::ProviderId;
use crate::request::{CompletionRequest, CompletionResponse, FinishReason, Message, Role, Usage};
use crate::secret::Secret;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Anthropic HTTP API adapter.
///
/// `api_key` is wrapped in [`Secret`] so it never leaks through `Debug`
/// / `Display` / accidental `Serialize` paths — matching the
/// anthropic/openai adapters (the 2026-07-10 fix). The struct derives no
/// `Debug` today, but the redacting hand-written impl below keeps the
/// credential out of any future `{:?}` splatter structurally, not by
/// convention.
pub struct ClaudeApiProvider {
    client: reqwest::Client,
    api_key: Secret<String>,
    base_url: String,
    capabilities: Capabilities,
}

impl std::fmt::Debug for ClaudeApiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeApiProvider")
            .field("api_key", &self.api_key)
            .field("base_url", &self.base_url)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl ClaudeApiProvider {
    /// Build an adapter against the production Anthropic endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    /// Build an adapter against a custom base URL (for proxies or tests).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: Secret::new(api_key.into()),
            base_url: base_url.into(),
            capabilities: Capabilities {
                max_context: 200_000,
                supports_streaming: false,
                supports_tools: true,
                supports_vision: true,
                rate_limit_hint: Some(RateLimitHint::default()),
            },
        }
    }
}

#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    messages: Vec<ApiMessage<'a>>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize)]
struct ApiContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ApiUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ApiContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    #[serde(rename = "type")]
    code: String,
    message: String,
}

fn split_system(messages: &[Message]) -> (Option<String>, Vec<ApiMessage<'_>>) {
    let mut system: Option<String> = None;
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m.role {
            Role::System => {
                let acc = system.get_or_insert_with(String::new);
                if !acc.is_empty() {
                    acc.push('\n');
                }
                acc.push_str(&m.content);
            }
            Role::User => out.push(ApiMessage {
                role: "user",
                content: &m.content,
            }),
            Role::Assistant => out.push(ApiMessage {
                role: "assistant",
                content: &m.content,
            }),
        }
    }
    (system, out)
}

fn map_stop_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCall,
        Some(_) => FinishReason::Other,
        None => FinishReason::Other,
    }
}

impl ClaudeApiProvider {
    /// Stable identifier for this adapter.
    pub fn id(&self) -> ProviderId {
        ProviderId::ClaudeApi
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
        let max_tokens = request.max_tokens.unwrap_or(4_096);
        if !self.capabilities.can_fit(max_tokens) {
            return Err(ProviderError::ContextOverflow {
                max_tokens: self.capabilities.max_context,
                requested: max_tokens,
            });
        }

        let (system, messages) = split_system(&request.messages);
        let body = ApiRequest {
            model: &request.model,
            messages,
            max_tokens,
            system,
            temperature: request.temperature,
        };

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::TransportFailed(TransportError::Io(e.to_string())))?;

        let status = resp.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ProviderError::AuthInvalid);
        }

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| {
                    self.capabilities
                        .rate_limit_hint
                        .as_ref()
                        .map(|h| h.default_cooloff)
                        .unwrap_or(Duration::from_millis(500))
                });
            return Err(ProviderError::RateLimited {
                retry_after,
                provider: ProviderId::ClaudeApi,
            });
        }

        if status.is_server_error() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransportFailed(TransportError::Io(format!(
                "{}: {}",
                status, body
            ))));
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&body) {
                // An output content-filter block (*"Output blocked by content
                // filtering policy"*) is unrecoverable by re-dispatch — type
                // it so the loop breaks and escalates rather than retries
                // (task-20260623-80f9; the task-20260622-27d3 pathology).
                if crate::is_content_filter_signal(&env.error.code, &env.error.message) {
                    return Err(ProviderError::OutputFiltered {
                        provider: ProviderId::ClaudeApi,
                        message: env.error.message,
                    });
                }
                return Err(ProviderError::ProviderSpecific {
                    provider: ProviderId::ClaudeApi,
                    code: env.error.code,
                    message: env.error.message,
                });
            }
            if crate::is_content_filter_signal(&status.as_u16().to_string(), &body) {
                return Err(ProviderError::OutputFiltered {
                    provider: ProviderId::ClaudeApi,
                    message: body,
                });
            }
            return Err(ProviderError::ProviderSpecific {
                provider: ProviderId::ClaudeApi,
                code: status.as_u16().to_string(),
                message: body,
            });
        }

        let parsed: ApiResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::TransportFailed(TransportError::Decode(e.to_string())))?;

        let content = parsed
            .content
            .into_iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");

        let usage = parsed
            .usage
            .map(|u| Usage {
                prompt_tokens: u.input_tokens,
                completion_tokens: u.output_tokens,
            })
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            finish_reason: map_stop_reason(parsed.stop_reason.as_deref()),
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_system_merges_multiple_system_messages() {
        let msgs = vec![
            Message::system("a"),
            Message::user("u"),
            Message::system("b"),
        ];
        let (sys, rest) = split_system(&msgs);
        assert_eq!(sys.as_deref(), Some("a\nb"));
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].role, "user");
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason(Some("end_turn")), FinishReason::Stop);
        assert_eq!(map_stop_reason(Some("max_tokens")), FinishReason::Length);
        assert_eq!(map_stop_reason(Some("tool_use")), FinishReason::ToolCall);
        assert_eq!(map_stop_reason(Some("moderation")), FinishReason::Other);
        assert_eq!(map_stop_reason(None), FinishReason::Other);
    }

    #[test]
    fn context_overflow_is_caught_before_dispatch() {
        // Use an invalid base URL; if dispatch occurred we'd get a transport
        // error, but overflow check runs first.
        let p = ClaudeApiProvider::with_base_url("k", "http://127.0.0.1:1");
        let mut req = CompletionRequest::new("claude", "hi");
        req.max_tokens = Some(u32::MAX);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let err = rt.block_on(p.complete(req)).expect_err("overflow");
        assert!(matches!(err, ProviderError::ContextOverflow { .. }));
    }

    #[test]
    fn id_and_capabilities_are_stable() {
        let p = ClaudeApiProvider::new("k");
        assert_eq!(p.id(), ProviderId::ClaudeApi);
        assert!(p.capabilities().supports_tools);
    }

    #[test]
    fn debug_format_redacts_api_key() {
        // Structural guarantee: `api_key` is `Secret<String>`, so any `{:?}`
        // splatter of the provider prints `<redacted>` rather than the token
        // (the 2026-07-10 tracing-debug leak class). Reverting the field to a
        // bare `String` reddens this test.
        let p = ClaudeApiProvider::new("sk-ant-very-secret");
        let formatted = format!("{p:?}");
        assert!(
            !formatted.contains("sk-ant-very-secret"),
            "Debug must not contain the api key; got: {formatted}"
        );
        assert!(
            formatted.contains("redacted"),
            "Debug should mark api_key as redacted; got: {formatted}"
        );
    }
}
