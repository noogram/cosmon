// SPDX-License-Identifier: AGPL-3.0-only

//! # Cosmon Daemon Supervisor — core + ports
//!
//! ## Role in the architecture
//!
//! `cosmon-daemon-supervisor` is the **domain core** for the meta-LaunchAgent
//! that will unify every long-running Cosmon-managed daemon (notification-bot,
//! almanac, archive-service, emacs-daemon, the Flask dashboard, …) under a single
//! supervised process. The motivating plan lives in an internal plan note
//! (`idea-20260419-25fd`).
//!
//! The relationship to `cosmon-scheduler` is deliberate: both read a single
//! TOML file, both run as resident `LaunchAgent`s, both honor the global
//! `~/.cosmon/stand-down.lock` kill-switch. Where `scheduler` is **tick-based**
//! (cron / interval cadence, spawn-and-forget), the supervisor is
//! **event-driven** (file-watch, `SIGCHLD`, throttle timers) and keeps its
//! children alive with `KeepAlive` semantics.
//!
//! ## What this crate ships today (Task 1 / 5 — scaffold)
//!
//! - [`config`] — TOML schema ([`SupervisorConfig`], [`DaemonSpec`]),
//!   validation, `~` expansion.
//! - [`model`] — the [`Child`] state machine expressed via typestate
//!   ([`model::Spawning`] / [`model::Running`] / [`model::Exited`] /
//!   [`model::Throttling`] / [`model::Respawning`]). Invalid transitions
//!   are compile errors.
//! - [`reload`] — the [`reload::diff`] function that partitions the
//!   old ∪ new daemon names into `{spawn, kill, keep, changed}` based on
//!   BLAKE3 content identity (so renaming an arg is *changed*, not *keep*).
//! - [`policy`] — pure decision helpers: throttle deadline, kill-switch
//!   precedence, enabled-flag.
//! - [`ports`] — four port traits ([`ports::ProcessPort`],
//!   [`ports::ConfigWatchPort`], [`ports::ClockPort`], [`ports::StatePort`])
//!   that bracket every side-effect.
//! - Mock adapters under `tests/` that drive the same ports deterministically.
//!
//! ## What is **not** in scope for Task 1
//!
//! Real spawning (`tokio::process::Command`), file-watching (`notify`),
//! signal cascades, the composition-root binary, and the `cs daemons`
//! subcommands all land in later tasks (see the plan). This crate **must**
//! compile with zero real I/O.
//!
//! ## Key invariants (checked by tests)
//!
//! 1. [`reload::diff`] is a *partition* of `old ∪ new` — every name appears
//!    in exactly one of spawn / kill / keep / changed.
//! 2. [`config::Config::validate`] surfaces every error in one pass (no
//!    short-circuit) so the operator sees the full picture.
//! 3. The [`Child`] state machine rejects invalid transitions at compile
//!    time (e.g. `Child<Running>::spawn()` does not exist).
//! 4. Public API is fully documented (`#![deny(missing_docs)]`) and contains
//!    no `unsafe` (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod adapters;
pub mod config;
pub mod event_loop;
pub mod model;
pub mod policy;
pub mod ports;
pub mod reload;

pub use adapters::{FileStatePort, NotifyConfigWatchPort, TokioProcessPort};
pub use config::{Config, ConfigError, DaemonSpec, SupervisorConfig};
pub use event_loop::{run, Supervisor, SupervisorError, DEFAULT_TERM_GRACE};
pub use model::{Child, ChildStatus};
pub use policy::{crash_loop_alert, prune_crash_times, KillSwitchDecision, RespawnDecision};
pub use ports::{
    ClockPort, ConfigChange, ConfigWatchPort, ProcessPort, ReapOutcome, Signal, StatePort,
};
pub use reload::{diff, spec_content_hash, DiffResult};
