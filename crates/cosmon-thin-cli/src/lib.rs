// SPDX-License-Identifier: Apache-2.0

//! `cosmon-thin-cli` — tenant-CLI ENGINE library (no binary since the
//! avatar-surface A2 fusion: the `cs-thin`
//! binary was the second tenant CLI, deleted as a copy; the installed
//! product is `cosmon-remote`, which projects its routes from the same
//! canon this crate's registry is bijection-tested against).
//!
//! This crate is the runtime half of the iso-verb pair (the proc-macro half is
//! `cosmon-thin-macro`). It exposes:
//!
//! - The [`IsoVerb`] trait — the compile-time contract a function annotated
//!   with `#[verb]` adheres to.
//! - The [`Principal`] enum — authorisation principal carried in
//!   [`IsoVerb::PRINCIPAL`].
//! - The [`Client`] type — a thin HTTP client that fires a verb against a
//!   running cosmon HTTPS endpoint, with `Authorization: Bearer <jwt>`.
//! - The [`registry`] module — compile-time enumeration of every annotated
//!   verb in the binary, via the [`linkme`] distributed slice.
//!
//! # Why a *thin* CLI
//!
//! Per §8p of `architectural-invariants.md`, the
//! HTTP API surface is a strict subset of the local `cs` CLI: not every verb
//! crosses the wire. `cs-thin` covers exactly that subset — never more — and
//! the macro guarantees we cannot accidentally expand the surface without an
//! ADR. T-CST-FOUNDATION (this task) lays the machinery; T-CST-V0 will wire
//! the first three verbs (`observe`, `nucleate`, `tag`).
//!
//! # Verb registration
//!
//! Each `#[verb]` macro invocation attaches a [`registry::VerbDescriptor`] to
//! the [`registry::VERBS`] distributed slice. Linking the binary aggregates
//! every descriptor; [`registry::all`] returns the sorted slice. There is no
//! runtime registration step.

// `linkme::distributed_slice` expands to `#[unsafe(link_section = ...)]`
// — required for cross-platform link-time aggregation. We can't use
// `#![forbid(unsafe_code)]` at the crate root because the registry slice
// definition (and every `#[verb]` annotation) trips that lint. Allowing
// `unsafe_code` here is a *narrow* concession: there is no `unsafe { ... }`
// block in this crate; only the `link_section` attribute that linkme owns.
#![allow(unsafe_code)]

// The proc-macro `#[verb]` emits `::cosmon_thin_cli::...` paths. When
// the macro is invoked *inside this very crate* (e.g. on stubs in
// [`verbs`]), we have to alias the crate's own name so the absolute
// paths still resolve. Same trick `linkme`, `serde`, `tokio`, etc. use
// to be self-host-friendly.
extern crate self as cosmon_thin_cli;

pub mod cli;
pub mod client;
pub mod coverage;
pub mod help;
pub mod parity;
pub mod registry;
pub mod surface_scopes;
pub mod traits;
pub mod verbs;

pub use cli::{Cli, CliError, Command};
pub use client::{Client, ClientError};
pub use traits::{IsoVerb, Principal};

/// The cargo package version of `cosmon-thin-cli`.
///
/// Re-exported for convenience so binaries can render `cs-thin --version`
/// without depending on `env!` semantics directly.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
