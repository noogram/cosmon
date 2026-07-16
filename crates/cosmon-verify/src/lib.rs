// SPDX-License-Identifier: AGPL-3.0-only

//! Trace validator for Cosmon event logs.
//!
//! This crate replays an append-only `events.jsonl` log through a *scheduler
//! model* and either (a) certifies the trace as a refinement of the model's
//! invariants, or (b) reports the first invariant violation with enough
//! context for an operator to diagnose it.
//!
//! # Why this exists
//!
//! `events.jsonl` is **already** the labeled-transition-system trace of
//! a cosmon run. The shortest path to a useful CI gate is therefore not a
//! full state-space exploration of a TLA+ spec, but **trace validation** —
//! the spec describes legal transitions; the real event log attests to what
//! actually happened.
//!
//! - Each historical trace we replay adds information-theoretic bits with
//!   no extra engineering cost.
//! - Trace validation sidesteps the lossy tmux / LLM / git channel because
//!   we validate what *actually happened*, not what *could* happen.
//!
//! # Architecture
//!
//! The validator is pluggable — the scheduler model is expressed as a set of
//! [`Invariant`] predicates over a [`SchedulerState`]. Phase 1 ships a
//! conservative baseline ([`baseline_invariants`]) drawn from the known-good
//! molecule lifecycle; a later Phase 1 TLA+ spec will provide a richer set
//! that replaces or extends it.
//!
//! # Example
//!
//! ```
//! use cosmon_verify::{baseline_invariants, TraceValidator};
//!
//! let log = "\
//! {\"seq\":0,\"timestamp\":\"2026-04-14T10:00:00Z\",\"type\":\"molecule_nucleated\",\
//! \"molecule_id\":\"cs-20260414-aaaa\",\"formula_id\":\"task-work\"}\n\
//! {\"seq\":1,\"timestamp\":\"2026-04-14T10:00:01Z\",\"type\":\"molecule_status_changed\",\
//! \"molecule_id\":\"cs-20260414-aaaa\",\"from\":\"pending\",\"to\":\"running\"}\n";
//!
//! let validator = TraceValidator::new(baseline_invariants());
//! let outcome = validator.validate_str(log).unwrap();
//! assert!(outcome.is_ok());
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod invariants;
pub mod model;
pub mod validator;

pub use error::{ValidationError, Violation};
pub use invariants::{baseline_invariants, Invariant, InvariantBox};
pub use model::{MoleculeTraceState, SchedulerState, WorkerTraceState};
pub use validator::{TraceValidator, ValidationOutcome};
