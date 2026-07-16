// SPDX-License-Identifier: AGPL-3.0-only

//! # Cosmon visual charter — rendering adapters
//!
//! This crate is the *rendering* half of the Cosmon visual charter.
//! The *source of truth* lives in
//! [`cosmon_core::visual`](../cosmon_core/visual/index.html) — one TOML
//! file, one set of enums, one struct (`VisualToken`). `cosmon-style`
//! adds two output adapters on top:
//!
//! 1. **ANSI** — [`ansi`] module — paints terminal strings in `TrueColor`
//!    using the charter's hex values. No 16-color fallback: the jr
//!    design directive explicitly requires us to avoid the terminal's
//!    theme-dependent base palette.
//! 2. **CSS** — [`css`] module — projects the charter onto a stylesheet
//!    served at `/charter.css` by `cosmon-cockpit-http`. Role hues become
//!    CSS variables; statuses become border-language utility classes.
//!
//! ## The one-struct-two-renderers rule
//!
//! Neither renderer is allowed to invent a concept. If the TOML does not
//! know about a role, status, or energy bucket, this crate cannot render
//! it. That is the invariant that keeps `cs watch` and the HTML cockpit
//! in lockstep.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ansi;
pub mod css;
pub mod swatch;

pub use ansi::{format_role, format_status, format_worker_status, paint_hex};
pub use css::charter_css;
pub use swatch::{print_swatch, render_swatch};

// Re-export the domain types so callers import one crate instead of two.
pub use cosmon_core::visual::{
    parse_hex, truecolor_to_256_cube, Charter, EnergyBucket, Role, Status, VisualToken,
};
