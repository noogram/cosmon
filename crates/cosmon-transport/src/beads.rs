// SPDX-License-Identifier: AGPL-3.0-only

//! Beads CLI wrapper — re-exported from [`cosmon_bridge_claude::beads`].
//!
//! This module re-exports the beads operations from `cosmon-bridge-claude`
//! so existing code that depends on `cosmon-transport::beads` continues to work.
//!
//! The re-export is deliberately **explicit**, not a glob (`pub use …::beads::*`).
//! A glob would let any future `pub` addition in the upstream module silently
//! widen this crate's public surface — an uncontrolled semver hazard (Rust Rule
//! no-glob-reexport, task-20260712-2897 review F4). New upstream items must be
//! re-exported here by name, as a conscious, reviewable decision.

pub use cosmon_bridge_claude::beads::{
    close_bead, create_bead, list_beads, update_bead, BeadSummary, BeadsError,
};
