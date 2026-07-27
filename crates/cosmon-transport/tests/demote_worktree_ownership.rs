// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #20 — the worktree-ownership catch-22 on the demotion path.
//!
//! # The bug this freezes
//!
//! An external tester ran a signed v0.3.0 inside a root Docker container with
//! `COSMON_WORKER_UID=10001`, nucleated a task, and tackled it. `cs tackle`
//! created the worktree **as root** — so root-owned — and then its own
//! preflight refused:
//!
//! ```text
//! cs: cs tackle: cannot provision uid 10001: worktree `/work/.worktrees/task-…`
//!     is not usable by it … chown the worktree to the uid before tackling …
//! worktree owner: 0
//! ```
//!
//! The advice was impossible to follow: `cs tackle` is what *creates* the
//! worktree, so for a freshly nucleated molecule there is no "before". The
//! guard was right and fail-closed; the **order of operations** was wrong.
//!
//! # Why the load-bearing test is `#[ignore]`d
//!
//! The bug only exists for a dispatcher that is root, because only a root
//! dispatcher creates root-owned paths and only root can hand them away. A
//! non-root test cannot create the initial condition (a path owned by a uid it
//! is not) nor perform the repair (`chown` to a foreign uid is `EPERM`). Rather
//! than hollow the test out into something that passes without proving
//! anything, it is gated on being root and skipped by default:
//!
//! ```text
//! # inside a root container (the tester's shape):
//! cargo test -p cosmon-transport --test demote_worktree_ownership -- --ignored --nocapture
//! ```
//!
//! Run as a non-root user it **fails loudly** instead of passing vacuously —
//! an ignored test that silently self-neuters is the failure mode this comment
//! exists to prevent.
//!
//! The two hermetic assertions below run everywhere and pin the parts that do
//! not need privilege: the remedy text no longer advises an impossible gesture,
//! and a path that does not exist is not an ownership-transfer error.

use std::os::unix::fs::MetadataExt as _;

use cosmon_core::root_spawn_policy::{DemoteResource, RootSpawnDecision};
use cosmon_transport::demote_provisioning::{
    chown_tree_to_uid, provision_and_decide_root_spawn, DemoteResources,
};
use tempfile::TempDir;

/// The demote target the tester's container used, and cosmon's default.
const TARGET: u32 = cosmon_core::root_spawn_policy::CONVENTIONAL_WORKER_UID;

/// A freshly nucleated molecule, tackled under demotion, gets a worktree AND a
/// molecule state dir owned by the demote target — and the dispatch proceeds.
///
/// Before the fix this returned
/// `Refuse { UnprovisionedTarget { resource: Worktree } }`, because the checks
/// ran against paths the root dispatcher had just created for itself. That is
/// the *right* red: the assertion that fails first is the decision, for the
/// exact reason the tester reported.
#[test]
#[ignore = "requires root (only root can create root-owned paths and chown them away); \
            run with `cargo test -p cosmon-transport --test demote_worktree_ownership -- --ignored`"]
fn a_fresh_worktree_is_handed_to_the_demote_target_and_the_dispatch_proceeds() {
    assert_eq!(
        nix::unistd::Uid::effective().as_raw(),
        0,
        "this test reproduces a root-dispatcher bug and proves nothing as a \
         non-root user — run it inside a root container, do not weaken it",
    );

    let tmp = TempDir::new().unwrap();
    // Exactly what `cs tackle` leaves behind when it runs as root: a worktree
    // and an out-of-worktree state tree, both freshly created, both uid 0.
    let worktree = tmp.path().join(".worktrees/task-20260725-a1fb");
    let state = tmp.path().join(".cosmon/state/fleets/default");
    let mol_state = state.join("molecules/task-20260725-a1fb");
    std::fs::create_dir_all(worktree.join("crates/cosmon-cli")).unwrap();
    std::fs::create_dir_all(&mol_state).unwrap();
    std::fs::write(mol_state.join("state.json"), b"{}").unwrap();
    for p in [&worktree, &state, &mol_state] {
        assert_eq!(
            std::fs::metadata(p).unwrap().uid(),
            0,
            "precondition: the root dispatcher created {} root-owned",
            p.display(),
        );
    }

    let decision = provision_and_decide_root_spawn(
        0,
        Some(TARGET),
        &DemoteResources {
            // Not declared: root's own `/root/.claude` is not cosmon's to give
            // away, and its provisioning is a separate operator gesture.
            config_home: None,
            worktree: worktree.clone(),
            state_dirs: vec![state.clone(), mol_state.clone()],
            consent_files: vec![],
        },
    );

    assert_eq!(
        decision,
        RootSpawnDecision::Demote { to_uid: TARGET },
        "a freshly created worktree must not refuse its own dispatch",
    );

    // The transfer is real, and it is recursive: the worker writes files deep
    // inside both trees, not only at their roots.
    for p in [
        &worktree,
        &worktree.join("crates/cosmon-cli"),
        &state,
        &mol_state,
        &mol_state.join("state.json"),
    ] {
        assert_eq!(
            std::fs::metadata(p).unwrap().uid(),
            TARGET,
            "{} must belong to the demoted worker",
            p.display(),
        );
    }
}

/// The guard is still fail-closed after the reordering: a path the transfer
/// cannot repair still refuses, typed, before any worker exists.
///
/// Modelled with a state dir on which the chown lands but the mode does not
/// grant write — the shape of a read-only mount or a restrictive ACL. Cosmon
/// must not treat "I attempted a chown" as "the uid can now write".
#[test]
#[ignore = "requires root; run with `cargo test -p cosmon-transport --test demote_worktree_ownership -- --ignored`"]
fn a_transfer_that_does_not_take_still_refuses() {
    use std::os::unix::fs::PermissionsExt as _;

    assert_eq!(
        nix::unistd::Uid::effective().as_raw(),
        0,
        "root-only, by construction — see the sibling test",
    );

    let tmp = TempDir::new().unwrap();
    let worktree = tmp.path().join("wt");
    let state = tmp.path().join(".cosmon");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    // Reachable and readable, but not writable by anyone: the chown will
    // succeed and the uid still cannot perform its `cs evolve` write.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();

    let decision = provision_and_decide_root_spawn(
        0,
        Some(TARGET),
        &DemoteResources {
            config_home: None,
            worktree: worktree.clone(),
            state_dirs: vec![state.clone()],
            consent_files: vec![],
        },
    );

    match decision {
        RootSpawnDecision::Refuse {
            reason:
                cosmon_core::root_spawn_policy::RootRefusalReason::UnprovisionedTarget {
                    uid,
                    resource,
                    ref path,
                },
        } => {
            assert_eq!(uid, TARGET);
            assert_eq!(resource, DemoteResource::StateDir);
            assert!(path.contains(".cosmon"), "must name the path: {path}");
        }
        other => panic!("a transfer that did not take must still refuse, got {other:?}"),
    }

    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Hermetic: the remedy an operator reads is one they can actually perform.
///
/// This is the half of the fix that needs no privilege to pin. Before it, the
/// worktree remedy read *"chown the worktree to the uid before tackling"* — an
/// instruction with no "before" for a freshly nucleated molecule, which is what
/// turned a good fail-closed guard into a catch-22.
#[test]
fn the_worktree_remedy_never_asks_for_a_gesture_before_tackling() {
    let remedy = DemoteResource::Worktree.remedy();
    assert!(
        !remedy.contains("before tackling"),
        "`cs tackle` creates the worktree; there is no before: {remedy}",
    );
    assert!(
        remedy.contains("chown"),
        "the remedy must still say what cosmon did on the operator's behalf: {remedy}",
    );
}

/// Hermetic: a path that is not there yet is not an ownership failure.
///
/// The state dirs a caller declares need not all exist — the checks fall back
/// to the nearest existing ancestor. The transfer must agree, or the demote
/// path would surface an `ENOENT` where there is no problem to report.
#[test]
fn transferring_a_missing_path_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("does/not/exist");
    chown_tree_to_uid(&missing, nix::unistd::Uid::effective().as_raw())
        .expect("a missing path is a no-op, not an error");
}
