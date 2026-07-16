// SPDX-License-Identifier: AGPL-3.0-only

//! Dashboard view ports and file-backed adapter for Cosmon fleet observation.
//!
//! Defines two hexagonal ports:
//!
//! - [`DashboardView`] — read-only queries for dashboard rendering
//!   (molecule list, single molecule, fleet snapshot, links, revision).
//! - [`SparkIntake`] — write port for ingesting spark events
//!   (energy ticks, status changes) from external probes.
//!
//! The [`FileCockpitView`] adapter implements `DashboardView` over
//! `cosmon-filestore`'s `FileStore`, reading directly from `.cosmon/state/`.

#![forbid(unsafe_code)]

pub mod adapter;
pub mod selfcheck;
pub mod view;

pub use adapter::FileCockpitView;
pub use selfcheck::{run_selfcheck, SelfcheckResult};
pub use view::{
    compute_liveness, DashboardView, EventEntry, Liveness, MoleculeDetail, MoleculeSummary,
    SparkIntake,
};
