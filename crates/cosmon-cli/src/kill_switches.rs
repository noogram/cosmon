// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem adapter for the kill-switch catalogue
//! ([`cosmon_core::kill_switch`], issue #108).
//!
//! The core decides which switch halts which component; this module answers
//! the one I/O question it leaves open — which switch files exist under
//! `~/.cosmon/` right now. Every `cs` path that acts autonomously asks
//! [`halting`] before acting, so the answer is read in one place.

use std::path::{Path, PathBuf};

use cosmon_core::kill_switch::{self, Autonomous, KillSwitch};

/// `~/.cosmon`, where every kill-switch file lives. `None` when no home
/// directory can be resolved — then no switch can be read, and none is
/// active.
#[must_use]
pub fn switch_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cosmon"))
}

/// The switches whose file exists under `dir`, in [`KillSwitch::ALL`] order.
#[must_use]
pub fn active_in(dir: &Path) -> Vec<KillSwitch> {
    KillSwitch::ALL
        .into_iter()
        .filter(|s| dir.join(s.file_name()).exists())
        .collect()
}

/// The switches active for the current user.
#[must_use]
pub fn active() -> Vec<KillSwitch> {
    switch_dir().map(|d| active_in(&d)).unwrap_or_default()
}

/// The switch that halts `component` right now, if any.
#[must_use]
pub fn halting(component: Autonomous) -> Option<KillSwitch> {
    kill_switch::halting(component, &active())
}

/// Absolute path of `switch`'s file, for messages that tell the operator
/// what to remove. Falls back to the `~/.cosmon/<file>` spelling.
#[must_use]
pub fn display_path(switch: KillSwitch) -> String {
    switch_dir().map_or_else(
        || format!("~/.cosmon/{}", switch.file_name()),
        |d| d.join(switch.file_name()).display().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_in_reads_file_presence_in_catalogue_order() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(active_in(tmp.path()).is_empty());
        std::fs::write(tmp.path().join("ask.off"), "").unwrap();
        std::fs::write(tmp.path().join("stand-down.lock"), "").unwrap();
        assert_eq!(
            active_in(tmp.path()),
            [KillSwitch::StandDown, KillSwitch::Ask]
        );
    }
}
