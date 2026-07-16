// SPDX-License-Identifier: AGPL-3.0-only

//! Local Ollama HTTP adapter.
//!
//! Ollama runs a daemon on `http://localhost:11434` that exposes
//! `POST /api/chat` with a superset of the OpenAI chat shape. This adapter
//! is the cheapest way to exercise cosmon against deliberately weaker models
//! (Llama 3.2 3B, Mistral Nemo 12B, etc.), which is the experiment called
//! for by ADR-043: does cosmon's scaffolding amplify a faillible cognition?
//!
//! Ollama has no auth and no rate limits; the error surface collapses to
//! transport failures plus provider-specific HTTP errors.

use serde::{Deserialize, Serialize};

use crate::capabilities::Capabilities;
use crate::error::{ProviderError, TransportError};
use crate::provider::ProviderId;
use crate::request::{
    CompletionRequest, CompletionResponse, FinishReason, GrammarFormat, Role, Usage,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Ollama local-daemon adapter.
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    capabilities: Capabilities,
    /// How long Ollama holds the model resident in VRAM after a request,
    /// as an Ollama duration string (`"5m"`, `"30m"`, `"-1"` = forever,
    /// `"0"` = unload immediately). `None` leaves the daemon default
    /// (5 min). See [`OllamaProvider::with_keep_alive`].
    keep_alive: Option<String>,
}

impl OllamaProvider {
    /// Adapter pointing at the default local daemon.
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Adapter pointing at a custom base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            capabilities: Capabilities {
                // Ollama's default context is model-dependent and negotiable
                // via `options.num_ctx`. Advertise a conservative ceiling.
                max_context: 32_768,
                supports_streaming: false,
                supports_tools: false,
                supports_vision: false,
                rate_limit_hint: None,
            },
            keep_alive: None,
        }
    }

    /// Pin how long Ollama keeps this model resident in VRAM after each
    /// request (the provider half of model-affinity batching, C3 of
    /// `delib-20260705-7288`).
    ///
    /// On a single-GPU oracle (`ollama-g5`: 48 GB ≈ one 120 B model), the
    /// scheduler drains same-model molecules contiguously
    /// (`cosmon_graph::affinity_order`); a widened
    /// `keep_alive` guarantees the resident model survives the gap between
    /// two same-model dispatches instead of being evicted at the 5-minute
    /// default and reloaded (~40 GB off disk). `value` is an Ollama
    /// duration string — `"30m"`, `"-1"` (never unload), `"0"` (unload
    /// now). An empty string is treated as "leave the daemon default".
    #[must_use]
    pub fn with_keep_alive(mut self, value: impl Into<String>) -> Self {
        let v = value.into();
        self.keep_alive = if v.is_empty() { None } else { Some(v) };
        self
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ApiOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    messages: Vec<ApiMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<ApiOptions>,
    /// Ollama `keep_alive` — how long to hold the model in VRAM after
    /// this request. Omitted (daemon default) when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<&'a str>,
    /// Ollama `format` — grammar-constrained decoding (C4 mechanism 1).
    /// Either the string `"json"` or a JSON Schema object; ollama constrains
    /// token sampling so the output cannot violate it. Omitted (unconstrained)
    /// when `None`, keeping the request byte-identical to the pre-C4 shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<serde_json::Value>,
}

/// Translate a provider-neutral [`GrammarFormat`] into ollama's native
/// `format` value.
///
/// - [`GrammarFormat::Json`] → the JSON string `"json"`.
/// - [`GrammarFormat::JsonSchema`] → the parsed schema object. If the carried
///   schema text does not parse as JSON it is passed through as a JSON string
///   so ollama surfaces its own error rather than this adapter silently
///   dropping the constraint (fail-loud, not fail-silent).
fn ollama_format(format: &GrammarFormat) -> serde_json::Value {
    match format {
        GrammarFormat::Json => serde_json::Value::String("json".to_owned()),
        GrammarFormat::JsonSchema(schema) => serde_json::from_str(schema)
            .unwrap_or_else(|_| serde_json::Value::String(schema.clone())),
    }
}

#[derive(Deserialize)]
struct ApiResponseMessage {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    message: ApiResponseMessage,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

fn role_tag(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn map_done_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some(_) => FinishReason::Other,
        None => FinishReason::Stop,
    }
}

impl OllamaProvider {
    /// Stable identifier for this adapter.
    pub fn id(&self) -> ProviderId {
        ProviderId::Ollama
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
        if let Some(requested) = request.max_tokens {
            if !self.capabilities.can_fit(requested) {
                return Err(ProviderError::ContextOverflow {
                    max_tokens: self.capabilities.max_context,
                    requested,
                });
            }
        }

        let messages: Vec<ApiMessage<'_>> = request
            .messages
            .iter()
            .map(|m| ApiMessage {
                role: role_tag(m.role),
                content: &m.content,
            })
            .collect();

        let options = if request.max_tokens.is_some() || request.temperature.is_some() {
            Some(ApiOptions {
                num_predict: request.max_tokens,
                temperature: request.temperature,
            })
        } else {
            None
        };

        let body = ApiRequest {
            model: &request.model,
            messages,
            stream: false,
            options,
            keep_alive: self.keep_alive.as_deref(),
            format: request.format.as_ref().map(ollama_format),
        };

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::TransportFailed(TransportError::Io(e.to_string())))?;

        let status = resp.status();
        if status.is_server_error() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransportFailed(TransportError::Io(format!(
                "{}: {}",
                status, body
            ))));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ProviderSpecific {
                provider: ProviderId::Ollama,
                code: status.as_u16().to_string(),
                message: body,
            });
        }

        let parsed: ApiResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::TransportFailed(TransportError::Decode(e.to_string())))?;

        Ok(CompletionResponse {
            content: parsed.message.content,
            finish_reason: map_done_reason(parsed.done_reason.as_deref()),
            usage: Usage {
                prompt_tokens: parsed.prompt_eval_count.unwrap_or(0),
                completion_tokens: parsed.eval_count.unwrap_or(0),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_tag_matches_ollama_vocabulary() {
        assert_eq!(role_tag(Role::System), "system");
        assert_eq!(role_tag(Role::User), "user");
        assert_eq!(role_tag(Role::Assistant), "assistant");
    }

    #[test]
    fn map_done_reason_defaults_to_stop() {
        assert_eq!(map_done_reason(None), FinishReason::Stop);
        assert_eq!(map_done_reason(Some("stop")), FinishReason::Stop);
        assert_eq!(map_done_reason(Some("length")), FinishReason::Length);
        assert_eq!(map_done_reason(Some("weird")), FinishReason::Other);
    }

    #[test]
    fn id_is_ollama() {
        let p = OllamaProvider::new();
        assert_eq!(p.id(), ProviderId::Ollama);
        assert!(!p.capabilities().supports_tools);
    }

    #[test]
    fn keep_alive_defaults_to_none_and_is_omitted() {
        let p = OllamaProvider::new();
        assert!(p.keep_alive.is_none());
        let body = ApiRequest {
            model: "gpt-oss:120b",
            messages: vec![],
            stream: false,
            options: None,
            keep_alive: p.keep_alive.as_deref(),
            format: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(
            !json.contains("keep_alive"),
            "unset keep_alive must be omitted (daemon default), got {json}"
        );
    }

    #[test]
    fn with_keep_alive_sets_and_serializes() {
        let p = OllamaProvider::new().with_keep_alive("30m");
        assert_eq!(p.keep_alive.as_deref(), Some("30m"));
        let body = ApiRequest {
            model: "gpt-oss:120b",
            messages: vec![],
            stream: false,
            options: None,
            keep_alive: p.keep_alive.as_deref(),
            format: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"keep_alive\":\"30m\""), "got {json}");
    }

    #[test]
    fn with_keep_alive_empty_string_is_treated_as_unset() {
        let p = OllamaProvider::new().with_keep_alive("");
        assert!(
            p.keep_alive.is_none(),
            "empty string means leave the daemon default, not send an empty value"
        );
    }

    #[test]
    fn ollama_format_json_is_the_string_json() {
        assert_eq!(
            ollama_format(&GrammarFormat::Json),
            serde_json::Value::String("json".to_owned())
        );
    }

    #[test]
    fn ollama_format_schema_is_parsed_object() {
        let schema = r#"{"type":"object","required":["verdict"]}"#;
        let v = ollama_format(&GrammarFormat::JsonSchema(schema.to_owned()));
        assert!(v.is_object(), "schema must become a JSON object, got {v}");
        assert_eq!(v["type"], "object");
    }

    #[test]
    fn ollama_format_bad_schema_falls_back_to_string_not_dropped() {
        // A non-JSON schema must NOT be silently dropped — pass it through so
        // ollama surfaces the error (fail-loud, mechanism-1 integrity).
        let v = ollama_format(&GrammarFormat::JsonSchema("not json".to_owned()));
        assert_eq!(v, serde_json::Value::String("not json".to_owned()));
    }

    #[test]
    fn request_format_none_omits_format_field() {
        let p = OllamaProvider::new();
        let body = ApiRequest {
            model: "gpt-oss:120b",
            messages: vec![],
            stream: false,
            options: None,
            keep_alive: p.keep_alive.as_deref(),
            format: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(
            !json.contains("format"),
            "unset format must be omitted (unconstrained), got {json}"
        );
    }

    #[test]
    fn request_format_schema_serializes_into_body() {
        let req = CompletionRequest::new("gpt-oss:120b", "emit a verdict").with_format(
            GrammarFormat::JsonSchema(r#"{"type":"object","required":["verdict"]}"#.to_owned()),
        );
        let body = ApiRequest {
            model: &req.model,
            messages: vec![],
            stream: false,
            options: None,
            keep_alive: None,
            format: req.format.as_ref().map(ollama_format),
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(
            json.contains("\"format\":{") && json.contains("\"required\":[\"verdict\"]"),
            "schema must be embedded as an object under `format`, got {json}"
        );
    }

    #[tokio::test]
    async fn unreachable_daemon_is_transport_failed() {
        let p = OllamaProvider::with_base_url("http://127.0.0.1:1");
        let err = p
            .complete(CompletionRequest::new("llama3", "hi"))
            .await
            .expect_err("should fail");
        assert!(matches!(
            err,
            ProviderError::TransportFailed(TransportError::Io(_))
        ));
    }
}
