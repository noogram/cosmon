// SPDX-License-Identifier: AGPL-3.0-only

//! CLI repository-root resolution for the shared work-location projection.

use std::path::PathBuf;

pub(crate) use cosmon_state::WorkLocation;

use super::Context;

/// Resolve the repository root against which fleet-relative checkout paths
/// were recorded. A flat test store falls back to the invocation directory.
#[must_use]
pub(crate) fn repo_root(ctx: &Context) -> PathBuf {
    let config = super::resolve_config_from_context(ctx);
    let root = cosmon_cli::target_repo::resolve_from_config(&config)
        .map(|resolved| resolved.root)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_else(|_| PathBuf::from("."));
    std::fs::canonicalize(&root).unwrap_or(root)
}
