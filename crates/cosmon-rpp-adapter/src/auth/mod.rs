// SPDX-License-Identifier: AGPL-3.0-only

//! `auth` — `OAuth2` scope catalog and scope-related helpers for the
//! cosmon RPP surface.
//!
//! The module is deliberately small and load-bearing: the [`scopes`]
//! submodule is the single source of truth for every scope literal the
//! adapter emits or accepts. Adding a scope here is additive (minor
//! bump); reusing an existing one for a costly verb is a doctrinal
//! regression (ADR-080 §6.5).

pub mod scopes;
