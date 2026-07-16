// SPDX-License-Identifier: AGPL-3.0-only

//! Loop invariants for the agent-loop spine — named for the record.
//!
//! ADR-102 §D-3 inscribes four loop invariants that travel with every
//! iteration of [`crate::spine::run_loop`]. Each is preserved by the
//! v0 control-flow implementation; the PR-A.5 typestate promotion
//! converts each invariant into a trait bound on `HarnessState`.
//!
//! The invariants are documented here (not implemented as types) to
//! keep the v0 spine deliberately small. The names are load-bearing —
//! every later deliberation on the harness perimeter cites them by
//! these labels (knuth §5–§7).
//!
//! # I1 — Turn-bounded
//!
//! At any point during [`crate::spine::run_loop`] execution,
//! `0 ≤ turn ≤ K`. The constant `K` is enforced as
//! [`crate::budget::TurnBudget::max_turns`] (default 8 — a loud upper
//! bound on a single `cs tackle`, not a silent rate-limiter). The
//! `for _turn in 0..K` loop in `spine.rs` is the structural enforcement.
//!
//! Loud failure on breach: [`crate::error::HarnessError::TurnBudgetExhausted`].
//!
//! # I2 — Tool-bounded
//!
//! `used_tools` starts at 0 and is monotonically incremented in the
//! `Turn::ToolCalls` arm of the FSM. The explicit ceiling
//! [`crate::budget::ToolBudget::max_tool_calls`] is named in this v0
//! crate but the spine **does not yet enforce it** — the toy
//! `write_file`-only loop cannot exhibit a tool-budget breach with the
//! 8-turn cap. Enforcement lands in PR-A.5 (typestate promotion) and
//! is named pre-emptively so the budget API is stable from day one.
//!
//! # I3 — Context-bounded
//!
//! The estimated input-token count is non-decreasing across turns and
//! is bounded above by [`crate::budget::ContextBudget::max_input_tokens`].
//! The pre-flight check at the head of [`crate::spine::run_loop`]
//! catches SF-5 (context overflow) before the first HTTP dispatch — a
//! loud cap, not silent truncation.
//!
//! Loud failure on breach: [`crate::error::HarnessError::ContextOverflow`].
//!
//! # I4 — Message-log well-formedness
//!
//! Every assistant message that carries `tool_calls` is immediately
//! followed in the per-provider message log by exactly
//! `|tool_calls|` matching tool-result entries (OpenAI's
//! `role:"tool"` messages with `tool_call_id` set; Anthropic's
//! `tool_result` content blocks). **This is the only provider-shaped
//! loop invariant** — and the reason
//! [`crate::message_log::MessageLog`] is a per-provider trait impl
//! rather than a shared concrete type.
//!
//! The spine asserts I4 via [`crate::message_log::MessageLog::invariant_well_formed`]
//! through a `debug_assert!` at the loop head. A breach is a logic
//! bug in the provider's `MessageLog` impl, not a runtime failure;
//! release builds skip the check.
