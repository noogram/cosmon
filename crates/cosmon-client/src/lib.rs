// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::unnecessary_debug_formatting,
    clippy::doc_markdown,
    clippy::module_name_repetitions
)]

//! Thin HTTP client for a remote `cosmon-saas` server.
//!
//! The client mirrors the pilot cycle vocabulary (`nucleate`, `tackle`,
//! `observe`, `wait`, `done`, `fetch`) but every operation becomes an HTTPS
//! call to the server. There is **no local `.cosmon/` state** — the client
//! stores only downloaded artifacts and a tiny config file.
//!
//! ```text
//! laptop (LB)                tunnel                  Mac (server)
//! ┌────────────┐   HTTPS   ┌─────────┐   local cs   ┌────────────┐
//! │ cs-client  │ ────────▶ │  CF    │ ───────────▶ │ cosmon-saas │
//! │  (thin)    │ ◀──────── │ tunnel │ ◀─────────── │ + .cosmon/  │
//! └────────────┘           └─────────┘              └────────────┘
//! ```

pub mod client;
pub mod config;

pub use client::{ArtifactListing, Client, MoleculeState, NucleateResponse};
pub use config::{ClientConfig, ConfigOverrides};
