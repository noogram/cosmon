// SPDX-License-Identifier: AGPL-3.0-only

//! Transport backend adapters for Cosmon.
//!
//! Provides concrete implementations of [`cosmon_core::transport::TransportBackend`]:
//! - [`TmuxBackend`] — spawns agents in tmux sessions
//! - `MockBackend` — in-memory fake for unit tests of higher layers
//!
//! Plus standalone functions for adapter-driven worker sessions,
//! beads CLI, and dispatch:
//! - [`claude`] — spawn/kill/check Claude Code sessions via tmux
//! - [`aider`] — spawn/kill/check Aider (`aider-chat`) sessions via tmux
//! - [`codex`] — spawn/kill/check `OpenAI` `Codex` CLI sessions via tmux,
//!   with a load-bearing version pin
//! - [`opencode`] — spawn/kill/check `opencode` (sst/opencode) CLI sessions
//!   via tmux — the external-CLI sibling of [`codex`] (ADR-125)
//! - [`beads`] — shell out to `bd` for issue tracking
//! - [`dispatch`] — create beads and nudge targets for task dispatch
//! - [`spawn`] — the `Spawn` trait extracted against
//!   both Adapters (ADR-097 / PR-4)

#![forbid(unsafe_code)]

pub mod aider;
pub mod beads;
pub mod claude;
pub mod codex;
pub mod command_runner;
pub mod dispatch;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod opencode;
pub mod presence_sensor;
pub mod readiness;
pub mod registry;
pub mod spawn;
pub mod tmux;

#[cfg(any(test, feature = "test-support"))]
pub use mock::MockBackend;
pub use tmux::TmuxBackend;
