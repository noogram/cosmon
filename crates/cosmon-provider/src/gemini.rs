// SPDX-License-Identifier: AGPL-3.0-only

//! Direct Google Gemini HTTP API adapter.
//!
//! This adapter is the **fourth `complete`-style HTTP adapter**, joining
//! [`crate::claude_api`] and [`crate::ollama`] in the lightweight family —
//! a concrete struct with inherent `id` / `capabilities` / `complete`
//! methods over the provider-neutral [`CompletionRequest`] /
//! [`CompletionResponse`] types. It is **not** an agent-loop worker adapter
//! (that family is [`crate::openai`] / [`crate::anthropic`], which drive the
//! `cosmon-agent-harness` spine); [`GeminiProvider`] exists for callers that
//! want a programmatic, non-interactive single completion — synthetic tests,
//! off-cluster experiments, or benchmarking cosmon's scaffolding against the
//! Gemini family.
//!
//! Field-heuristic notes:
//!
//! - Endpoint is `POST {base}/v1beta/models/{model}:generateContent`; the
//!   model id is part of the *path*, not the JSON body (the shape that
//!   distinguishes Gemini from the OpenAI/Anthropic envelopes).
//! - Auth header is `x-goog-api-key`, **not** bearer and **not** the
//!   `?key=` query parameter — keeping the credential out of the URL keeps
//!   it out of access logs and `tracing` request spans.
//! - Roles are `user` and `model` (the assistant turn maps to `model`);
//!   system prompts move to a dedicated top-level `systemInstruction`
//!   object rather than an in-band message.
//! - 401/403 → [`ProviderError::AuthInvalid`]; 429 (`RESOURCE_EXHAUSTED`) →
//!   [`ProviderError::RateLimited`]; 5xx is retryable
//!   ([`ProviderError::TransportFailed`]); other 4xx surface as
//!   [`ProviderError::ProviderSpecific`] carrying Gemini's `status` string.
//! - Context overflow is prevented by capping `max_tokens` against
//!   [`Capabilities::max_context`]; it is not reported as a separate error.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capabilities::{Capabilities, RateLimitHint};
use crate::error::{ProviderError, TransportError};
use crate::provider::ProviderId;
use crate::request::{CompletionRequest, CompletionResponse, FinishReason, Message, Role, Usage};
use crate::secret::Secret;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
/// API surface version segment in the request path. `v1beta` is the
/// feature-complete surface Google publishes for `generateContent`
/// (tools, system instructions, JSON mode); `v1` lags it.
const API_VERSION: &str = "v1beta";
/// Header carrying the API key. The query-parameter form (`?key=`) is
/// deliberately avoided so the credential never lands in a URL log line.
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Google Gemini HTTP API adapter.
///
/// `api_key` is wrapped in [`Secret`] so it never leaks through `Debug`
/// / `Display` / accidental `Serialize` paths — matching the
/// anthropic/openai adapters (the 2026-07-10 fix). The struct derives no
/// `Debug` today, but the redacting hand-written impl below keeps the
/// credential out of any future `{:?}` splatter structurally, not by
/// convention.
pub struct GeminiProvider {
    client: reqwest::Client,
    api_key: Secret<String>,
    base_url: String,
    capabilities: Capabilities,
}

impl std::fmt::Debug for GeminiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiProvider")
            .field("api_key", &self.api_key)
            .field("base_url", &self.base_url)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl GeminiProvider {
    /// Build an adapter against the production Gemini endpoint.
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
                // Gemini 1.5/2.x advertise a 1 Mi-token input window; the
                // adapter clamps `max_tokens` against this before dispatch.
                max_context: 1_048_576,
                supports_streaming: false,
                supports_tools: true,
                supports_vision: true,
                rate_limit_hint: Some(RateLimitHint::default()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Wire envelope — Gemini generateContent
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct Content<'a> {
    role: &'a str,
    parts: [Part<'a>; 1],
}

#[derive(Serialize)]
struct SystemInstruction<'a> {
    parts: [Part<'a>; 1],
}

#[derive(Serialize)]
struct GenerationConfig {
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct GenRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Deserialize)]
struct RespPart {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<RespPart>,
}

#[derive(Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<CandidateContent>,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
}

#[derive(Deserialize)]
struct GenResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
}

// ---------------------------------------------------------------------------
// Pure mapping helpers
// ---------------------------------------------------------------------------

/// Split the neutral message list into Gemini's `(systemInstruction, contents)`
/// shape. System turns are concatenated (newline-joined) into the dedicated
/// top-level instruction; user/assistant turns become `user`/`model`
/// `contents` entries in order.
fn split_system(messages: &[Message]) -> (Option<String>, Vec<Content<'_>>) {
    let mut system: Option<String> = None;
    let mut contents = Vec::with_capacity(messages.len());
    for m in messages {
        match m.role {
            Role::System => {
                let acc = system.get_or_insert_with(String::new);
                if !acc.is_empty() {
                    acc.push('\n');
                }
                acc.push_str(&m.content);
            }
            Role::User => contents.push(Content {
                role: "user",
                parts: [Part { text: &m.content }],
            }),
            Role::Assistant => contents.push(Content {
                role: "model",
                parts: [Part { text: &m.content }],
            }),
        }
    }
    (system, contents)
}

/// Map a Gemini `finishReason` onto the neutral [`FinishReason`]. The
/// safety/recitation family is a provider-side truncation, distinct from a
/// clean `STOP` or a `MAX_TOKENS` length cap.
fn map_finish_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("STOP") => FinishReason::Stop,
        Some("MAX_TOKENS") => FinishReason::Length,
        Some("SAFETY")
        | Some("RECITATION")
        | Some("BLOCKLIST")
        | Some("PROHIBITED_CONTENT")
        | Some("SPII") => FinishReason::Truncated,
        Some(_) | None => FinishReason::Other,
    }
}

impl GeminiProvider {
    /// Stable identifier for this adapter.
    pub fn id(&self) -> ProviderId {
        ProviderId::Gemini
    }

    /// Static capability advertisement.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Run a completion and return the full response.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::ContextOverflow`] pre-dispatch when the
    /// requested `max_tokens` exceeds [`Capabilities::max_context`];
    /// [`ProviderError::AuthInvalid`] on 401/403; [`ProviderError::RateLimited`]
    /// on 429; [`ProviderError::TransportFailed`] on transport failure or 5xx;
    /// and [`ProviderError::ProviderSpecific`] for other non-success
    /// responses.
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

        let (system, contents) = split_system(&request.messages);
        let body = GenRequest {
            contents,
            system_instruction: system.as_deref().map(|s| SystemInstruction {
                parts: [Part { text: s }],
            }),
            generation_config: GenerationConfig {
                max_output_tokens: max_tokens,
                temperature: request.temperature,
            },
        };

        let url = format!(
            "{}/{}/models/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            API_VERSION,
            request.model
        );
        let resp = self
            .client
            .post(&url)
            .header(API_KEY_HEADER, self.api_key.expose())
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
                        .map_or(Duration::from_millis(500), |h| h.default_cooloff)
                });
            return Err(ProviderError::RateLimited {
                retry_after,
                provider: ProviderId::Gemini,
            });
        }

        if status.is_server_error() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransportFailed(TransportError::Io(format!(
                "{status}: {body}"
            ))));
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&body) {
                let code = if env.error.status.is_empty() {
                    status.as_u16().to_string()
                } else {
                    env.error.status
                };
                return Err(ProviderError::ProviderSpecific {
                    provider: ProviderId::Gemini,
                    code,
                    message: env.error.message,
                });
            }
            return Err(ProviderError::ProviderSpecific {
                provider: ProviderId::Gemini,
                code: status.as_u16().to_string(),
                message: body,
            });
        }

        let parsed: GenResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::TransportFailed(TransportError::Decode(e.to_string())))?;

        let candidate = parsed.candidates.into_iter().next();
        let finish_reason =
            map_finish_reason(candidate.as_ref().and_then(|c| c.finish_reason.as_deref()));
        let content = candidate
            .and_then(|c| c.content)
            .map(|c| {
                c.parts
                    .into_iter()
                    .map(|p| p.text)
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        let usage = parsed
            .usage_metadata
            .map(|u| Usage {
                prompt_tokens: u.prompt_token_count,
                completion_tokens: u.candidates_token_count,
            })
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            finish_reason,
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
        let (sys, contents) = split_system(&msgs);
        assert_eq!(sys.as_deref(), Some("a\nb"));
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
    }

    #[test]
    fn split_system_maps_assistant_to_model_role() {
        let msgs = vec![Message::user("hi"), Message::assistant("hello")];
        let (sys, contents) = split_system(&msgs);
        assert!(sys.is_none());
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role, "user");
        // Gemini names the assistant turn `model`, not `assistant`.
        assert_eq!(contents[1].role, "model");
    }

    #[test]
    fn finish_reason_mapping() {
        assert_eq!(map_finish_reason(Some("STOP")), FinishReason::Stop);
        assert_eq!(map_finish_reason(Some("MAX_TOKENS")), FinishReason::Length);
        assert_eq!(map_finish_reason(Some("SAFETY")), FinishReason::Truncated);
        assert_eq!(
            map_finish_reason(Some("RECITATION")),
            FinishReason::Truncated
        );
        assert_eq!(map_finish_reason(Some("OTHER")), FinishReason::Other);
        assert_eq!(map_finish_reason(None), FinishReason::Other);
    }

    #[test]
    fn context_overflow_is_caught_before_dispatch() {
        // Use an invalid base URL; if dispatch occurred we'd get a transport
        // error, but the overflow check runs first.
        let p = GeminiProvider::with_base_url("k", "http://127.0.0.1:1");
        let mut req = CompletionRequest::new("gemini-2.0-flash", "hi");
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
        let p = GeminiProvider::new("k");
        assert_eq!(p.id(), ProviderId::Gemini);
        assert!(p.capabilities().supports_tools);
        assert!(p.capabilities().supports_vision);
    }

    #[test]
    fn debug_format_redacts_api_key() {
        // Structural guarantee: `api_key` is `Secret<String>`, so any `{:?}`
        // splatter of the provider prints `<redacted>` rather than the token
        // (the 2026-07-10 tracing-debug leak class). Reverting the field to a
        // bare `String` reddens this test.
        let p = GeminiProvider::new("sk-gemini-very-secret");
        let formatted = format!("{p:?}");
        assert!(
            !formatted.contains("sk-gemini-very-secret"),
            "Debug must not contain the api key; got: {formatted}"
        );
        assert!(
            formatted.contains("redacted"),
            "Debug should mark api_key as redacted; got: {formatted}"
        );
    }

    #[test]
    fn error_envelope_parses_status_field() {
        let body =
            r#"{"error":{"code":429,"message":"Quota exceeded.","status":"RESOURCE_EXHAUSTED"}}"#;
        let env: ApiErrorEnvelope = serde_json::from_str(body).expect("parse");
        assert_eq!(env.error.status, "RESOURCE_EXHAUSTED");
        assert_eq!(env.error.message, "Quota exceeded.");
    }

    #[test]
    fn response_envelope_joins_parts_and_reads_usage() {
        let body = r#"{
            "candidates":[{"content":{"role":"model","parts":[{"text":"Hello "},{"text":"world"}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}
        }"#;
        let parsed: GenResponse = serde_json::from_str(body).expect("parse");
        let candidate = parsed.candidates.into_iter().next().expect("candidate");
        assert_eq!(
            map_finish_reason(candidate.finish_reason.as_deref()),
            FinishReason::Stop
        );
        let text: String = candidate
            .content
            .expect("content")
            .parts
            .into_iter()
            .map(|p| p.text)
            .collect();
        assert_eq!(text, "Hello world");
        let usage = parsed.usage_metadata.expect("usage");
        assert_eq!(usage.prompt_token_count, 7);
        assert_eq!(usage.candidates_token_count, 3);
    }
}
