// SPDX-License-Identifier: Apache-2.0

//! `#[verb]` stub annotations that populate the link-time registry for
//! the `cs-thin` binary (T-CST-V0 / T-CST-EXPAND).
//!
//! # Why stubs ?
//!
//! The macro emits a `linkme::distributed_slice` entry per annotated
//! function. The slice is populated at link time, so a verb only
//! shows up in `cosmon_thin_cli::registry::all()` when the crate
//! that annotates it is *linked into the binary*. cs-thin is
//! deliberately thin — it does not link `cosmon-state` (which would
//! pull a state store, formula loader, briefing-seal, …) — so the
//! annotations on `cosmon_state::ops::*` cannot reach cs-thin's
//! registry by dep chain.
//!
//! The fix is a parallel *stub* declaration here. Each stub names
//! the canonical wire metadata (method + path + principal); the body
//! is a no-op that nobody ever calls because cs-thin dispatches
//! verbs by hand in [`crate::cli`].
//!
//! # The duplication is intentional and load-bearing
//!
//! Keeping the wire metadata in two places — cs-thin (this file) and
//! `cosmon-state::ops::*` (the lib functions themselves) — looks
//! redundant, but it is the §8p invariant in code:
//!
//! - `cosmon-state::ops::*#[verb]` is the *server-side* declaration
//!   that the rpp-adapter `api_surface_freeze` test consumes via
//!   `frozen_api_surface()`.
//! - `cosmon-thin-cli::verbs::*` is the *client-side* declaration
//!   that the cs-thin registry surfaces in `verbs --check`.
//!
//! The bijection test in `crates/cosmon-rpp-adapter/tests/
//! api_surface_freeze.rs` refuses any drift between the two.

#![allow(clippy::unnecessary_wraps)]

/// Wire stub for `GET /v1/molecules/:id`.
#[cosmon_thin_macro::verb(method = "GET", path = "/v1/molecules/:id", principal = "tenant")]
pub fn observe() {}

/// Wire stub for `POST /v1/molecules`.
#[cosmon_thin_macro::verb(method = "POST", path = "/v1/molecules", principal = "tenant")]
pub fn nucleate() {}

/// Wire stub for `POST /v1/molecules/:id/tags`.
#[cosmon_thin_macro::verb(method = "POST", path = "/v1/molecules/:id/tags", principal = "tenant")]
pub fn tag() {}

/// Wire stub for `GET /v1/molecules` (T-CST-EXPAND).
#[cosmon_thin_macro::verb(method = "GET", path = "/v1/molecules", principal = "tenant")]
pub fn ensemble() {}

/// Wire stub for `POST /v1/molecules/:id/collapse` (T-CST-EXPAND).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/collapse",
    principal = "tenant"
)]
pub fn collapse() {}

/// Wire stub for `POST /v1/molecules/:id/freeze` (T-CST-EXPAND).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/freeze",
    principal = "tenant"
)]
pub fn freeze() {}

/// Wire stub for `POST /v1/molecules/:id/stuck` (T-CST-EXPAND).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/stuck",
    principal = "tenant"
)]
pub fn stuck() {}

/// Wire stub for `POST /v1/molecules/:id/tackle` (remote-tackle V2).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/tackle",
    principal = "tenant"
)]
pub fn tackle() {}

/// Wire stub for `POST /v1/molecules/:id/run` (bounded drain, ADR-124).
/// The client REQUESTS a drain of the
/// DAG rooted at `:id`; the resident loop runs inside the tenant
/// container under the binding's B1/B2/B3 bounds. This is NOT the
/// operator `cs run` — ADR-124 documents the §5.1 re-decision.
#[cosmon_thin_macro::verb(method = "POST", path = "/v1/molecules/:id/run", principal = "tenant")]
pub fn run() {}

// ---------------------------------------------------------------------------
// D-AVATAR instance lifecycle (task-20260525-738e)
// ---------------------------------------------------------------------------

/// Wire stub for `GET /v1/avatar/:instance_id/status`.
#[cosmon_thin_macro::verb(
    method = "GET",
    path = "/v1/avatar/:instance_id/status",
    principal = "tenant"
)]
pub fn avatar_status() {}

/// Wire stub for `POST /v1/avatar/:instance_id/incarnate`.
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/avatar/:instance_id/incarnate",
    principal = "tenant"
)]
pub fn avatar_incarnate() {}

/// Wire stub for `POST /v1/avatar/:instance_id/grant`.
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/avatar/:instance_id/grant",
    principal = "tenant"
)]
pub fn avatar_grant() {}

/// Wire stub for `GET /v1/avatar/:instance_id/audit`.
#[cosmon_thin_macro::verb(
    method = "GET",
    path = "/v1/avatar/:instance_id/audit",
    principal = "tenant"
)]
pub fn avatar_audit() {}

/// Wire stub for `GET /v1/avatar/:instance_id/mould-info`.
#[cosmon_thin_macro::verb(
    method = "GET",
    path = "/v1/avatar/:instance_id/mould-info",
    principal = "tenant"
)]
pub fn avatar_mould_info() {}

// ---------------------------------------------------------------------------
// D-AVATAR canal (b) — converse (task-20260610-0b57, delib-20260610-9a0c T3)
// ---------------------------------------------------------------------------

/// Wire stub for `POST /v1/avatar/converse`.
///
/// The client verb is TOP-LEVEL `converse` — never an `avatar`
/// subcommand: « avatar est un mot de doctrine, jamais un nom d'API »
/// (tenant guide §12.2). The route stays on-by-binding
/// server-side; synchronous `request` messages carry a hop counter
/// bounded by the adapter (L3 anti-cycle).
#[cosmon_thin_macro::verb(method = "POST", path = "/v1/avatar/converse", principal = "tenant")]
pub fn converse() {}
