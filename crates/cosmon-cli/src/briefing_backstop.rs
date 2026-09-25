// SPDX-License-Identifier: AGPL-3.0-only

//! The briefing-submit receipt kernel, and the durable record that lets it be
//! re-run by a process the dispatcher does not own (COSMON #26-B).
//!
//! # The problem this closes
//!
//! `cs tackle` pastes a briefing into a fresh worker's composer and presses
//! Enter. On a loaded fleet the TUI is still rendering the paste, swallows the
//! keystroke, and the worker sits on `❯ [Pasted text #1 +86 lines]` doing
//! nothing — the 2026-07-20 knowledge-fleet stall, where four of eleven workers
//! burned a fleet slot for zero tokens until a human pressed Enter by hand
//! twenty minutes later.
//!
//! #26-A gave that retry a *receipt*: the briefing text we wrote ourselves is
//! gone from the composer. What it could not give it is *patience*. The retry
//! runs inside `cs tackle`, and `cs tackle` returns in seconds — whatever waits
//! on it (`cs run`, a patrol pass, a fleet loop) is usually serial, so every
//! second spent pressing Enter for one stuck worker is a second the whole fleet
//! does not dispatch. The in-band window is therefore a few short retries, and
//! an earlier attempt to hand the residual patience to a *thread* was a false
//! promise: the thread died with the process it was spawned from, long before
//! the TUI settled.
//!
//! # The shape of the fix
//!
//! Patience that outlives the dispatcher cannot live in the dispatcher. So:
//!
//! 1. when the in-band window closes with the paste still visible, `cs tackle`
//!    writes a [`BriefingPending`] record next to the molecule's state — the
//!    durable half, readable by any later process ([`write()`], [`read()`]);
//! 2. it re-execs itself as a detached `cs briefing-backstop` child, in its own
//!    process group ([`backstop_argv`]), which resumes pressing on a long
//!    budget;
//! 3. the child signs — or fails to sign — the *same* receipt, because the
//!    decision kernel below is one piece of code with two callers and two
//!    budgets, not two implementations that can drift apart.
//!
//! The record is the reason step 3 is possible at all: the needle it stores is
//! what a process with no memory of the briefing needs in order to recognise it
//! still sitting in the composer.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ─────────────────────────── the durable record ───────────────────────────

/// File name of the durable briefing-pending record, inside a molecule's state
/// directory.
pub const RECORD_FILE: &str = "briefing-pending.json";

/// Everything a process that never saw the briefing needs in order to keep
/// pressing submit for it.
///
/// Written by `cs tackle` at the moment its in-band window gives up, read by
/// the detached `cs briefing-backstop` child that outlives it. Its presence on
/// disk means exactly one thing: **the last process to look saw an unsubmitted
/// briefing in this worker's composer.** It is removed on a delivery receipt,
/// and only on a delivery receipt.
///
/// # Why the needle and not the briefing
///
/// The composer scan consults exactly one line of the input it was given — the
/// last non-empty one ([`cosmon_transport::tmux::composer_needle`]). Storing
/// the whole briefing would copy `briefing.md`, which already sits in the same
/// directory, into a second file that is neither more precise nor easier to
/// read. Storing the needle is *exact*: handing it back as the `input` of a
/// later composer scan reproduces the identical comparison, because the needle
/// of a needle is itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefingPending {
    /// The molecule whose dispatch left this behind.
    pub molecule: String,
    /// The worker whose composer still holds the briefing. Also the tmux
    /// session name — resolved through the session listing, so a worker that
    /// has since died is recognised rather than nudged into the void.
    pub worker: String,
    /// The tmux socket (`tmux -L <socket>`) the worker lives on. Recorded
    /// rather than re-derived: a detached child inherits no fleet context and
    /// must not guess which server holds the session.
    pub socket: String,
    /// The line the composer scan looks for. See the type docs.
    pub needle: String,
    /// When the in-band window gave up, RFC 3339.
    pub recorded_at: String,
    /// How long the in-band window pressed before recording this, in seconds —
    /// so a reader can tell "gave up after 8 s" from "gave up after 90 s"
    /// without knowing which build wrote the file.
    pub inband_seconds: u64,
    /// Set only when the durable backstop *also* ran out of patience: the
    /// outcome it ended on, as a bare word (`stuck-pasted`, `unobservable`,
    /// `session-gone`). Absent while the record is still live work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstop_outcome: Option<String>,
    /// When the durable backstop stopped, RFC 3339. Absent while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstop_ended_at: Option<String>,
    /// How many submit keystrokes the durable backstop landed. Absent while
    /// live; `0` on a record whose composer could never be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstop_nudges: Option<u64>,
}

/// Where the durable record lives for a molecule whose state directory is
/// `mol_state_dir`.
#[must_use]
pub fn record_path(mol_state_dir: &Path) -> PathBuf {
    mol_state_dir.join(RECORD_FILE)
}

/// Persist `record` for `mol_state_dir`, atomically.
///
/// Write-then-rename, because the reader is a *different process* that may look
/// at any instant: a torn JSON file would be read as "no record" by
/// [`read`] and the backstop would silently do nothing, which is the failure
/// this whole mechanism exists to remove.
///
/// # Errors
///
/// Any filesystem error from creating, writing, or renaming the record.
pub fn write(mol_state_dir: &Path, record: &BriefingPending) -> std::io::Result<()> {
    std::fs::create_dir_all(mol_state_dir)?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = mol_state_dir.join(format!("{RECORD_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, record_path(mol_state_dir))
}

/// Read the durable record for `mol_state_dir`, if one is there.
///
/// A missing file and an unparseable one both answer `None`: the caller's only
/// sensible response to either is "there is nothing here I can act on", and a
/// backstop that refused to start because a hand-edited file lost a comma would
/// be strictly worse than one that treats it as absent.
#[must_use]
pub fn read(mol_state_dir: &Path) -> Option<BriefingPending> {
    let raw = std::fs::read_to_string(record_path(mol_state_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Remove the durable record — the on-disk form of signing the receipt.
///
/// Idempotent: a record that is already gone is success, because two backstops
/// racing on the same molecule must both be able to finish.
///
/// # Errors
///
/// Any filesystem error other than the record already being absent.
pub fn clear(mol_state_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(record_path(mol_state_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The argv (subcommand + flags, no program name) of the detached backstop
/// `cs tackle` arms for the molecule whose state lives in `mol_state_dir`.
///
/// Lives here, on the library surface, for the same reason
/// [`crate::realized_watcher::watcher_argv`] does: the spawner and the
/// integration test that proves the child survives its caller must invoke the
/// *same* command line, so a renamed flag is a compile failure rather than a
/// silently unarmed backstop.
#[must_use]
pub fn backstop_argv(mol_state_dir: &Path) -> Vec<OsString> {
    vec![
        OsString::from("briefing-backstop"),
        OsString::from("--state-dir"),
        mol_state_dir.as_os_str().to_os_string(),
    ]
}

/// Re-exec this binary as a detached child running `argv`, and return whether
/// the spawn succeeded.
///
/// This is the *whole* mechanism by which the briefing-submit guarantee
/// outlives the dispatcher, so each of the three things it does is load-bearing:
///
/// - **Its own process group.** `cs run` launches `cs tackle` as a child and
///   waits on it. A signal aimed at that job's group — an operator's Ctrl-C, a
///   fleet loop reaping a timed-out dispatcher — would otherwise reach the
///   backstop too. `process_group(0)` puts the child's pgid at its own pid, so
///   `kill(-dispatcher_pgid, …)` cannot name it.
/// - **Never waited on.** The child is orphaned deliberately and reparented to
///   init; nothing holds it open and nothing collects it as a zombie.
/// - **Silenced stdio.** It is detached into a session with no terminal. Its
///   findings go to the record file, which is a thing a human can read later,
///   rather than to a pipe nobody is holding.
///
/// It re-execs `current_exe` rather than resolving `cs` on `PATH` for the same
/// reason [`crate::realized_watcher`] does: the backstop and the dispatcher
/// must never skew versions.
///
/// Shared between `cs tackle` (which arms it in production) and
/// `cs briefing-backstop --detach` (which is the same act, reachable as a
/// command so an integration test can kill the caller and watch the child keep
/// working). One implementation, so the tested detach is the shipped one.
#[must_use]
pub fn detach(argv: &[OsString]) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut command = std::process::Command::new(exe);
    command
        .args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn().is_ok()
}

// ─────────────────────────── the receipt kernel ───────────────────────────

// The kernel moved to `cosmon_transport::briefing_submit` so the RPP API's
// in-process executor runs the same receipt as `cs tackle` and the backstop
// (issue #81). Re-exported here so every existing caller keeps its path.
pub use cosmon_transport::briefing_submit::{
    briefing_submit_deadline, briefing_submit_step, run_briefing_submit_loop,
    BriefingSubmitOutcome, BriefingSubmitStep, BRIEFING_CLEAR_CONFIRMATIONS, BRIEFING_SUBMIT_POLL,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> BriefingPending {
        BriefingPending {
            molecule: "task-20260730-73a1".to_owned(),
            worker: "worker-73a1".to_owned(),
            socket: "cosmon".to_owned(),
            needle: "final line of the brief".to_owned(),
            recorded_at: "2026-07-30T20:00:00Z".to_owned(),
            inband_seconds: 8,
            backstop_outcome: None,
            backstop_ended_at: None,
            backstop_nudges: None,
        }
    }

    /// The whole point of the record: a process that never saw the briefing
    /// reads back exactly what the process that sent it wrote. If this ever
    /// stops round-tripping, the backstop starts nudging against a needle it
    /// invented.
    #[test]
    fn a_record_round_trips_through_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read(dir.path()),
            None,
            "an empty molecule dir has no record"
        );

        write(dir.path(), &record()).expect("write");
        assert_eq!(read(dir.path()), Some(record()));
    }

    /// Clearing the record IS signing the receipt, so it must be the one
    /// observation a later reader can trust — and it must be safe to do twice,
    /// because two backstops can race on the same molecule after a `--force`
    /// re-dispatch.
    #[test]
    fn clearing_is_idempotent_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &record()).expect("write");

        clear(dir.path()).expect("first clear");
        assert_eq!(read(dir.path()), None);
        clear(dir.path()).expect("clearing an absent record is success");
    }

    /// A half-written or hand-mangled record reads as absent rather than
    /// exploding: the backstop's job is to press Enter, not to adjudicate JSON.
    #[test]
    fn an_unparseable_record_reads_as_no_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(record_path(dir.path()), "{ not json").expect("write junk");
        assert_eq!(read(dir.path()), None);
    }

    /// The needle is stored so it can be handed straight back to a composer
    /// scan. That only works because the derivation is idempotent — assert it
    /// here, on the transport function the record and the scan share, so a
    /// change to one cannot silently desync the other.
    #[test]
    fn the_stored_needle_is_its_own_needle() {
        let brief = "# Molecule\n\nline one\n\nfinal line of the brief\n\n";
        let needle = cosmon_transport::tmux::composer_needle(brief).expect("a needle");
        assert_eq!(needle, "final line of the brief");
        assert_eq!(
            cosmon_transport::tmux::composer_needle(needle),
            Some(needle)
        );
    }

    /// The argv shape is the contract between the spawner and the child. Lock
    /// it, or a renamed flag becomes a backstop that exits 2 into `/dev/null`.
    #[test]
    fn the_backstop_argv_names_the_state_dir_by_flag() {
        assert_eq!(
            backstop_argv(Path::new("/s/molecules/task-20260730-73a1")),
            vec![
                OsString::from("briefing-backstop"),
                OsString::from("--state-dir"),
                OsString::from("/s/molecules/task-20260730-73a1"),
            ]
        );
    }
}
