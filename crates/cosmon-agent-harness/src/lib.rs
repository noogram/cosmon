// SPDX-License-Identifier: AGPL-3.0-only

//! Provider-agnostic agent-loop spine for cosmon — the realisation of
//! the **`AgentLoop` port** named in
//! [ADR-102](../../docs/adr/102-cosmon-agent-harness-and-agentloop-port.md).
//!
//! # The four-word closure
//!
//! ADR-102 §1 commits **{ Loop · Tool · Turn · Schema }** as the
//! load-bearing vocabulary for the agent-harness perimeter. The first
//! three live in this crate; *Schema* is per-provider and lives in
//! `cosmon-provider::{openai,anthropic}::*` so the OpenAI envelope's
//! `role:"tool"` shape never sediments into the spine and break
//! invariant I4 the day a third schema (Gemini, Mistral) lands.
//!
//! # What lives here vs. per-provider
//!
//! Extracted into this crate (ADR-102 §D-3 *"draw the spine NOW, but
//! only the spine"*):
//!
//! - The eight-state FSM as control flow over a single `loop {}` in
//!   [`spine::run_loop`] (knuth §2–§7 — the typestate-encoded
//!   `Harness<S>` is sequenced as PR-A.5, deliberately deferred).
//! - The [`tool::Tool`] trait + `BTreeMap`-backed
//!   [`tool::ToolRegistry`] (S5 — stable iteration order from day one
//!   is the prerequisite for the future prompt-cache prefix-stability
//!   work, NOT a commitment to ship prompt caching).
//! - The [`spine::Provider`] trait carrying the I4 obligation through
//!   its [`message_log::MessageLog`] associated type.
//! - The four loop invariants `{ I1 turn-bounded, I2 tool-bounded,
//!   I3 context-bounded, I4 message-log well-formedness }` (knuth §5,
//!   named for the record in [`invariants`]).
//!
//! NOT extracted (per-provider, duplicated until the second example
//! proves the abstraction — same IFBDD discipline as ADR-100):
//!
//! - `ChatRequest`/`Messages` Serde types (per-provider HTTP envelope).
//! - HTTP wire serialization functions.
//! - A normalized `ToolCall` / `ToolResult` wire envelope. The shared
//!   [`tool::ToolCall`] in this crate is the spine's *internal*
//!   representation — providers translate their native envelope into
//!   it inside their own `Provider::one_turn` impl.
//!
//! # API shape
//!
//! See [`spine::run_loop`] for the entry point and [`spine::Provider`]
//! for the two-method trait. Per-provider impls construct a
//! [`message_log::MessageLog`] from the briefing and translate
//! HTTP responses into [`spine::Turn`] variants.
//!
//! # PR-A vs PR-A.5
//!
//! ADR-102 §D-6 sequences the implementation in two steps:
//!
//! - **PR-A (this crate, v0):** minimal spine. Eight states live in
//!   control flow, four invariants named in `invariants.rs`, two
//!   methods on `Provider`.
//! - **PR-A.5 (v0.5, separate bead — operator-filed):** typestate
//!   promotion to `Harness<S: HarnessState>` with the twelve transitions
//!   as typestate methods. Not nucleated by this PR; named here to
//!   keep the target in institutional memory.

// Crate-level discipline: `deny(unsafe_code)` rather than `forbid`
// (relaxed from `forbid` 2026-05-19 / delib-20260519-e6db W3 /
// adversary F1.4). The harness needs `setsid(2)` + `kill(-pgid)` in
// `tools::exec_command::ExecSession::spawn` and
// `kill_group_and_reap` so a 1-second timeout on `cargo build`
// reaps the rustc worker grandchildren. Two #[allow(unsafe_code)]
// scopes are documented at the call sites; the rest of the crate
// remains unsafe-free.
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    // Proper nouns like OpenAI / Anthropic / BTreeMap appear in
    // prose throughout the module headers; backticking each
    // occurrence would hurt readability without adding signal.
    clippy::doc_markdown,
    // `cosmon_agent_harness::tool::Tool` is intentional — the
    // module names the concept; "Tool" alone is the concept's name.
    clippy::module_name_repetitions
)]

pub mod bootstrap;
pub mod budget;
pub mod compaction;
pub mod egress_probe;
pub mod error;
pub mod invariants;
pub mod message_log;
pub mod spine;
pub mod tool;
pub mod tools;

pub use budget::{ContextBudget, ToolBudget, TurnBudget};
pub use compaction::{
    build_summary_body, CompactionError, CompactionPolicy, CompactionReport,
    COMPACTION_SUMMARY_PREFIX,
};
pub use error::HarnessError;
pub use message_log::{MessageLog, TranscriptEntry, TranscriptRole};
pub use spine::{
    run_loop, run_loop_with_capability, run_loop_with_registry, InteractiveSession,
    ScriptedProviderFn, StepOutcome, Turn,
};
pub use tool::{
    default_registry, default_registry_with_operator_block, local_sandbox_registry,
    ParametersSchema, ReadFile, ReadParams, ReadResult, Tool, ToolCall, ToolDeclaration, ToolError,
    ToolRegistry, READ_FILE_CAP_BYTES,
};
pub use tools::await_operator::AwaitOperator;
pub use tools::edit_file::{EditError, EditFile, EditOp, EditParams, EditResult};
pub use tools::exec_command::{ExecCommand, ExecResult};
// Local-research extension (task-20260521-a095) — re-export the
// concrete tool structs so adapter authors can build a custom
// `ToolRegistry` without going through `tools::*` paths.
pub use tools::find_file::{FindFile, FindResult};
pub use tools::grep::{Grep, GrepMatch, GrepResult};
pub use tools::list_dir::{ListDir, ListEntry, ListResult};
pub use tools::write_file::{WriteFile, WriteResult};
