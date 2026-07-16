// SPDX-License-Identifier: AGPL-3.0-only

//! Provider-neutral request and response types.
//!
//! These types are the lingua franca between cosmon callers and the concrete
//! adapter. They are deliberately minimal — chat messages, token limits,
//! temperature — and defer provider-specific extras (reasoning budgets,
//! cache controls, tool schemas) to later iterations. This keeps the v0
//! public surface small enough to stabilise without blocking experiments.

use serde::{Deserialize, Serialize};

/// Who authored a given [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System-level instructions (persona, constraints).
    System,
    /// Human or upstream-cosmon input.
    User,
    /// Model output.
    Assistant,
}

/// A grammar constraint on the model's decoding — mechanism 1 of the
/// weak-oracle oracle-boundary hardening (C4, `delib-20260705-7288`).
///
/// Attaching a `GrammarFormat` to a [`CompletionRequest`] asks the backend to
/// **constrain decoding** so that the sampled tokens are guaranteed to satisfy
/// the format. This makes a malformed structured output *impossible* rather
/// than merely unlikely — the highest-leverage hardening for a high-variance
/// local oracle, because it attacks the first-to-break failure (malformed
/// JSON) at its root instead of repairing it after the fact.
///
/// Only backends that support constrained decoding honour it; today that is
/// [`crate::ollama`] via ollama's native `format` field (a bare `"json"` or a
/// full JSON Schema object). Backends that cannot constrain decoding ignore
/// the field — the request degrades to unconstrained generation, never errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarFormat {
    /// Free-form but guaranteed *syntactically valid* JSON. Maps to ollama's
    /// `format: "json"`. Use when the shape is described in the prompt but no
    /// machine schema is available.
    Json,
    /// A full JSON Schema, carried as its serialised JSON text. Maps to
    /// ollama's `format: <schema-object>`, which constrains decoding to the
    /// schema — the strongest guarantee. The string is the schema document,
    /// e.g. `{"type":"object","properties":{...},"required":[...]}`.
    JsonSchema(String),
}

/// A single turn in the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Speaker identity.
    pub role: Role,
    /// Opaque UTF-8 content. v0 does not model multimodal parts.
    pub content: String,
}

impl Message {
    /// Construct a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
    /// Construct a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    /// Construct an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// A completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Provider-native model identifier (e.g. `claude-sonnet-4-6`,
    /// `llama3.2:3b`). The caller owns the mapping; the provider does not
    /// translate between aliases.
    pub model: String,
    /// Ordered conversation prefix.
    pub messages: Vec<Message>,
    /// Hard cap on generated tokens. `None` means "use the provider's
    /// default"; adapters that need a number will clamp against
    /// [`crate::Capabilities::max_context`].
    pub max_tokens: Option<u32>,
    /// Sampling temperature in `[0.0, 2.0]`. `None` means "use the
    /// provider default".
    pub temperature: Option<f32>,
    /// Optional grammar constraint on decoding (mechanism 1 of the C4
    /// oracle-boundary hardening). `None` (the default) leaves decoding
    /// unconstrained — byte-identical to the pre-C4 request shape, so this
    /// addition is backward-compatible on the wire (`skip_serializing_if`)
    /// and for existing callers ([`CompletionRequest::new`] sets `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<GrammarFormat>,
}

impl CompletionRequest {
    /// Build a minimal request with just a model and a single user prompt.
    pub fn new(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: vec![Message::user(prompt)],
            max_tokens: None,
            temperature: None,
            format: None,
        }
    }

    /// Attach a grammar constraint on decoding (builder style).
    ///
    /// On a backend that supports constrained decoding ([`crate::ollama`]) this
    /// makes malformed structured output impossible; on any other backend it is
    /// ignored. Chainable after [`CompletionRequest::new`].
    #[must_use]
    pub fn with_format(mut self, format: GrammarFormat) -> Self {
        self.format = Some(format);
        self
    }
}

/// Why the model stopped emitting tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model decided it was done.
    Stop,
    /// `max_tokens` was reached.
    Length,
    /// The provider truncated for a non-length reason (moderation, etc.).
    Truncated,
    /// The model emitted a tool call that a future iteration will surface.
    ToolCall,
    /// Reason unknown or not reported.
    Other,
}

/// Token accounting reported by the provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed from the prompt side.
    pub prompt_tokens: u32,
    /// Tokens emitted by the model.
    pub completion_tokens: u32,
}

impl Usage {
    /// Total tokens billed for this call.
    pub fn total(&self) -> u32 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

/// The result of a successful completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Full assistant text.
    pub content: String,
    /// Why the provider stopped.
    pub finish_reason: FinishReason,
    /// Token accounting. Zero if the provider does not report usage.
    pub usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_request_is_single_user_message() {
        let req = CompletionRequest::new("claude-sonnet-4-6", "hi");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
    }

    #[test]
    fn usage_total_saturates() {
        let u = Usage {
            prompt_tokens: u32::MAX,
            completion_tokens: 10,
        };
        assert_eq!(u.total(), u32::MAX);
    }

    #[test]
    fn request_roundtrips_through_json() {
        let req = CompletionRequest {
            model: "m".into(),
            messages: vec![Message::system("s"), Message::user("u")],
            max_tokens: Some(128),
            temperature: Some(0.7),
            format: None,
        };
        let s = serde_json::to_string(&req).expect("serialise");
        let back: CompletionRequest = serde_json::from_str(&s).expect("deserialise");
        assert_eq!(req, back);
    }

    #[test]
    fn unset_format_is_omitted_on_the_wire() {
        let req = CompletionRequest::new("m", "hi");
        let s = serde_json::to_string(&req).expect("serialise");
        assert!(
            !s.contains("format"),
            "unset format must be omitted (backward-compatible), got {s}"
        );
    }

    #[test]
    fn format_roundtrips_json_and_schema() {
        for fmt in [
            GrammarFormat::Json,
            GrammarFormat::JsonSchema("{\"type\":\"object\"}".into()),
        ] {
            let req = CompletionRequest::new("m", "hi").with_format(fmt.clone());
            let s = serde_json::to_string(&req).expect("serialise");
            let back: CompletionRequest = serde_json::from_str(&s).expect("deserialise");
            assert_eq!(back.format, Some(fmt));
        }
    }

    proptest::proptest! {
        #[test]
        fn request_roundtrip(model in "[a-z0-9\\-:]{1,32}", prompt in ".{0,256}") {
            let req = CompletionRequest::new(model, prompt);
            let s = serde_json::to_string(&req).unwrap();
            let back: CompletionRequest = serde_json::from_str(&s).unwrap();
            assert_eq!(req, back);
        }
    }
}
