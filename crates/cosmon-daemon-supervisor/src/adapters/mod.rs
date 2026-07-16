// SPDX-License-Identifier: AGPL-3.0-only

//! Real adapters wiring the four port traits to the outside world.
//!
//! Task 2 deliverables: [`tokio_process`] for `spawn` / `signal` / `reap`
//! backed by `tokio::process`, [`notify_watcher`] for `daemons.toml` edits
//! observed via the `notify` crate with a 200 ms debounce window, and
//! [`filestore`] for `SupervisorState` persistence via the classic
//! write-tmp-and-rename atomic pattern.
//!
//! The adapters deliberately stay *thin*: every policy decision lives in
//! [`crate::policy`] (pure), every transition in [`crate::model`] (typestate),
//! every diff in [`crate::reload`] (pure). The adapters only translate
//! OS events into port-trait calls and back.

pub mod filestore;
pub mod notify_watcher;
pub mod tokio_process;

pub use filestore::FileStatePort;
pub use notify_watcher::NotifyConfigWatchPort;
pub use tokio_process::TokioProcessPort;
