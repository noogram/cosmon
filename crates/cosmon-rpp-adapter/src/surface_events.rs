// SPDX-License-Identifier: AGPL-3.0-only

//! Compile-time event log of `surface_added` events for the §8p frozen
//! API surface (ADR-080).
//!
//! The events live in `data/surface_events.txt`, an append-only text
//! file: each non-comment line declares one HTTP route mounted by some
//! molecule. `build.rs` parses the file and emits
//! `surface_events_generated.rs`, which we `include!` below to expose
//! [`SURFACE_EVENTS`] and [`SURFACE_ROUTES`].
//!
//! ## Why an event-fold, not a `const` array
//!
//! Originally, `frozen_api_surface()` was a hand-edited `&'static [&'static str]`,
//! and two test files carried a hard-coded `assert_eq!(surface.len(),
//! 29)`. Every parallel branch that mounted a route bumped the
//! counter naïvely, and any two such branches conflicted at merge time
//! (v1.4 drain: 5 such conflicts; v1.5 drain: 6) — the exact
//! "additive counter" anti-pattern ADR-110 §I3 abolishes.
//!
//! Folding the list out of an append-only event log fixes it: appends
//! merge cleanly, and the counter is derived from `EVENTS.len()` — no
//! integer in source code ever names the surface size (wheeler
//! `I-ADDITIVE-COUNTERS`).

/// §8p exposure classification, re-exported from the shared canon
/// parser so the build-time fold and the runtime consumers share one
/// type. `TenantVerb` routes participate in the
/// bijection test; `AdapterOnly` routes intentionally have no `#[verb]`
/// counterpart; `OperatorOnly` must never appear on the frozen surface.
pub use cosmon_surface_canon::Exposure;

/// A single `surface_added` event: one molecule mounted one route at
/// a known date. Persisted as a line in `data/surface_events.txt`,
/// reified at compile time by `build.rs`.
///
/// Since the column enrichment the event also carries the route's
/// principal, minimal scope,
/// §8p exposure and blurb — the canon columns that replaced the
/// hand-maintained copies (`is_adapter_only()` + `forbidden` in
/// `tests/api_surface_freeze.rs`, `help::scope_for` in
/// `cosmon-thin-cli`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceEvent {
    /// `"METHOD PATH"` — the same wire-shape returned by
    /// [`crate::frozen_api_surface`] (e.g. `"GET /v1/molecules/{id}"`).
    pub method_path: &'static str,
    /// Identifier of the molecule that mounted the route. Free-form
    /// (`task-YYYYMMDD-xxxx`, `adr-NNNN`, …) — used for forensics, not
    /// keyed.
    pub molecule_id: &'static str,
    /// `YYYY-MM-DD` date the molecule landed. Free-form string; no
    /// `chrono` parsing — the value is for humans reading the diff.
    pub timestamp: &'static str,
    /// Wire-form principal class of the caller: `"tenant"`,
    /// `"operator"`, or `"worker"` — mirrors the `#[verb]` annotation
    /// on the matching stub (validated at build time).
    pub principal: &'static str,
    /// Minimal `OAuth2` scope expression required by the route: `"-"`
    /// for authentication-level routes (no scope check), else one or
    /// more `cosmon:`-prefixed scopes joined by `+` (AND semantics,
    /// e.g. tackle's `"cosmon:molecule:write+cosmon:worker:spawn"`).
    pub scope: &'static str,
    /// §8p exposure classification — see [`Exposure`].
    pub exposure: Exposure,
    /// One-line human description of the route, drained from the clap
    /// doc-comments. Replaces the former free-form `note` field.
    pub blurb: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/surface_events_generated.rs"));
