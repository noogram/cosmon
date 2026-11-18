// SPDX-License-Identifier: AGPL-3.0-only

//! Gate slots (ADR-190 D1, noogram/cosmon#214): at most N holders of a pool.
//!
//! Each test names the property it checks and fails when that property is
//! removed. The cross-process crash case (`kill -9` of a holder) lives with the
//! `cs gates slot` verb, in `cosmon-cli/tests/gate_slots_cli.rs`, because the
//! verb is the process that holds a slot in production.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cosmon_filestore::gate_slots::{live_gate_slot_holders_in, try_acquire_gate_slot_in};
use cosmon_filestore::resolve::resolve_state_dir_from;
use cosmon_filestore::FileStore;

/// With N+1 contenders for a pool of N, the number of runs inside the critical
/// section never exceeds N, and N of them really do overlap.
#[test]
fn a_pool_of_n_never_admits_more_than_n_holders_at_once() {
    const N: usize = 3;
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..=N)
        .map(|_| {
            let (store, inside, peak) = (store.clone(), inside.clone(), peak.clone());
            std::thread::spawn(move || {
                let _slot = store.acquire_gate_slot("full_gate", N).unwrap();
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                inside.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(peak.load(Ordering::SeqCst), N, "peak overlap");
}

/// The slot is released when the guard drops: a pool of one admits a second
/// holder once the first guard is gone, and not before.
#[test]
fn a_slot_is_free_again_once_its_guard_drops() {
    let dir = tempfile::tempdir().unwrap();
    let first = try_acquire_gate_slot_in(dir.path(), "p", 1)
        .unwrap()
        .unwrap();
    assert!(try_acquire_gate_slot_in(dir.path(), "p", 1)
        .unwrap()
        .is_none());
    drop(first);
    assert!(eventually(|| try_acquire_gate_slot_in(dir.path(), "p", 1)
        .unwrap()
        .is_some()));
}

/// Holders of pool X never block pool Y.
#[test]
fn pools_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let _x = try_acquire_gate_slot_in(dir.path(), "x", 1)
        .unwrap()
        .unwrap();
    assert!(try_acquire_gate_slot_in(dir.path(), "x", 1)
        .unwrap()
        .is_none());
    assert!(try_acquire_gate_slot_in(dir.path(), "y", 1)
        .unwrap()
        .is_some());
}

/// Poll `cond` for up to five seconds.
///
/// A sibling test in the same process may `fork` (to spawn a subprocess) while
/// a slot file is open; the child holds a copy of the descriptor, and so of the
/// lock, until it `exec`s. A release is therefore visible to a probe within
/// milliseconds, not instantly, under load.
fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// The live-holder count is the kernel's answer: it counts a held slot and
/// stops counting it when the guard drops.
#[test]
fn live_holders_counts_held_slots_only() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(live_gate_slot_holders_in(dir.path(), "p").unwrap(), 0);
    let a = try_acquire_gate_slot_in(dir.path(), "p", 2)
        .unwrap()
        .unwrap();
    let b = try_acquire_gate_slot_in(dir.path(), "p", 2)
        .unwrap()
        .unwrap();
    assert_eq!(live_gate_slot_holders_in(dir.path(), "p").unwrap(), 2);
    drop(a);
    assert!(eventually(|| live_gate_slot_holders_in(dir.path(), "p")
        .unwrap()
        == 1));
    drop(b);
    assert!(eventually(|| live_gate_slot_holders_in(dir.path(), "p")
        .unwrap()
        == 0));
}

/// A pool name comes from configuration and must not escape `gate-slots/`.
#[test]
fn a_pool_name_cannot_escape_the_slot_directory() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["", ".", "..", "../x", "a/b"] {
        assert!(
            try_acquire_gate_slot_in(dir.path(), bad, 1).is_err(),
            "{bad:?}"
        );
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Two worktrees of one galaxy share one set of slot files: with N=1 the
/// second never gets a slot while the first holds one. A per-worktree state
/// directory would give each its own slot and fail this.
#[test]
fn two_worktrees_of_one_galaxy_share_one_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(main.join(".cosmon/state")).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    // A tracked file inside `.cosmon/` makes each worktree carry its own
    // `.cosmon/`, as in production, so resolution must redirect to the main one.
    std::fs::write(main.join(".cosmon/config.toml"), "").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let (wa, wb) = (tmp.path().join("wa"), tmp.path().join("wb"));
    git(
        &main,
        &["worktree", "add", "-q", wa.to_str().unwrap(), "-b", "a"],
    );
    git(
        &main,
        &["worktree", "add", "-q", wb.to_str().unwrap(), "-b", "b"],
    );

    let (state_a, state_b) = (resolve_state_dir_from(&wa), resolve_state_dir_from(&wb));
    let first = FileStore::new(state_a)
        .try_acquire_gate_slot("full_gate", 1)
        .unwrap();
    assert!(first.is_some());
    let second = FileStore::new(state_b)
        .try_acquire_gate_slot("full_gate", 1)
        .unwrap();
    assert!(second.is_none(), "worktree B got a slot of its own");
}
