// SPDX-License-Identifier: AGPL-3.0-only

//! cosmon-surface: Project internal state onto standard files.
//!
//! The surface projection engine reads fleet/molecule state and writes
//! human-readable files (STATUS.md, ISSUES.md) that any developer can
//! understand without knowing Cosmon.
//!
//! See THESIS.md Part XVI (Surface Observability) and ADR-013.

#![forbid(unsafe_code)]

mod config;
pub mod escalation;
pub mod github;
pub mod github_mirror;
mod render;
pub mod snapshot;

pub use config::{Branding, Surface, SurfaceConfig, SurfaceKind};
pub use render::{
    filter_by_surface_kinds, project_surfaces, render_deliberations_content, render_ideas_content,
    render_issues_content, render_status_content, DeclarationMap, FormulaMap,
};
