// SPDX-License-Identifier: AGPL-3.0-only

//! The retackle lease — the marker that tells patrol "a forced re-tackle
//! holds this molecule; a dead pane here is not an orphan" (COSMON #90).
//!
//! `cs tackle --force` on a molecule with a live worker tears the old tmux
//! session down before the new one is spawned. In that window — old pane
//! dead, new pane not up yet — the molecule's control-plane facts read
//! exactly like a genuinely orphaned one: `Running`, assigned worker, no
//! live session. Patrol's orphan sweep (`auto_freeze_orphans`) cannot tell
//! the two apart from state alone, so without a marker it freezes or
//! collapses the molecule mid-retackle and releases its dependents (#90),
//! echoing the deadlock #35 already found in the reverse direction (a dead
//! session that survives its process and blocks the respawn).
//!
//! This module holds the pure, I/O-free half: the marker's filename, its
//! lease window, and the freshness test. Writing and reading the marker
//! file is effectful and lives in the CLI shell (`cs tackle` writes it
//! before reclaiming the old session; `cs patrol` reads it before treating
//! a dead session as an orphan) — the domain core stays I/O-free per
//! `docs/architectural-invariants.md`.
//!
//! **Self-expiring by design.** The lease is not cleared by a guaranteed
//! callback alone (an RAII guard in `cs tackle` removes it on every return
//! path, including error returns) — it also carries a TTL, so a `cs tackle`
//! process that is `kill -9`'d mid-retackle cannot wedge the molecule out of
//! patrol's reach forever. [`LEASE_TTL`] is generous (worktree setup + spawn
//! can take real time on a cold checkout) but finite: once it elapses, a
//! molecule that is still stuck reverts to being a plain orphan.

use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};

/// The sentinel file's name inside a molecule's state directory.
pub const LEASE_FILENAME: &str = ".retackle-in-progress";

/// How long a lease stays valid after it is written. Generous enough to
/// cover a cold `git worktree add` + spawn (measured cold gate runs in this
/// repo run into minutes; a single worktree setup is a small fraction of
/// that), short enough that an abandoned lease self-heals well within an
/// operator's patience.
pub const LEASE_TTL: Duration = Duration::minutes(10);

/// The path of the retackle-lease marker inside a molecule's state
/// directory. Both the writer (`cs tackle`) and the reader (`cs patrol`)
/// build the path through this one function, so the two sides can never
/// drift onto different filenames.
#[must_use]
pub fn lease_path(mol_dir: &Path) -> PathBuf {
    mol_dir.join(LEASE_FILENAME)
}

/// Whether a lease written at `written_at` is still active at `now`.
///
/// Pure: the CLI shell reads the marker's timestamp and calls this to
/// decide whether the lease still covers the molecule, or has expired and
/// the molecule should be treated as a plain, unprotected orphan.
#[must_use]
pub fn is_fresh(written_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(written_at) < LEASE_TTL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_path_is_the_sentinel_inside_the_molecule_dir() {
        let mol_dir = Path::new("/tmp/mol-dir");
        assert_eq!(
            lease_path(mol_dir),
            Path::new("/tmp/mol-dir/.retackle-in-progress")
        );
    }

    #[test]
    fn a_lease_written_just_now_is_fresh() {
        let now = Utc::now();
        assert!(is_fresh(now, now));
    }

    #[test]
    fn a_lease_within_the_ttl_is_fresh() {
        let now = Utc::now();
        let written_at = now - Duration::minutes(9);
        assert!(is_fresh(written_at, now));
    }

    #[test]
    fn a_lease_past_the_ttl_is_not_fresh() {
        let now = Utc::now();
        let written_at = now - Duration::minutes(11);
        assert!(!is_fresh(written_at, now));
    }

    #[test]
    fn a_lease_exactly_at_the_ttl_boundary_is_not_fresh() {
        // Strict `<`, matching the whisper quiet-period boundary convention
        // in `patrol::heal_gate` — the boundary itself is not still covered.
        let now = Utc::now();
        let written_at = now - LEASE_TTL;
        assert!(!is_fresh(written_at, now));
    }
}
