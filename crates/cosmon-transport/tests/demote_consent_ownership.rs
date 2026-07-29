// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #20, the last door — the startup consent cosmon writes as root and
//! never hands to the worker.
//!
//! **The path this file exercises is dormant.** As of 2026-07-28 a root
//! dispatcher is refused outright (ADR-166): the grant a demotion needs — the
//! repository's shared object store and shared `refs/heads` — is authority over
//! every sibling molecule. These tests still measure the repair and the judge,
//! entered explicitly through `dormant_provision`, because that machinery is
//! the substrate of the bounded per-worker lifecycle and the evidence the
//! refusal rests on. They no longer claim a live dispatch reaches it.
//!
//! # What was measured
//!
//! Two arms in the tester's container image (Claude Code 2.1.220, **no
//! credential involved anywhere**), differing in exactly one property — who
//! owns `.claude.json` — with the containing directory worker-owned in both:
//!
//! ```text
//! owned 10001:10001, mode 600 → after: {hasCompletedOnboarding: true, projects: 1}  pane: "Welcome back!"
//! owned root:root,   mode 600 → after: {hasCompletedOnboarding: null, projects: 0}  pane: "Let's get started."
//! ```
//!
//! The second is not an `EACCES` the worker reports. Claude Code reads the
//! unreadable file as a *first run*, replaces it wholesale, and renders the
//! onboarding wizard nobody is there to answer. `settings.json` survives intact
//! only because Claude Code never rewrites it — which is what made the failure
//! look selective in the field report.
//!
//! # The defect these tests freeze
//!
//! The provisioning step judged three resources and repaired two.
//! `config_home` was judged and never chowned, and the judgement passed anyway
//! because it stats the *directory* — worker-owned, hence readable and writable
//! — and never looked at the file inside it. A gate reporting green over a
//! broken state.
//!
//! # Why one of the two is `#[ignore]`d
//!
//! The ownership half can only exist for a root dispatcher: only root creates
//! root-owned files and only root can hand them away. That test is gated on
//! being root and **fails loudly** as a non-root user rather than passing
//! vacuously — same discipline as the sibling `demote_worktree_ownership`
//! suite:
//!
//! ```text
//! cargo test -p cosmon-transport --test demote_consent_ownership -- --ignored --nocapture
//! ```
//!
//! The judge half needs no privilege and runs everywhere.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use cosmon_core::root_spawn_policy::{
    enforce_demote_provisioning, DemoteResource, RootRefusalReason, RootSpawnDecision,
};
use cosmon_transport::demote_provisioning::{provision_demote_resources, DemoteResources};
use tempfile::TempDir;

/// Drive the demote arm — repair, then judge — with the decision supplied
/// rather than decided.
///
/// `decide_root_spawn` refuses every root dispatch now: making a demotion work
/// means handing the worker the repository's shared object store and shared
/// `refs/heads`, which is authority over every sibling molecule. So
/// `provision_and_decide_root_spawn(0, Some(uid), …)` returns a refusal without
/// touching a file, and a test that kept calling it would assert "a refusal
/// refuses" while believing it measured the provisioning.
///
/// This composes the same two halves that funnel's demote arm composes, so what
/// the tests here measure is unchanged. What they no longer claim is that a live
/// root dispatch gets here — `a_root_dispatch_refuses_without_touching_the_filesystem`
/// in `demote_provisioning.rs` pins the opposite.
fn dormant_provision(to_uid: u32, resources: &DemoteResources) -> RootSpawnDecision {
    enforce_demote_provisioning(
        RootSpawnDecision::Demote { to_uid },
        &provision_demote_resources(to_uid, resources),
    )
}

/// The demote target the tester's container used, and cosmon's default.
const TARGET: u32 = cosmon_core::root_spawn_policy::CONVENTIONAL_WORKER_UID;

/// Can `uid` actually read `path`, asked of the **kernel** rather than of
/// cosmon's own mode arithmetic?
///
/// `[ -r file ]` is `access(2)` performed by a process running as that uid: it
/// answers the exact question the worker will ask, walks the whole path chain,
/// and — the reason it is used here rather than a read — never puts a byte of
/// the file anywhere. Asserting with `path_usable_by_uid` instead would be
/// asserting the judge against itself.
fn readable_by_uid(path: &Path, uid: u32) -> bool {
    use std::os::unix::process::CommandExt as _;

    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"[ -r "$1" ]"#)
        .arg("sh")
        .arg(path)
        .uid(uid)
        .gid(uid)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A worktree and a state dir the target can already use, so the only thing any
/// verdict below can be about is the consent files.
fn open_scratch() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    // Rooted in `/tmp`: the per-user temp dir is 0700 on macOS, under which
    // every path is unreachable by a foreign uid and every verdict would name
    // whichever resource is probed first.
    let tmp = TempDir::new_in("/tmp").expect("scratch");
    let worktree = tmp.path().join("worktree");
    let state = tmp.path().join(".cosmon");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::create_dir_all(&state).expect("state");
    for d in [tmp.path(), worktree.as_path(), state.as_path()] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o777)).expect("open up");
    }
    (tmp, worktree, state)
}

/// Hermetic — the **judge** half.
///
/// A consent file the demote target cannot read must refuse the dispatch, even
/// though the directory holding it is perfectly usable. That gap is the whole
/// defect: before this, the checks stopped at the directory and answered yes.
///
/// The target is the running uid, so the repair is a no-op and the only thing
/// under test is what the judge looks at. The mode is 0000 rather than a
/// foreign owner because a non-root process cannot create the latter — and a
/// file whose owner cannot read it is the same question the kernel asks.
#[test]
fn a_consent_file_the_target_cannot_read_refuses_the_demote() {
    let (tmp, worktree, state) = open_scratch();
    let me = nix::unistd::Uid::effective().as_raw();

    let config_home = tmp.path().join("dot-claude");
    std::fs::create_dir_all(&config_home).expect("config home");
    std::fs::set_permissions(&config_home, std::fs::Permissions::from_mode(0o700))
        .expect("a worker-owned, perfectly usable config home");
    let claude_json = config_home.join(".claude.json");
    let settings_json = config_home.join("settings.json");
    std::fs::write(&claude_json, b"{}").expect("the pre-grant's config file");
    std::fs::write(&settings_json, b"{}").expect("the pre-grant's settings file");
    // Exactly the measured arm, expressed with the one lever a non-root test
    // has: the file is there, inside a usable directory, and unopenable.
    std::fs::set_permissions(&claude_json, std::fs::Permissions::from_mode(0o000))
        .expect("close the file");

    assert!(
        !readable_by_uid(&claude_json, me),
        "precondition: the uid that will run the worker cannot read .claude.json",
    );

    let decision = dormant_provision(
        me,
        &DemoteResources::for_dispatch(
            &worktree,
            vec![state],
            Some(config_home.clone()),
            vec![claude_json.clone(), settings_json],
            None,
        ),
    );

    match decision {
        RootSpawnDecision::Refuse {
            reason:
                RootRefusalReason::UnprovisionedTarget {
                    uid,
                    resource,
                    ref path,
                },
        } => {
            assert_eq!(uid, me);
            assert_eq!(
                resource,
                DemoteResource::ConsentFile,
                "the refusal must name the FILE, not the directory holding it: {path}",
            );
            assert!(path.ends_with(".claude.json"), "must name the path: {path}");
        }
        other => panic!(
            "an unreadable .claude.json must refuse — a worker spawned anyway \
             replaces it and re-opens the onboarding wizard; got {other:?}",
        ),
    }

    // Restore so TempDir can clean up.
    std::fs::set_permissions(&claude_json, std::fs::Permissions::from_mode(0o600)).expect("reopen");
}

/// Hermetic — the check must not cry wolf.
///
/// The same shape with readable consent files still demotes. A gate that
/// refused here would ground every correctly-provisioned container dispatch,
/// which is the mirror image of the bug and no better than it.
#[test]
fn readable_consent_files_still_demote() {
    let (tmp, worktree, state) = open_scratch();
    let me = nix::unistd::Uid::effective().as_raw();

    let config_home = tmp.path().join("dot-claude");
    std::fs::create_dir_all(&config_home).expect("config home");
    let claude_json = config_home.join(".claude.json");
    std::fs::write(&claude_json, b"{}").expect("config file");
    std::fs::set_permissions(&claude_json, std::fs::Permissions::from_mode(0o600))
        .expect("the mode the pre-grant leaves");

    let decision = dormant_provision(
        me,
        &DemoteResources::for_dispatch(
            &worktree,
            vec![state],
            Some(config_home),
            vec![claude_json, tmp.path().join("dot-claude/settings.json")],
            None,
        ),
    );
    assert_eq!(decision, RootSpawnDecision::Demote { to_uid: me });
}

/// Root-only — the **repair** half, and the property that actually broke.
///
/// After provisioning, the demote target can READ the consent files. Asserted
/// by asking the kernel as that uid, not by asserting that a chown was called:
/// a test that checked the call, or that checked the directory, would pass
/// against the bug.
///
/// The `.credentials.json` sitting in the same directory is left alone on
/// purpose, and that is asserted too. Cosmon takes ownership of what it wrote
/// and of nothing else; the config home can be an operator-supplied directory
/// holding an operator's own credential, and it is never opened here.
#[test]
#[ignore = "requires root (only root can create root-owned files and chown them away); \
            run with `cargo test -p cosmon-transport --test demote_consent_ownership -- --ignored`"]
fn provisioning_leaves_the_consent_files_readable_by_the_demote_target() {
    assert_eq!(
        nix::unistd::Uid::effective().as_raw(),
        0,
        "this test reproduces a root-dispatcher bug and proves nothing as a \
         non-root user — run it inside a root container, do not weaken it",
    );

    let (tmp, worktree, state) = open_scratch();

    // The measured shape: the config home belongs to the worker, and the files
    // cosmon just pre-granted into it belong to root, mode 600.
    let config_home = tmp.path().join("dot-claude");
    std::fs::create_dir_all(&config_home).expect("config home");
    let claude_json = config_home.join(".claude.json");
    let settings_json = config_home.join("settings.json");
    let credentials = config_home.join(".credentials.json");
    for f in [&claude_json, &settings_json, &credentials] {
        std::fs::write(f, b"{}").expect("write");
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600)).expect("mode 600");
    }
    std::os::unix::fs::lchown(&config_home, Some(TARGET), Some(TARGET)).expect("worker-owned dir");
    for f in [&claude_json, &settings_json, &credentials] {
        assert_eq!(
            std::fs::metadata(f).expect("stat").uid(),
            0,
            "precondition: the root dispatcher wrote {} as root",
            f.display(),
        );
    }
    assert!(
        !readable_by_uid(&claude_json, TARGET),
        "precondition: this is the arm that reproduced `Let's get started.`",
    );

    let decision = dormant_provision(
        TARGET,
        &DemoteResources::for_dispatch(
            &worktree,
            vec![state],
            Some(config_home.clone()),
            vec![claude_json.clone(), settings_json.clone()],
            None,
        ),
    );
    assert_eq!(
        decision,
        RootSpawnDecision::Demote { to_uid: TARGET },
        "a dispatch whose consent files cosmon itself wrote must not refuse",
    );

    // The property that broke, asked of the kernel as the worker's own uid.
    for f in [&claude_json, &settings_json] {
        assert!(
            readable_by_uid(f, TARGET),
            "after provisioning the worker must be able to read {} — it cannot \
             report failing to, it silently replaces the file",
            f.display(),
        );
    }

    // And the line cosmon draws: what it did not write, it does not take.
    assert_eq!(
        std::fs::metadata(&credentials).expect("stat").uid(),
        0,
        "a credential cosmon never authored is not cosmon's to reassign",
    );
}
