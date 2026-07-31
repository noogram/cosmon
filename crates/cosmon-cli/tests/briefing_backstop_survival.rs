// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test — the durable briefing-submit backstop keeps pressing after
//! the process that armed it is dead (COSMON #26-B).
//!
//! # Why this cannot be a unit test
//!
//! The claim under test is not about a decision, it is about a *process
//! lifetime*. The unit tests in `cosmon_cli::briefing_backstop` prove the
//! receipt kernel; they would pass unchanged against the previous design, where
//! the residual patience lived in a thread that died with `cs tackle` and
//! nudged nothing. That design failed for a reason no in-process test can
//! observe: something killed the parent.
//!
//! So this file kills the parent. It arms the backstop the way `cs tackle` does
//! — through [`cosmon_cli::briefing_backstop::detach`], the one implementation
//! both paths use — then SIGKILLs the arming process *and its entire process
//! group*, verifies the group is empty, and only then waits for the work to
//! happen anyway.
//!
//! # The rig
//!
//! A real tmux session standing in for a stuck Claude worker: its pane shows a
//! glyph composer holding a needle (`❯ COSMON-BACKSTOP-NEEDLE-…`) and blocks on
//! `read`, which is exactly the shape of the field incident — a briefing pasted
//! into the composer whose submitting Enter was swallowed. One Enter unblocks
//! it; it then writes a marker file and clears the screen, so the composer
//! reads `Clear` and the backstop can sign its receipt.
//!
//! # What the assertions establish
//!
//! 1. **The record is durable.** It is on disk, written by one process and read
//!    by another that shares nothing with it but a path.
//! 2. **The caller is really gone.** `kill -0` on its process group answers
//!    ESRCH — so the backstop cannot be hiding in it.
//! 3. **The retry still lands.** The marker file appears: an Enter reached the
//!    pane after the kill.
//! 4. **The receipt is signed.** The record file is removed — which the
//!    backstop only ever does on two consecutive `Clear` readings, i.e. several
//!    polls *after* the kill. That timing is what makes this a survival proof
//!    rather than a race the backstop happened to win.
//!
//! # The test has teeth (verified, 2026-07-30)
//!
//! A test whose subject is "this does not die" passes trivially if the kill
//! never bites, so the kill was checked by breaking the thing it tests:
//! deleting `process_group(0)` from
//! [`cosmon_cli::briefing_backstop::detach`] — leaving the backstop an ordinary
//! child in the caller's group — turns this into `timed out after 60s waiting
//! for: the submit keystroke to reach the pane`. The group kill really does
//! reach an undetached backstop, and the green above is therefore about
//! survival and not about the assertions being unfalsifiable.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Upper bound on any single wait here. Generous enough for a loaded runner,
/// short enough that a genuine hang fails the job instead of stalling it.
const PATIENCE: Duration = Duration::from_secs(60);

/// Poll interval handed to the backstop. Far below the production default:
/// this test is about *whether* the pressing continues, not about the cadence,
/// and a fast poll keeps the whole file to a couple of seconds.
const INTERVAL_MS: u64 = 200;

/// Is tmux usable here? The rig needs a real terminal multiplexer — there is no
/// honest way to fake "a pane that swallowed a keystroke". A bare container
/// without tmux reports the skip rather than a green it did not earn.
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
        .expect("tmux must be runnable")
}

/// Wait for `cond`, or fail with `what` once [`PATIENCE`] is spent.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < PATIENCE {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out after {PATIENCE:?} waiting for: {what}");
}

/// Does any process still live in group `pgid`? `kill -0` on a negative pid
/// asks exactly that, and answers non-zero (ESRCH) when the group is empty.
fn process_group_alive(pgid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(format!("-{pgid}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn the_backstop_keeps_pressing_after_its_caller_is_killed() {
    if !tmux_available() {
        eprintln!("SKIP the_backstop_keeps_pressing_after_its_caller_is_killed: tmux not found");
        return;
    }

    let dir = tempfile::TempDir::new().expect("tempdir");
    let state_dir = dir.path().join("molecule");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let marker = dir.path().join("submitted.marker");

    // Names unique to this run, so a parallel test or a leftover server on the
    // developer's machine cannot be mistaken for our rig.
    let stamp = std::process::id();
    let socket = format!("cosmon-test-backstop-{stamp}");
    let worker = format!("cosmon-backstop-worker-{stamp}");
    let needle = format!("COSMON-BACKSTOP-NEEDLE-{stamp}");

    // The stuck worker: a glyph composer holding the needle, blocked on `read`.
    // One Enter releases it; it then records that the keystroke arrived and
    // wipes the screen (ANSI erase + home, so no `clear` binary is required),
    // which is what turns the composer reading from `Pending` into `Clear`.
    let pane_script = format!(
        "printf '\\342\\235\\257 {needle}'; read -r _; \
         : > {marker}; printf '\\033[2J\\033[Hdone\\n'; sleep 300",
        marker = marker.display()
    );
    let out = tmux(
        &socket,
        &["new-session", "-d", "-s", &worker, "sh", "-c", &pane_script],
    );
    assert!(
        out.status.success(),
        "tmux new-session failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Property 1 — the durable record. Written here exactly as `cs tackle`
    // writes it when its in-band window closes on a still-pending composer.
    let record = cosmon_cli::briefing_backstop::BriefingPending {
        molecule: "task-20260730-73a1".to_owned(),
        worker: worker.clone(),
        socket: socket.clone(),
        needle: needle.clone(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        inband_seconds: 8,
        backstop_outcome: None,
        backstop_ended_at: None,
        backstop_nudges: None,
    };
    cosmon_cli::briefing_backstop::write(&state_dir, &record).expect("persist the record");
    assert!(
        cosmon_cli::briefing_backstop::read(&state_dir).is_some(),
        "the record must be on disk before anything is spawned — it is the \
         evidence that survives even a failed spawn"
    );

    // The caller. It arms the backstop through the production detach and exits.
    // Its own process group is its own pid (`process_group(0)`), which is what
    // lets the kill below name the caller's world without touching this test's.
    let mut caller = Command::new(env!("CARGO_BIN_EXE_cs"));
    caller
        .arg("briefing-backstop")
        .arg("--detach")
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--interval-ms")
        .arg(INTERVAL_MS.to_string())
        .arg("--budget-secs")
        .arg("60")
        // A cwd inside the fixture, not the repo: `cs` walks up looking for a
        // galaxy at startup, and an aged checkout makes that scan expensive.
        .current_dir(dir.path());
    {
        use std::os::unix::process::CommandExt as _;
        caller.process_group(0);
    }
    let mut caller = caller.spawn().expect("spawn the arming caller");
    let caller_pgid = caller.id();

    // Let the caller finish arming — WITHOUT reaping it.
    //
    // # Why not `caller.wait()` here (the CI-runner hazard)
    //
    // `caller_pgid` is the caller's pid, and a pid belongs to a process only
    // while the kernel is still holding it. Reaping releases it for reuse
    // **immediately**, so a `kill -KILL -<pgid>` issued after `wait()` names a
    // process group that may already belong to something else. On a developer's
    // machine that window is too narrow to hit. On a CI runner churning through
    // hundreds of test binaries it is not, and the signal then lands on whatever
    // won the pid lottery — at the limit, the harness running this very test,
    // which dies without a verdict and without uploading a log.
    //
    // `waitpid(WNOWAIT)` waits for the exit and leaves the zombie in place. A
    // zombie still owns its pid: the kernel cannot hand it to anyone until it is
    // collected. So between here and the reap below, `caller_pgid` means exactly
    // one thing, and the kill has a well-defined target.
    //
    // The ordering matters in the other direction too: the caller has to have
    // ARMED the backstop before anything kills its group, which is why this
    // waits for its exit rather than firing straight after `spawn`.
    // `waitid` and not `waitpid`: `WNOWAIT` is a `waitid` flag, and `waitpid`
    // answers EINVAL for it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `caller_pgid` is our own live child's pid, `info` is a valid
    // writable `siginfo_t`, and `WNOWAIT` leaves the child collectable by the
    // `caller.wait()` below — so `Child`'s own reaping contract is preserved.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            caller_pgid as libc::id_t,
            &raw mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(
        rc,
        0,
        "waiting for the arming caller failed: {}",
        std::io::Error::last_os_error()
    );

    // Property 2 — the caller is really dead, and so is anything that stayed in
    // its process group. If the backstop had been spawned as an ordinary child
    // it would be in this group and would die right here, which is precisely
    // the failure mode the thread-based attempt had.
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{caller_pgid}"))
        .output();

    // Now the pid may be released: nothing reads it again.
    let status = caller.wait().expect("the caller returns immediately");
    assert!(status.success(), "arming the backstop failed: {status}");

    wait_for("the caller's process group to empty", || {
        !process_group_alive(caller_pgid)
    });

    // Property 3 — the retry still lands. Nothing but the detached backstop can
    // press this Enter: the caller is gone and the test never touches the pane.
    wait_for("the submit keystroke to reach the pane", || marker.exists());

    // Property 4 — the receipt. Removing the record takes two consecutive
    // `Clear` readings, so this can only become true several polls after the
    // kill: the backstop was demonstrably still running then.
    wait_for(
        "the durable record to be cleared by a delivery receipt",
        || cosmon_cli::briefing_backstop::read(&state_dir).is_none(),
    );
    assert!(
        !Path::new(&cosmon_cli::briefing_backstop::record_path(&state_dir)).exists(),
        "a signed receipt must leave no pending record behind"
    );

    let _ = tmux(&socket, &["kill-server"]);
}

/// The other half of the contract: a backstop that runs out of patience leaves
/// the record behind, annotated. Nothing here dies — the point is what the
/// on-disk evidence says when the pressing genuinely fails.
///
/// The rig is a composer that never clears: the pane holds the needle and never
/// reads, so every Enter lands on a process that ignores it. This is the
/// twenty-minute stall in miniature, with the budget dialled down to two
/// seconds so the test costs seconds rather than a coffee break.
#[test]
fn a_backstop_that_gives_up_leaves_an_annotated_record() {
    if !tmux_available() {
        eprintln!("SKIP a_backstop_that_gives_up_leaves_an_annotated_record: tmux not found");
        return;
    }

    let dir = tempfile::TempDir::new().expect("tempdir");
    let state_dir = dir.path().join("molecule");
    std::fs::create_dir_all(&state_dir).expect("state dir");

    let stamp = std::process::id();
    let socket = format!("cosmon-test-backstop-stuck-{stamp}");
    let worker = format!("cosmon-backstop-stuck-{stamp}");
    let needle = format!("COSMON-STUCK-NEEDLE-{stamp}");

    // Never reads, so the needle stays on screen no matter how many Enters
    // arrive — a composer too busy to accept the submit, forever.
    let pane_script = format!("printf '\\342\\235\\257 {needle}'; sleep 300");
    let out = tmux(
        &socket,
        &["new-session", "-d", "-s", &worker, "sh", "-c", &pane_script],
    );
    assert!(
        out.status.success(),
        "tmux new-session failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let record = cosmon_cli::briefing_backstop::BriefingPending {
        molecule: "task-20260730-73a1".to_owned(),
        worker: worker.clone(),
        socket: socket.clone(),
        needle,
        recorded_at: chrono::Utc::now().to_rfc3339(),
        inband_seconds: 8,
        backstop_outcome: None,
        backstop_ended_at: None,
        backstop_nudges: None,
    };
    cosmon_cli::briefing_backstop::write(&state_dir, &record).expect("persist the record");

    let status = Command::new(env!("CARGO_BIN_EXE_cs"))
        .arg("briefing-backstop")
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--interval-ms")
        .arg(INTERVAL_MS.to_string())
        .arg("--budget-secs")
        .arg("2")
        .current_dir(dir.path())
        .status()
        .expect("run the backstop in the foreground");
    assert!(status.success(), "the backstop must exit cleanly: {status}");

    let back = cosmon_cli::briefing_backstop::read(&state_dir)
        .expect("a composer that never cleared must leave its record behind");
    assert_eq!(
        back.backstop_outcome.as_deref(),
        Some("stuck-pasted"),
        "the give-up must be typed on disk, not implied by the file's presence"
    );
    assert!(
        back.backstop_nudges.unwrap_or(0) > 1,
        "the durable pass must press repeatedly, not once: {:?}",
        back.backstop_nudges
    );

    let _ = tmux(&socket, &["kill-server"]);
}
