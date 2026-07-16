// SPDX-License-Identifier: AGPL-3.0-only

//! Per-provider message-log abstraction — the trait that absorbs
//! invariant I4 (message-log well-formedness).
//!
//! ADR-102 §C5 / §S1: the only provider-shaped loop invariant is I4
//! — every assistant message that carries tool_calls is immediately
//! followed by the matching tool-result entries, in OpenAI's
//! `role:"tool"` shape or Anthropic's `tool_result` content blocks.
//! These envelopes are isomorphic at the information level and
//! **non-isomorphic at the type level** (knuth §8). A shared
//! concrete `Vec<Message>` would force one of the two providers to
//! lose information at the type boundary.
//!
//! [`MessageLog`] is the trait every provider's adapter implements
//! to carry I4 through its native envelope. The spine treats the
//! log as opaque — it appends an assistant message and tool results
//! through the trait methods, and asks for an estimated-token count
//! when needed; it never reaches into the underlying `Vec`.
//!
//! ## What the spine asks of every impl
//!
//! - [`MessageLog::from_briefing`] — construct the initial log from
//!   the operator's briefing string. The provider's impl is free to
//!   prepend its own system prompt (OpenAI puts it inside `messages`
//!   as `role:"system"`; Anthropic puts it at the top-level `system`
//!   field of the wire envelope).
//! - [`MessageLog::append_assistant`] — append the assistant message
//!   that emitted tool_calls, *before* the tool results land. This is
//!   the I4-preserving discipline: the v0 OpenAI impl pushes
//!   `messages.push(choice.message)` *then* iterates the calls.
//! - [`MessageLog::append_tool_result`] — append one tool result
//!   paired with the originating call's `id` and the tool's `name`,
//!   in the provider's native envelope.
//! - [`MessageLog::estimate_tokens`] — used by future I3 enforcement
//!   inside the loop (currently the pre-flight check uses the raw
//!   briefing length; this method is on the trait so PR-A.5 can
//!   tighten the enforcement without changing the trait).
//! - [`MessageLog::invariant_well_formed`] — asserted via
//!   `debug_assert!` at the loop head. A breach is a logic bug in
//!   the provider's impl, not a runtime failure.
//! - [`MessageLog::compact`] — escape valve for I3. The spine calls
//!   this when the log crosses
//!   [`crate::compaction::CompactionPolicy::threshold_tokens`]; the
//!   per-provider impl preserves the seed + last N messages + an
//!   inline `[compaction summary]` user message in their native
//!   envelope. A default no-op impl is provided so a `MessageLog` that
//!   is known short-lived can opt out without boilerplate.

use crate::compaction::{CompactionError, CompactionPolicy, CompactionReport};

/// Provider-agnostic speaker label for a single [`TranscriptEntry`].
///
/// The per-provider envelopes disagree on how a tool result is encoded
/// — OpenAI uses a dedicated `role:"tool"` message, Anthropic folds it
/// into a `role:"user"` content block, llama feeds it back as plain
/// user text (knuth §8 — *isomorphic at the information level,
/// non-isomorphic at the type level*). [`TranscriptRole`] is the
/// flattened, render-ready projection of those three shapes onto the
/// four roles an interactive driver needs to *display*. It is
/// deliberately lossy: it is for a human reading the scrollback, not
/// for re-priming the model (which still goes through the opaque
/// [`MessageLog::AssistantMsg`] envelope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TranscriptRole {
    /// System framing (persona, constraints, tool advertisement). Some
    /// providers carry this inside the message array (OpenAI, llama),
    /// others in a dedicated `system` field (Anthropic) — the
    /// [`MessageLog::transcript`] impl normalises both into this role.
    System,
    /// An operator (human) turn — the briefing seed and every line the
    /// operator types at the `❯` prompt via
    /// [`MessageLog::append_user`]. Maps to the wire `user` role.
    Operator,
    /// A model turn. Tool-call envelopes that carry no display text are
    /// rendered with a synthetic `(called <tool>…)` placeholder so the
    /// scrollback still shows that *something* happened that turn.
    Assistant,
    /// A tool result fed back to the model. Regardless of the wire
    /// shape (OpenAI `role:"tool"`, Anthropic `tool_result` block,
    /// llama user-text), it surfaces here so the driver can fold or
    /// dim it in the rendered history.
    Tool,
}

/// One flattened, render-ready entry in a [`MessageLog::transcript`].
///
/// This is the *read* counterpart to the write-only trio
/// ([`MessageLog::from_briefing`] / [`MessageLog::append_assistant`] /
/// [`MessageLog::append_tool_result`] / [`MessageLog::append_user`]).
/// Before the interactive `step()` path (ADR-115) the log was strictly
/// write-only: the spine appended and the provider serialized, but no
/// caller could read history back. An interactive driver needs to
/// render the scrollback after each operator turn, so the trait grows
/// this projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// Who spoke, normalised across provider envelopes.
    pub role: TranscriptRole,
    /// Display text. For tool-call assistant turns with no text this is
    /// a synthetic placeholder; for tool results it is the (fenced)
    /// result content. Never the raw provider envelope — that stays
    /// behind [`MessageLog::AssistantMsg`].
    pub content: String,
}

impl TranscriptEntry {
    /// Construct a transcript entry. Convenience for the per-provider
    /// [`MessageLog::transcript`] impls.
    #[must_use]
    pub fn new(role: TranscriptRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

/// Per-provider message-log abstraction (I4 carrier).
///
/// See the module-level docs for the full contract. `Send + Sync` is
/// required so the spine can drive an `async` provider without
/// auto-trait surprises.
pub trait MessageLog: Sized + Send + Sync {
    /// The provider's assistant-message envelope. Opaque to the
    /// spine; passed through [`crate::spine::Turn::ToolCalls`] and
    /// re-injected via [`Self::append_assistant`].
    type AssistantMsg: Send + Sync;

    /// Construct an initial log from the operator's briefing.
    /// Per-provider impls are responsible for any system-prompt
    /// framing required to satisfy I4 from the first turn.
    fn from_briefing(briefing: &str) -> Self;

    /// Append an assistant message that may carry tool_calls. Must
    /// be called **before** any [`Self::append_tool_result`] for
    /// the same turn, per I4.
    fn append_assistant(&mut self, msg: Self::AssistantMsg);

    /// Append one tool result paired with the originating call's
    /// `id` and the tool's `name`. The provider's impl is
    /// responsible for translating the spine's
    /// `(id, name, content)` triple into its native envelope.
    fn append_tool_result(&mut self, call_id: &str, tool_name: &str, content: &str);

    /// Append an operator (human) turn as a `user`-role message.
    ///
    /// This is the write-side primitive the interactive `step()` path
    /// (ADR-115) needs and the one-shot worker path never uses: the
    /// worker receives a single immutable briefing at construction and
    /// never hears from a human again, but an interactive session loops
    /// back to the operator after every model `Turn::Stop` and must
    /// fold the next typed line into the log before the next
    /// round-trip. Without it, the operator could speak exactly once
    /// (the briefing) and never again — which is the one-shot worker,
    /// not a REPL.
    ///
    /// I4 is unaffected: a `user` turn carries no tool calls, so it can
    /// land anywhere a fresh turn is legal (i.e. when the log is not
    /// mid-tool-pair). Per-provider impls translate `content` into
    /// their native `user`/`Operator` envelope, exactly as
    /// [`Self::from_briefing`] does for the seed turn.
    fn append_user(&mut self, content: &str);

    /// Flatten the current log into a provider-agnostic, render-ready
    /// [`TranscriptEntry`] list — the *read* accessor an interactive
    /// driver uses to paint the scrollback.
    ///
    /// Before the interactive path the log was strictly write-only
    /// (append + serialize, no read-back). This method is deliberately
    /// lossy: it projects the per-provider envelope onto the four
    /// display roles in [`TranscriptRole`] and drops structural fields
    /// (tool-call ids, JSON argument blobs) the model needs but a human
    /// reading scrollback does not. Re-priming the model still flows
    /// through the opaque [`Self::AssistantMsg`] envelope — this is for
    /// rendering only.
    ///
    /// Ordering matches the underlying message array, so the returned
    /// vector reads chronologically (seed first, latest turn last).
    fn transcript(&self) -> Vec<TranscriptEntry>;

    /// Estimated input-token count for the *current* state of the
    /// log — used by I3 enforcement. The v0 implementations are
    /// expected to use the 4-chars-per-token heuristic; a real
    /// tokenizer is a PR-A.5 concern (knuth §10 — *empirically
    /// calibrated, not theoretically sound*).
    fn estimate_tokens(&self) -> u32;

    /// Self-check for I4 — every assistant message with tool_calls
    /// is followed by matching tool-result entries. Asserted via
    /// `debug_assert!` at the loop head; release builds skip the
    /// check.
    fn invariant_well_formed(&self) -> bool;

    /// Compact this log to approximately `target_tokens`, preserving
    /// the seed message and the most recent `policy.preserve_recent`
    /// messages, replacing the middle with an inline
    /// `[compaction summary]` user-role message. The per-provider impl
    /// is responsible for keeping I4 well-formed across the boundary
    /// (extending the preserved tail to keep tool-call / tool-result
    /// pairs together).
    ///
    /// # Errors
    ///
    /// - [`CompactionError::NotApplicable`] when the log is already
    ///   below the target or the preserved tail already covers the
    ///   whole log. Informational; the spine continues.
    /// - [`CompactionError::WouldBreakInvariant`] when an in-flight
    ///   tool_use lacks its matching tool_result. Informational; the
    ///   spine continues, the next iteration may re-attempt.
    ///
    /// # Default implementation
    ///
    /// The default returns
    /// [`CompactionError::NotApplicable`] so an exploratory
    /// `MessageLog` impl (e.g. the in-crate `TestLog` used by the
    /// spine unit tests) can opt out without explicitly overriding
    /// the method. Production impls (OpenAILog, AnthropicLog,
    /// LlamaLog) override this with a real compactor.
    fn compact(
        &mut self,
        target_tokens: u32,
        policy: CompactionPolicy,
    ) -> Result<CompactionReport, CompactionError> {
        let _ = (target_tokens, policy);
        Err(CompactionError::NotApplicable)
    }
}
