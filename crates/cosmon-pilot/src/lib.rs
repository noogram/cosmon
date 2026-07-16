// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-pilot` — the interactive **cognitive-pilot driver**: a
//! foreground `read → step → render` loop that lets an operator converse
//! with a local model which can *see* the cosmon fleet through read-only
//! domain tools.
//!
//! ## Role in the architecture
//!
//! This crate is the **driver** half of the cs-pilot walking skeleton
//! (delib `2026-05-31-cs-pilot-external-cognitive-pilot`, §4/§5; ADR-115).
//! It owns the REPL loop and wires together three pieces it does **not**
//! own:
//!
//! - the model round-trip — [`cosmon_agent_harness::InteractiveSession`]'s
//!   `step()` FSM (the ADR-115 interactive refactor of `run_loop`);
//! - the tools the model may call mid-turn —
//!   [`cosmon_ops_tools::read_only_registry`] (`observe` / `peek` /
//!   `ensemble`, all read-only, all calling `cosmon-core` /
//!   `cosmon-state` directly — no `cs` subprocess);
//! - the model itself — [`cosmon_provider::OpenAIProvider`] pointed at the
//!   **local Ollama** OpenAI-compatible endpoint
//!   (`http://localhost:11434/v1`, per the autonomy-local-first doctrine).
//!
//! ## Stateless between runs, save the transcript (ADR-016)
//!
//! There is **no daemon and no persistent process**. The loop is a
//! foreground program; when it exits, nothing of it survives except the
//! on-disk [`transcript`] artifact it appended to. A second `cosmon-pilot`
//! invocation starts a *fresh* [`cosmon_agent_harness::InteractiveSession`]
//! — the transcript is a record, not a resumable context (ADR-096 forbids
//! the claw `Session`-as-context shape; the cosmon term is **transcript**).
//!
//! ## Pilot directives, not slash commands (ADR-096)
//!
//! In-REPL meta-commands that never reach the model — `/help`, `/quit`,
//! `/compact`, `/observe` — are **pilot directives**
//! ([`directives::PilotDirective`]), the cosmon-glossary rename of claw's
//! "slash commands" borrowed as bibliography under ADR-096. A line that is
//! not a directive is folded into the conversation as an operator turn and
//! sent to the model.
//!
//! ## v0 scope
//!
//! No streaming, no permissions UI, no remote, no session resume — all
//! deferred (delib §6/§8). The smallest thing that *runs*: one molecule
//! observed end-to-end via a model tool call against a local model.

#![forbid(unsafe_code)]
#![allow(
    // Proper nouns (OpenAI, Ollama, ADR ids) and claw-code identifiers
    // cited as bibliography appear in prose throughout the docs;
    // backticking each occurrence would hurt readability — same stance
    // as `cosmon-agent-harness` (delib-20260519-e6db W3).
    clippy::doc_markdown,
    // `PilotError` in `error`, `PilotDirective` in `directives` — the
    // module names the concept; the prefixed type IS the concept's name.
    clippy::module_name_repetitions
)]

pub mod directives;
pub mod error;
pub mod repl;
pub mod transcript;

pub use directives::PilotDirective;
pub use error::PilotError;
pub use repl::{run_repl, ReplConfig};
pub use transcript::Transcript;
