// SPDX-License-Identifier: AGPL-3.0-only

//! Shared fleet observability queries for Cosmon.
//!
//! This crate is the common substrate consumed by both:
//!
//! - `cs peek` — the terminal TUI that inspects a running fleet.
//! - `cosmon-cockpit-http` — the HTTP dashboard serving the same view.
//!
//! It is **not** a CLI nor a TUI — it exposes pure data types and query
//! functions. Both adapters run against the same in-memory model, so a
//! single [`FleetSnapshot`] fixture pins their behavior and prevents
//! drift between the two surfaces.
//!
//! # Domain model
//!
//! The core types mirror what an observer of a multi-project, multi-socket
//! Cosmon deployment sees:
//!
//! - [`Session`] — a tmux session running a worker.
//! - [`Molecule`] — the unit of tracked work (task, bug, decision, …).
//! - [`Worker`] — the live process assigned to a molecule, with energy budget.
//! - [`Event`] — an append-only entry from a molecule's event log.
//!
//! # Aggregation
//!
//! Fleet state can span **multiple tmux sockets** (e.g. several
//! `/private/tmp/tmux-501/*` sockets for concurrent fleets) and **multiple
//! projects** (each with its own `.cosmon/` root). The [`aggregate`] module
//! merges those sources into a single [`FleetSnapshot`] for query.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod aggregate;
pub mod event;
pub mod fixture;
pub mod molecule;
pub mod render;
pub mod replay;
pub mod sensorium;
pub mod session;
pub mod worker;

pub use aggregate::FleetSnapshot;
pub use event::Event;
pub use molecule::{Molecule, MoleculeId, MoleculeStatus};
pub use sensorium::{HeartbeatKind, Sensorium, HEARTBEAT_WINDOW};
pub use session::{HeartbeatTier, Session, SessionFilter};
pub use worker::{EnergyBudget, Worker, WorkerId};

use thiserror::Error;

/// Errors returned by observability queries.
#[derive(Debug, Error)]
pub enum ObservabilityError {
    /// No session matched the given predicate.
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// No molecule matched the given id.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(String),
    /// No worker is attached to the queried session.
    #[error("no worker attached to session: {0}")]
    NoWorker(String),
}

/// Canonical `Result` alias for this crate.
pub type Result<T> = std::result::Result<T, ObservabilityError>;
