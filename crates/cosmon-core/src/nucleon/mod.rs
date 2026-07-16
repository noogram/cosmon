// SPDX-License-Identifier: AGPL-3.0-only

//! Nucléon admission-test primitives.
//!
//! A Nucléon is *anything that causes molecules* — a human operator, an
//! LLM worker with a stable `nucleon_id`, a hypothetical world-model, or
//! Noogram-self. The `nucleon-test` formula
//! (`.cosmon/formulas/nucleon-test.formula.toml`) runs seven behavior
//! tests (T1..T7) and three guarantees (G1..G3) over the candidate's
//! `.cosmon/` footprint and writes an admissibility report. The formula
//! is a **telescope, not a gate**: it produces evidence, never prevents
//! nucleation.
//!
//! This module provides the pure helpers that the formula's steps
//! consume: the test identifiers, verdict enum, scan inventory, and
//! report aggregation. I/O and filesystem access live in the worker path
//! (`cosmon-cli` + `cosmon-state`); the types here are I/O-free so they
//! can be unit-tested without a fixture on disk.
//!
//! # References
//!
//! - ADR-066 §(4) — ratified T1..T7 spec.

pub mod test;

pub use test::{
    decide_admission, detect_excluded_substrates, probe, smallest_fix_hints, AdmissionDecision,
    ExcludedSubstratePattern, GuaranteeId, GuaranteeResult, NucleonReport, NucleonScan,
    ProbeOutput, SessionOverlap, TestId, TestResult, Verdict,
};
