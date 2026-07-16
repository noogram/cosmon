// SPDX-License-Identifier: AGPL-3.0-only

//! # Cosmon Scheduler — unified patrol scheduler.
//!
//! ## Role in the architecture
//!
//! `cosmon-scheduler` is a **cron-triggered, TOML-driven launcher** for
//! recurring patrols. One macOS `LaunchAgent` (or Linux cron entry) invokes
//! `cosmon-scheduler tick` every N seconds; the binary reads
//! `~/.config/cosmon/patrols.toml`, evaluates which patrols are due, and
//! dispatches each as a detached subprocess. Between ticks nothing runs —
//! this honors the cosmon "no daemon in core" invariant
//! ([ADR-016](../../docs/adr/016-autonomy-regimes-and-resident-runtime.md)).
//!
//! Think of it as **Starship for schedulers**: one config, one binary, many
//! patrols. See the governing plan
//! ([idea-20260417-b52d](../../.cosmon/state/fleets/default/molecules/idea-20260417-b52d/plan.md)).
//!
//! ## What this crate now ships (Step 2 — complete)
//!
//! - The TOML schema ([`Config`], [`SchedulerConfig`], [`Patrol`]) with
//!   XOR validation on `interval_seconds` vs `cron`.
//! - A pure [`tick`] evaluator that returns one [`Decision`] per patrol.
//! - **Interval accounting** + **POSIX 5-field cron matching**
//!   ([`CronExpr`]) gated on the prior [`SchedulerState`].
//! - **Atomic [`SchedulerState`] I/O** (write-to-`.tmp` + rename).
//! - **Real subprocess dispatch** ([`ProcessDispatcher`]) with detached
//!   and wait modes, log redirection, and working-dir + env injection.
//! - A CLI front-end (`src/main.rs`) that supports both
//!   `cosmon-scheduler tick --dry-run` (no dispatch) and
//!   `cosmon-scheduler tick` (real dispatch + state update).
//!
//! ## What is still deferred
//!
//! - `cs scheduler status` TUI (child #3 of the governing plan).
//! - `LaunchAgent` template + install script (child #4).
//! - Per-run timeout enforcement (`timeout_seconds` is parsed but not
//!   enforced; this is a v2 enhancement).
//! - Named months / weekdays in cron (`JAN`, `SUN`).
//! - Log rotation (explicit v1 non-goal).
//!
//! ## Example
//!
//! ```
//! use cosmon_scheduler::{tick, Config, Decision, EnvProbe, SchedulerState};
//!
//! let raw = r#"
//!     [scheduler]
//!     state_file = "~/.cosmon/scheduler.state.json"
//!     log_file = "~/.cosmon/scheduler.log"
//!     kill_switch = "~/.cosmon/stand-down.lock"
//!     tick_interval_seconds = 60
//!
//!     [[patrol]]
//!     name = "hello"
//!     interval_seconds = 300
//!     command = ["echo", "hello"]
//! "#;
//! let cfg: Config = toml::from_str(raw).unwrap();
//! let state = SchedulerState::default();
//! let env = EnvProbe;
//!
//! let decisions = tick(&cfg, &env, &state);
//! assert_eq!(decisions.len(), 1);
//! assert_eq!(decisions[0].0, "hello");
//! assert!(matches!(decisions[0].1, Decision::WouldFire));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod config;
pub mod convergence;
pub mod cron;
pub mod decision;
pub mod dispatch;
pub mod environment;
pub mod events;
pub mod hooks;
pub mod state;
pub mod tick;

pub use config::{Config, Patrol, SchedulerConfig, Sunset, SunsetStrategy};
pub use convergence::{
    operator_trigger_predicate, read_samples_tolerant, rolling_stdev, sample_count_predicate,
    variance_threshold_predicate, ConvergenceWarning, SampleRead, WarningKind,
};
pub use cron::{CronError, CronExpr};
pub use decision::Decision;
pub use dispatch::{
    run_sunset_action, DispatchError, DispatchOutcome, Dispatcher, ProcessDispatcher, SunsetOutcome,
};
pub use environment::{EnvProbe, Environment};
pub use events::{append_event, derive_events_path, SchedulerEvent};
pub use hooks::{run_sunset_hooks, HookOutcome, HookStatus};
pub use state::{PatrolState, SchedulerState, StateError};
pub use tick::tick;
