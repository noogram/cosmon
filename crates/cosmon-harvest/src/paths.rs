// SPDX-License-Identifier: AGPL-3.0-only

//! Paths and identifiers the harvest resolves from a [`HarvestContext`].
//!
//! These four functions were `pub(crate)` helpers in `cosmon_cli::cmd`, which
//! made them unreachable from anything but the binary. They are thin wrappers
//! over `cosmon-filestore` resolvers, and the reason they exist at all is
//! that the *precedence* between an explicit `--config` and walk-up discovery
//! must be answered identically by every caller. Re-homing them here keeps
//! that one answer shared by `cs done` and by the RPP route rather than
//! copied.

use std::path::PathBuf;

use crate::HarvestContext;

/// Resolve the state directory by walk-up discovery, ignoring any explicit
/// override.
///
/// Delegates to [`cosmon_filestore::resolve_state_dir`], which walks up from
/// the working directory the way `git` finds `.git/`.
#[must_use]
pub fn default_state_dir() -> PathBuf {
    cosmon_filestore::resolve_state_dir(None)
}

/// Derive the `config.toml` path from context.
///
/// When the context pins the state dir (`.cosmon/state/`), the config file
/// lives at `.cosmon/config.toml` (= parent of state dir). Falls back to
/// checking the state dir itself (for test environments where the state dir
/// is a flat temp directory), then to CWD walk-up discovery.
#[must_use]
pub fn resolve_config_from_context(ctx: &HarvestContext) -> PathBuf {
    if let Some(ref state_dir) = ctx.config {
        // Production: state_dir = .cosmon/state/ → parent = .cosmon/
        if let Some(parent) = state_dir.parent() {
            let candidate = parent.join("config.toml");
            if candidate.exists() {
                return candidate;
            }
        }
        // Flat layout: config.toml next to fleet.json in the same dir.
        let sibling = state_dir.join("config.toml");
        if sibling.exists() {
            return sibling;
        }
    }
    cosmon_filestore::resolve_config_path(None)
}

/// Resolve the project identity this harvest acts under.
///
/// # Errors
///
/// When the galaxy declares no resolvable `project_id`.
pub fn require_project_identity(
    ctx: &HarvestContext,
) -> anyhow::Result<cosmon_core::id::ProjectId> {
    let config_path = resolve_config_from_context(ctx);
    cosmon_filestore::resolve_project_id(&config_path).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolve the tmux socket name from project config.
///
/// Delegates to [`cosmon_filestore::resolve_tmux_socket_name`], which uses
/// `project_id` when set and otherwise derives a globally unique name from
/// the project root path. Every cosmon invocation passes this through
/// `tmux -L <socket>`, so two fleets on the same host never share a tmux
/// server (sibling-isolation invariant).
#[must_use]
pub fn tmux_socket_name(ctx: &HarvestContext) -> String {
    let config_path = resolve_config_from_context(ctx);
    cosmon_filestore::resolve_tmux_socket_name(&config_path)
}
