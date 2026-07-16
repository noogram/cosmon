// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-daemon` — multi-galaxy HTTP daemon for cluster-side cosmon state.
//!
//! Decision (2026-04-26 operator): the third native app of the local cluster
//! (after Verdict and Mur du Matin) is the **Cosmon-app** — an iOS surface
//! to *feel* the reactor from the couch. The wire to feed it is here: a
//! tiny axum daemon that aggregates every galaxy's `.cosmon/state/`
//! directory and exposes them under `/v1/galaxies/...`.
//!
//! ## Wire surface (read-only in v1)
//!
//! - `GET /v1/health` — liveness + galaxy/running counts.
//! - `GET /v1/galaxies` — list of galaxies under `/srv/cosmon/*/` that
//!   carry a `.cosmon/state/` directory.
//! - `GET /v1/galaxies/{galaxy}/molecules` — molecule list, optional
//!   `?status=pending,running` filter, sorted `updated_at` desc.
//! - `GET /v1/galaxies/{galaxy}/molecules/{id}` — full molecule detail
//!   (briefing + log tail + current step).
//! - `GET /v1/galaxies/{galaxy}/molecules/{id}/log` — `log.md` raw text.
//! - `GET /v1/fleets` — fleet summaries across every galaxy.
//!
//! Write actions (tackle/wait/done/collapse) are explicitly out of scope
//! for v1 — the operator runs `cs` on the Mac for those. The app just
//! *feels* the cluster, it does not steer it. v1.1 may add SSE for push
//! and a curated POST surface.
//!
//! ## Hexagonal boundary
//!
//! The daemon imports `cosmon-cockpit::{FileCockpitView, DashboardView}`
//! to read each galaxy's state, and `apps-transport-http` for binding,
//! middleware and error mapping. No domain logic lives here — this crate
//! is a wire adapter.

#![forbid(unsafe_code)]

pub mod galaxies;
pub mod handlers;
pub mod state;

pub use state::{AppState, GalaxiesRoot};
