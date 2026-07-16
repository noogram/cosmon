// SPDX-License-Identifier: AGPL-3.0-only

//! Reconciliation — pure computation of derived state.
//!
//! This module holds the reconciliation primitives of cosmon: given
//! operator intent + fresh observations, compute the effective view.
//! Everything here is pure (no I/O, no persistence), infallible, and
//! recomputed on demand.
//!
//! # Module convention
//!
//! Reconciliation functions live under `reconcile/<subject>.rs`, one
//! file per reconciled entity:
//!
//! | file         | input                                  | output          |
//! | ------------ | -------------------------------------- | --------------- |
//! | `molecule.rs`| `MoleculeStatus` + worker view         | `MoleculeHealth`|
//!
//! The worker reconciliation primitive ([`crate::worker::reconcile`])
//! currently lives in [`crate::worker`] for historical reasons; it
//! obeys the same contract as the entries below and will migrate here
//! in a follow-up without a signature change.
//!
//! # Design
//!
//! The observation DAG is `Transport → Worker → Molecule → Fleet`.
//! Each layer derives its effective view from the layer below, and
//! the result is **never persisted**. Adding a new reconciled subject
//! means adding a new file here — not a new state field, not a new
//! `Reconcilable` trait, not a new migration. The function signature
//! is the contract.

pub mod molecule;

pub use molecule::{molecule_health, MoleculeHealth};
