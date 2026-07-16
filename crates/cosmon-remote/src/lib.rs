// SPDX-License-Identifier: AGPL-3.0-only

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::doc_markdown
)]

//! `cosmon-remote` — thin Rust CLI mirroring the cosmon-rpp v1 wire surface.
//!
//! This crate replaces the justfile served by the cosmon-rpp-adapter in
//! Phase 0. Every recipe of the justfile
//! becomes one subcommand of the binary; persistent per-deployment
//! configuration replaces the brittle `__COSMON_HOST__` templating that
//! pinned only the host in Phase 0.
//!
//! Three learnings from the AWS live-deploy test (2026-05-22) shape the
//! design:
//!
//! 1. **Templating partial = real gap.** Phase 0 pinned only `COSMON_HOST`;
//!    the operator had to edit `sub`, `aud`, `oidc_url`, `noyau` by hand
//!    every time. Profiles in `~/.cosmon-remote/profiles/<name>.toml`
//!    persist the full four-tuple.
//! 2. **No placeholder check in the binary.** The fragile
//!    `__COSMON_HOST__` self-test was removed in install.sh v1.2.1; the
//!    CLI never reintroduces the pattern — it reads concrete values from
//!    the profile.
//! 3. **Scheme preservation.** The CLI honours the scheme the operator
//!    typed (`http` vs `https`); the adapter is read-only on this.
//!
//! Surface: every route the binary dials is projected at build time
//! from the §8p surface canon
//! (`crates/cosmon-rpp-adapter/data/surface_events.txt`) — see
//! [`canon`]. Since the tenant-CLI fusion this is the ONE tenant
//! binary; the former
//! `cs-thin` engine discipline (canon projection + bijection test)
//! lives under its hood.

pub mod canon;
pub mod client;
pub mod config;
pub mod cost;
pub mod credential;
pub mod do_flow;
pub mod doctor;
pub mod error;
pub mod hints;
pub mod oidc;
pub mod phone_home;
pub mod pkce;

pub use client::{
    ArtifactEntry, ArtifactManifest, Client, DrainBounds, DrainStarted, EnsembleEnvelope,
    ListFilters, MoleculeEnvelope, MoleculeView, NucleateRequest, ReactiveRefresh, RunEnvelope,
    TackleBody, TackleEnvelope,
};
pub use config::{Profile, ProfileStore};
pub use credential::{
    BackendKind, CredentialKey, CredentialLock, CredentialStore, SecretToken, StoredCredential,
};
pub use error::{CredentialStoreError, Error, Result};
pub use oidc::{
    LoginOutcome, OidcEndpoints, OidcError, RefreshConfig, RefreshRotation, TokenState,
};
