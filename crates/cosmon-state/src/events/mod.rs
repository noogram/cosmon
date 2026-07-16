// SPDX-License-Identifier: AGPL-3.0-only

//! Per-port emission helpers for [`cosmon_core::event_v2::EventV2`].
//!
//! Each submodule groups the **stable callsites** for one cosmon Port —
//! one free function per `EventV2` variant that adapter code calls in
//! lieu of constructing and appending the variant by hand. The
//! discipline (forgemaster §2.4, ADR-097 PR-1) is that adapters
//! never reach for [`crate::event_log::EventLogWriter`] directly:
//! the trait extraction in a later PR can move the variant constructors
//! without rewriting every adapter, because the callsite is the
//! free function here, not the writer.
//!
//! All emission helpers are **best-effort** — a serialise or write
//! failure is logged-but-swallowed in keeping with the seal-not-lock
//! discipline (see [`crate::briefing_seal`]). The hot path must never
//! fail because telemetry is unhappy.

pub mod autonomy;
pub mod worker_spawn;
