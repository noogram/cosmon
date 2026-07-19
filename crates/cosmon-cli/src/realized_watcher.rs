// SPDX-License-Identifier: AGPL-3.0-only

//! The argv contract of the detached realized-model watcher.
//!
//! `cs tackle` arms the first-turn watcher by re-execing the current binary
//! as `cs realized-watch …` (round-4 / COND-1, delib-20260718-c70e / D4).
//! That re-exec is the only path that runs in production, so the integration
//! test that exercises it must invoke the *same* command line — not a
//! hand-copied approximation that can silently drift when a flag is renamed.
//!
//! The argv therefore lives here, on the library surface both sides share:
//! `cmd::tackle` spawns it, `tests/realized_watch_reexec.rs` runs it. A flag
//! rename that forgets the test is impossible by construction — there is one
//! definition, and the test consumes it.

use std::ffi::OsString;
use std::path::Path;

/// The argv (subcommand + flags, no program name) of the detached watcher
/// `cs tackle` arms for `mol_id`.
///
/// * `mol_id` — the molecule whose worker session is watched.
/// * `worktree` — the worker's working directory: the pane-independent join
///   key the capture core uses to resolve the session log.
/// * `state_dir` — the fleet state directory, passed through the global
///   `--config` flag so the child writes to the same journal as the
///   dispatcher (a child that inherited discovery would resolve the *cwd's*
///   galaxy, not the dispatched one).
#[must_use]
pub fn watcher_argv(mol_id: &str, worktree: &Path, state_dir: &Path) -> Vec<OsString> {
    vec![
        OsString::from("realized-watch"),
        OsString::from(mol_id),
        OsString::from("--cwd"),
        worktree.as_os_str().to_os_string(),
        OsString::from("--config"),
        state_dir.as_os_str().to_os_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape is load-bearing: `cs realized-watch` takes the molecule
    /// positionally and both paths by flag. Locking it here makes a rename
    /// a compile-or-test failure rather than a silently unarmed watcher.
    #[test]
    fn argv_carries_molecule_positionally_and_both_paths_by_flag() {
        let argv = watcher_argv(
            "task-20260719-ada3",
            Path::new("/w/tree"),
            Path::new("/s/state"),
        );
        assert_eq!(
            argv,
            vec![
                OsString::from("realized-watch"),
                OsString::from("task-20260719-ada3"),
                OsString::from("--cwd"),
                OsString::from("/w/tree"),
                OsString::from("--config"),
                OsString::from("/s/state"),
            ]
        );
    }
}
