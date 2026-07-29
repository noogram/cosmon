// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #20, fourth instance — how *much* git authority a demoted worker gets.
//!
//! # The defect this freezes
//!
//! The third instance fixed a worker that could edit files and not record them:
//! a linked worktree commits through plumbing that is not inside it, so the
//! plumbing was added to the transfer. The list it was added as was
//! `[gitdir, commondir]`, handed to a **recursive** chown — and the common dir
//! is the repository's `.git`. One demoted worker therefore came to own
//! `config`, `hooks/`, every branch under `refs/`, and every *other* live
//! worktree's plumbing under `worktrees/`.
//!
//! Two of those are worse than data. `hooks/` is executed by whoever runs git
//! next, and the dispatcher runs git next, as root, when `cs done` merges the
//! branch back. `config` names commands too — `core.hooksPath`, `alias.*`,
//! `core.fsmonitor`. A bounded per-molecule commit capability had become a
//! standing invitation to run code as the dispatcher.
//!
//! # What is proven here, and by which test
//!
//! - **Sufficiency** — the three subpaths cosmon now transfers are *enough* to
//!   commit. Proven hermetically, by mode bits, on any account:
//!   [`a_commit_needs_only_the_shared_subpaths_this_module_names`]. This is the
//!   half that would otherwise be an argument about git's internals, and it is
//!   the half that goes red first if git grows a fourth write.
//! - **Necessity of the narrowing** — the transfer leaves the dispatcher's
//!   `hooks/`, `config` and the sibling worktrees alone, and a worker running
//!   as a genuinely different uid can commit its branch and cannot touch them.
//!   That needs two uids, so it needs root, so it is `#[ignore]`d — and, like
//!   the sibling demote suites, it **fails loudly** off its precondition rather
//!   than passing vacuously, on every platform.
//! - **The residue** — what the granted paths still permit, as opposed to what
//!   the withheld ones refuse. Every other test here asks the withheld
//!   question; asking only that one is how a suite reports `ok` beside a HIGH
//!   finding. See the section below and
//!   [`the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete`].
//!
//! # What is *not* proven, because it is not true
//!
//! **The demoted worker still holds repository-wide destructive authority.**
//! Two grants carry it, and both are measured — not argued — by
//! [`the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete`]:
//!
//! - **`refs/heads`** — a worker can rewrite or delete a **sibling branch
//!   ref**. The root is transferred as a directory because a loose-ref store
//!   creates `<branch>.lock` beside `<branch>`, and a cosmon branch is
//!   `feat/task-…`, so that directory holds the other molecules' branches.
//!   Ownership cannot separate them: git's files backend has no per-ref
//!   delegation, and a ref moved into `packed-refs` is shadowed by a loose
//!   ref the worker may create. `git update-ref` is plumbing and does not
//!   honour the "checked out in another worktree" guard, so a live sibling
//!   worktree is no protection.
//! - **`objects`** — a worker can **delete or replace any object in the
//!   shared store**, including ones another molecule's history depends on.
//!   Narrowing does not help here either: creating a loose object means
//!   creating the `objects/XX/` fan-out directory, which needs write on
//!   `objects` itself, and owning `objects/XX/` is owning every object in it.
//!
//! Both were reproduced independently in round 2 of `converge-20260727-a302`:
//! in a Linux container at uid 10001 (`sibling_rewrite=SUCCEEDED`,
//! `shared_object_delete=SUCCEEDED`) and, by the mode-bit freeze this module
//! already uses, on an ordinary macOS account at uid 501.
//!
//! The shape that closes it is per-worker ref and object storage — a
//! per-worker repository sharing the object store read-only through
//! `objects/info/alternates`, with `cs done` **fetching** rather than merging
//! in place. That is a different worktree lifecycle, not a tighter `chown`:
//! it changes `cs tackle`'s `git worktree add`, `cleanup_partial_tackle`'s
//! `git worktree remove`, the `cs done` merge, and every place that assumes
//! one repository. It is deliberately not attempted as a patch.
//!
//! Stated here so a later round reads it as a known residue and not as a
//! fresh finding — and, since round 3, backed by a test rather than by this
//! paragraph, because prose does not go red.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_transport::demote_provisioning::git_plumbing_paths;
use tempfile::TempDir;

/// The demote target the tester's container used, and cosmon's default.
const TARGET: u32 = cosmon_core::root_spawn_policy::CONVENTIONAL_WORKER_UID;

/// Run `git` with the arguments given, and fail with its own diagnostics
/// rather than a bare exit code — a red here should say what git said.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

/// A repository with one commit and one linked worktree on `feat/task-…` —
/// the exact shape `cs tackle` leaves behind. Returns `(repo, worktree)`.
/// `root` is canonicalised first: on macOS a tempdir lives under `/var`, which
/// is a symlink to `/private/var`, and git writes the resolved form into its
/// `.git` pointer file. Comparing a derived path against an unresolved one
/// would fail on a difference that is not the one under test.
fn repo_with_linked_worktree(root: &Path) -> (PathBuf, PathBuf) {
    let root = &std::fs::canonicalize(root).expect("canonicalise the test root");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("create repo dir");
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["config", "user.name", "Noogram"]);
    git(&repo, &["config", "user.email", "fleet@noogram.org"]);
    // Deterministic: auto-gc writes `gc.log` in the common dir, which this
    // test deliberately makes unwritable. A worker's commit must not depend on
    // it, and a spurious red here would be about gc, not about the narrowing.
    git(&repo, &["config", "gc.auto", "0"]);
    std::fs::write(repo.join("seed"), b"seed\n").expect("write seed");
    git(&repo, &["add", "seed"]);
    git(&repo, &["commit", "--quiet", "-m", "seed"]);

    let worktree = root.join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feat/task-20260727-659b",
            worktree.to_str().expect("utf-8 worktree path"),
        ],
    );
    (repo, worktree)
}

/// Every entry of `dir`, one level deep.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok).map(|e| e.path()).collect()
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// Prove the freeze took, by trying to defeat it.
///
/// # Why counting was not enough
///
/// This guard used to be `assert!(frozen.len() > 5)`. That counts the paths
/// the walk **visited**: [`set_mode`] discards its `set_permissions` result
/// and [`freeze_all_but`] pushes every path it reaches whether or not the
/// mode actually changed anything. Mode bits do not bind euid 0, so under
/// root the freeze is completely inert and the count is unchanged — the
/// sufficiency half then reports `ok` for a `SHARED_COMMIT_SUBPATHS` that
/// is genuinely broken. Measured on this branch: drop `"refs/heads"` from
/// the granted set at uid 501 → red with a real `EACCES` from git; the same
/// broken narrowing with the freeze neutered → `RC=0`, "ok. 1 passed".
/// That is not a hypothetical bench: `18b820d`'s own message records this
/// suite's validation in a root `rust:1-bookworm` container.
///
/// So measure the effect instead of a proxy for it: pick a frozen directory
/// and try to create a file in it. Root fails here, and so does any other
/// way of not being bound by mode bits — a `CAP_DAC_OVERRIDE` container, a
/// mount with no permission enforcement, a filesystem that quietly ignores
/// `chmod`. A green from this suite now means the freeze was real.
///
/// Returns the complaint rather than panicking, so the caller can thaw
/// first and not leave an undeletable tempdir behind.
fn freeze_verdict(frozen: &[PathBuf]) -> Result<(), String> {
    let Some(dir) = frozen.iter().find(|p| p.is_dir()) else {
        return Err(format!(
            "the walk froze no directory at all, so it cannot have covered a \
             real common dir: {frozen:?}"
        ));
    };
    let probe = dir.join("freeze-probe");
    if std::fs::write(&probe, b"probe\n").is_err() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&probe);
    let euid = nix::unistd::Uid::effective().as_raw();
    Err(format!(
        "the freeze is inert: this process created a file inside {}, which \
         the walk had just frozen to r-x. euid is {euid}{because}. This suite \
         proves sufficiency by mode bits alone, so on this bench it can prove \
         nothing about the narrowing — every `SHARED_COMMIT_SUBPATHS`, \
         including a broken one, would report `ok`. Run it as an ordinary \
         user on a filesystem that enforces modes. It fails here rather than \
         reporting `ok`, because a green that measures nothing is the exact \
         defect this branch exists to police.",
        dir.display(),
        euid = euid,
        because = if euid == 0 {
            " — mode bits do not bind euid 0"
        } else {
            ", so something other than root defeated the freeze: a \
             `CAP_DAC_OVERRIDE` container, a mount that ignores modes, or a \
             `set_mode` that no longer sets anything"
        },
    ))
}

/// Make every path under `root` unwritable **except** what `granted` covers,
/// and return what was frozen so it can be thawed again.
///
/// Three cases, and the middle one is why this is a walk and not a name list:
///
/// - under a granted path — left alone, and not descended into: a granted root
///   is granted whole.
/// - anything else — frozen to `r-x` (dir) or `r--` (file), **and descended
///   into**. Freezing a directory only stops entries being created or removed
///   *in it*; a writable child of a frozen parent is still writable, so
///   stopping at the top level would leave `refs/heads` open while `refs` was
///   frozen and the test would prove nothing about `refs/heads`. This walk is
///   what makes each granted subpath individually necessary.
///
/// Deriving all of this from `granted` is the load-bearing part. Freeze by
/// name and dropping an entry from `SHARED_COMMIT_SUBPATHS` changes nothing
/// the test can see.
fn freeze_all_but(root: &Path, granted: &[PathBuf]) -> Vec<PathBuf> {
    let mut frozen = Vec::new();
    let mut stack = entries(root);
    while let Some(path) = stack.pop() {
        if granted.iter().any(|g| path.starts_with(g)) {
            continue;
        }
        let is_dir = path.is_dir();
        if is_dir {
            stack.extend(entries(&path));
        }
        set_mode(&path, if is_dir { 0o500 } else { 0o400 });
        frozen.push(path);
    }
    frozen
}

/// Sufficiency, measured rather than argued.
///
/// Freezes the repository's common dir to read-only **except** for the three
/// subpaths [`git_plumbing_paths`] now hands over, then makes a real commit in
/// the linked worktree. Ownership plays no part: on a non-root account the mode
/// bits bind the owner too, which is what lets this run on any bench instead of
/// only inside a root container.
///
/// If this goes red, cosmon's list of what a commit writes is wrong, and the
/// red belongs here — not in a container where a worker wedges on `EACCES`
/// with the pane looking alive.
#[test]
fn a_commit_needs_only_the_shared_subpaths_this_module_names() {
    let tmp = TempDir::new().expect("tempdir");
    let (repo, worktree) = repo_with_linked_worktree(tmp.path());
    let common = repo.join(".git");

    let granted = git_plumbing_paths(&worktree);
    assert!(
        granted.contains(&common.join("objects")),
        "precondition: the derivation names the object store: {granted:?}",
    );

    // Freeze the common dir against `granted`, and *only* against `granted` —
    // never against a hand-written list of names. A freeze that names the
    // entries itself stays green when a subpath is dropped from
    // `SHARED_COMMIT_SUBPATHS`, because nothing it froze was ever the dropped
    // one; the first draft of this test did exactly that for two of the three,
    // which is the defect class this whole branch is about.
    let frozen = freeze_all_but(&common, &granted);
    // Coverage: the walk reached most of a real common dir. This is a claim
    // about the *walk*, and it is all a count can ever be.
    assert!(
        frozen.len() > 5,
        "the walk covered too little of the common dir to be measuring the \
         narrowing, got {frozen:?}",
    );
    // Effect: the freeze actually made something unwritable. Measured, not
    // counted — see `freeze_verdict` for why the count above cannot say this.
    let bites = freeze_verdict(&frozen);

    // The commit a worker makes: a new artefact and an edit to a tracked file,
    // staged and then recorded on its own branch. `add` is exercised
    // separately because staging is the write that creates the loose objects,
    // and `commit` is the one that moves the ref.
    std::fs::write(worktree.join("artefact.txt"), b"the worker did its work\n")
        .expect("write artefact");
    std::fs::write(worktree.join("seed"), b"seed, amended\n").expect("edit seed");
    let staged = Command::new("git")
        .current_dir(&worktree)
        .args(["add", "artefact.txt", "seed"])
        .output()
        .expect("git is on PATH");
    let out = Command::new("git")
        .current_dir(&worktree)
        .args(["commit", "--quiet", "-m", "feat: the worker's commit"])
        .output()
        .expect("git is on PATH");

    // Thaw before asserting, so a red does not also leave an undeletable
    // tempdir behind.
    for path in &frozen {
        set_mode(path, 0o700);
    }

    // First, before anything below is allowed to mean anything: the freeze
    // was real. If it was not, the two successes below are the successes of
    // a process nothing was withheld from.
    if let Err(complaint) = bites {
        panic!("{complaint}");
    }

    assert!(
        staged.status.success(),
        "staging needs something outside the three granted subpaths — the \
         narrowing in `SHARED_COMMIT_SUBPATHS` is incomplete:\n{}\n{}",
        String::from_utf8_lossy(&staged.stdout),
        String::from_utf8_lossy(&staged.stderr),
    );
    assert!(
        out.status.success(),
        "a commit needs something outside the three granted subpaths — the \
         narrowing in `SHARED_COMMIT_SUBPATHS` is incomplete:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // And it really recorded: the branch moved, which is the property the
    // third instance of this issue was about.
    let log = git(&worktree, &["log", "--oneline", "-1", "--format=%s"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "feat: the worker's commit",
        "the commit must be on the worker's own branch",
    );
}

/// Run `args` as the demote target, the way cosmon's own dispatch prefix does.
///
/// `setpriv` rather than `fork` + `setuid` deliberately: it is the exact
/// mechanism [`cosmon_transport`] uses to demote a real worker, so a difference
/// between what this test can do and what a worker can do is a difference in
/// cosmon, not in the harness.
///
/// Not `#[cfg(target_os = "linux")]`: the platform gate lives *inside* the
/// body, so an `--ignored` run on a non-Linux bench reaches a loud failure
/// instead of compiling the caller out and reporting `running 0 tests … ok`.
#[allow(dead_code)]
fn as_target_uid(dir: &Path, args: &[&str]) -> std::process::Output {
    assert!(
        cfg!(target_os = "linux"),
        "this test demotes with `setpriv`, which is util-linux — it needs a \
         root Linux container. Reaching this line on {} means the bench \
         cannot run it; that is a loud failure on purpose, so the suite \
         never reports `ok` for a proof it did not perform.",
        std::env::consts::OS,
    );
    Command::new("setpriv")
        .current_dir(dir)
        .arg(format!("--reuid={TARGET}"))
        .arg(format!("--regid={TARGET}"))
        .arg("--clear-groups")
        .args(args)
        .output()
        .expect("setpriv is present in a root container (util-linux)")
}

/// The necessity half, at a genuinely different uid: after provisioning, the
/// demoted worker can record a commit on its own branch and cannot touch the
/// dispatcher's `hooks/`, `config`, or a sibling worktree's plumbing.
///
/// # Why this is `#[ignore]`d, and why it must stay loud
///
/// Two uids are needed, and only root can create a path owned by a uid it is
/// not, hand it away, and then act as the recipient. A non-root bench cannot
/// build the initial condition at all. Rather than hollow it out into something
/// that passes without proving anything, it is gated on being root — and run as
/// an ordinary user it **fails loudly**, exactly like the sibling demote
/// suites. An ignored test that silently self-neuters is the failure mode this
/// whole branch exists to police; do not weaken this gate to make a bench go
/// green.
///
/// It carries no `#[cfg(target_os = "linux")]`, and that absence is the
/// point. It used to, and the effect was measured on this branch:
///
/// ```text
/// --test demote_git_plumbing_scope -- --ignored  → RC=0    running 0 tests   ok
/// --test demote_worktree_ownership -- --ignored  → RC=101  running 2 tests   FAILED
/// ```
///
/// The sibling fails loudly because it is `#[ignore]` + a loud assertion.
/// This one added a platform `cfg` on top, so on macOS it was compiled out
/// and `--ignored` printed a green line — the doc above claimed it "fails
/// loudly", and on the only bench most people run it was not even present.
/// A skip that reads as a pass is precisely this round's defect class,
/// reappearing inside the fix for it. The Linux-only `setpriv` is gated in
/// [`as_target_uid`]'s body instead, so a macOS `--ignored` run now fails
/// with a sentence naming what it needs.
///
/// ```text
/// cargo test -p cosmon-transport --test demote_git_plumbing_scope -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires root and two uids (only root can create root-owned paths and chown them away); \
            run with `cargo test -p cosmon-transport --test demote_git_plumbing_scope -- --ignored`"]
fn the_demoted_worker_commits_its_branch_and_cannot_reach_the_dispatchers_hooks() {
    use std::os::unix::fs::MetadataExt as _;

    use cosmon_core::root_spawn_policy::{enforce_demote_provisioning, RootSpawnDecision};
    use cosmon_transport::demote_provisioning::{provision_demote_resources, DemoteResources};

    // The demote arm, entered explicitly: `decide_root_spawn` refuses every
    // root dispatch now (see its docs), so the funnel would return a refusal
    // here without chowning anything, and every ownership assertion below
    // would be measuring the refusal instead of the transfer.
    let dormant_provision = |to_uid: u32, resources: &DemoteResources| {
        enforce_demote_provisioning(
            RootSpawnDecision::Demote { to_uid },
            &provision_demote_resources(to_uid, resources),
        )
    };

    assert_eq!(
        nix::unistd::Uid::effective().as_raw(),
        0,
        "this test reproduces a root-dispatcher defect and proves nothing as a \
         non-root user — run it inside a root container, do not weaken it",
    );

    let tmp = TempDir::new().expect("tempdir");
    let (repo, worktree) = repo_with_linked_worktree(tmp.path());
    let common = repo.join(".git");
    // A second molecule already in flight: its plumbing must survive the first
    // one's dispatch untouched.
    let sibling = tmp.path().join("wt-sibling");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feat/task-20260727-0000",
            sibling.to_str().expect("utf-8 sibling path"),
        ],
    );
    let sibling_gitdir = common.join("worktrees/wt-sibling");
    let hook = common.join("hooks/pre-commit");
    std::fs::write(&hook, b"#!/bin/sh\nexit 0\n").expect("write a dispatcher hook");

    let decision = dormant_provision(
        TARGET,
        &DemoteResources::for_dispatch(&worktree, vec![], None, vec![], None),
    );
    assert_eq!(
        decision,
        RootSpawnDecision::Demote { to_uid: TARGET },
        "a freshly created worktree must not refuse its own dispatch",
    );

    // What the worker got.
    for granted in [
        &worktree,
        &common.join("worktrees/wt"),
        &common.join("objects"),
        &common.join("refs/heads"),
    ] {
        assert_eq!(
            std::fs::metadata(granted).expect("stat granted").uid(),
            TARGET,
            "{} must belong to the demoted worker",
            granted.display(),
        );
    }

    // What it did not. These are the entries the previous whole-common-dir
    // transfer handed over, and `hooks/` is the one the dispatcher *executes*
    // as root at `cs done`.
    for withheld in [
        &common,
        &common.join("config"),
        &common.join("hooks"),
        &hook,
        &sibling_gitdir,
        &common.join("worktrees"),
    ] {
        assert_eq!(
            std::fs::metadata(withheld).expect("stat withheld").uid(),
            0,
            "{} was handed to the worker and must not have been",
            withheld.display(),
        );
    }

    // And ownership is not theory: the worker can commit…
    std::fs::write(worktree.join("artefact.txt"), b"the worker did its work\n")
        .expect("write artefact");
    for args in [
        vec!["git", "add", "artefact.txt"],
        vec![
            "git",
            "commit",
            "--quiet",
            "-m",
            "feat: the worker's commit",
        ],
    ] {
        let out = as_target_uid(&worktree, &args);
        assert!(
            out.status.success(),
            "the demoted worker cannot {args:?} on its own branch:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    // Control for the two refusals below: the same gesture, on a path the
    // worker *was* given, must succeed. Without it, "tee failed" could mean
    // setpriv is broken, or tee is missing, or the uid cannot exec at all —
    // and the refusals would read as proof while proving nothing.
    let owned = worktree.join("proof-of-write");
    let out = as_target_uid(tmp.path(), &["tee", owned.to_str().expect("utf-8 path")]);
    assert!(
        out.status.success(),
        "control failed: the demoted worker cannot write its own worktree, so \
         the refusals below say nothing about the narrowing:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );

    // …and cannot rewrite what the dispatcher will execute.
    let out = as_target_uid(
        tmp.path(),
        &["tee", hook.to_str().expect("utf-8 hook path")],
    );
    assert!(
        !out.status.success(),
        "the demoted worker rewrote a hook the dispatcher runs as root:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    let out = as_target_uid(
        tmp.path(),
        &[
            "tee",
            sibling_gitdir
                .join("HEAD")
                .to_str()
                .expect("utf-8 sibling HEAD path"),
        ],
    );
    assert!(
        !out.status.success(),
        "the demoted worker reached a sibling molecule's plumbing:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The residue, executable: the grant a demote would have to hand out lets a
/// worker rewrite a sibling molecule's branch and delete the very commit object
/// that molecule's branch points at.
///
/// This is now the **justification** of a refusal rather than a description of
/// live behaviour: `decide_root_spawn` declines every root dispatch precisely
/// because this grant is what making one work requires. The test stays, and
/// stays keyed on [`git_plumbing_paths`], because the day someone bounds the
/// grant is the day the refusal can be lifted — and this is the thing that has
/// to go red first.
///
/// # Why this test asserts the hole instead of asserting its absence
///
/// The module header has stated the sibling-ref residue in prose since the
/// third instance of issue #20. Prose does not go red. Round 2's referees
/// measured it in a uid-10001 container and found both operations succeed —
/// `sibling_rewrite=SUCCEEDED`, `shared_object_delete=SUCCEEDED` — while
/// this suite reported `ok`, because every test here asks about paths that
/// were **withheld** and none asks about what the **granted** paths permit.
///
/// So this asks the second question, and answers it truthfully. It is a
/// characterisation test: it records the authority cosmon actually hands
/// out, keyed on [`git_plumbing_paths`] so it cannot drift away from the
/// real grant. Two things follow, and both are the point.
///
/// - If someone widens the grant, the residue is already named here and the
///   next reader is not surprised by it.
/// - If someone **bounds** the grant — per-worker ref and object storage
///   with a controlled integration step, which is the design the module
///   header names — this test goes red. That red is the good news, and its
///   failure message says so. Delete it, delete the residue section of the
///   header, and write the refusal test that replaces it.
///
/// # Why it needs no container
///
/// The container measurement used two uids. This uses the mode-bit freeze:
/// everything in the common dir except the granted set is made unwritable,
/// and [`freeze_verdict`] proves that freeze took. What survives is
/// therefore permitted *by the grant* and by nothing else — the same
/// property, on every bench, in CI, without root.
#[test]
fn the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete() {
    let tmp = TempDir::new().expect("tempdir");
    let (repo, worktree) = repo_with_linked_worktree(tmp.path());
    let common = repo.join(".git");

    // A second molecule already in flight, exactly as the fleet runs them.
    let sibling_branch = "feat/task-20260727-0000";
    let sibling = tmp.path().join("wt-sibling");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            sibling_branch,
            sibling.to_str().expect("utf-8 sibling path"),
        ],
    );

    // The worker's own honest commit, which gives us a sha to vandalise with.
    std::fs::write(worktree.join("artefact.txt"), b"the worker did its work\n")
        .expect("write artefact");
    git(&worktree, &["add", "artefact.txt"]);
    git(&worktree, &["commit", "--quiet", "-m", "feat: the worker"]);
    let worker_sha = String::from_utf8_lossy(&git(&worktree, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();

    // The sibling molecule's own commit. It exists so the victim object below
    // can be one the SIBLING depends on and the worker demonstrably does not.
    std::fs::write(sibling.join("sibling.txt"), b"the other molecule's work\n")
        .expect("write the sibling's artefact");
    git(&sibling, &["add", "sibling.txt"]);
    git(&sibling, &["commit", "--quiet", "-m", "feat: the sibling"]);

    let sibling_ref = format!("refs/heads/{sibling_branch}");
    let before = String::from_utf8_lossy(&git(&repo, &["rev-parse", &sibling_ref]).stdout)
        .trim()
        .to_owned();

    // The victim, chosen by NAME rather than by directory order.
    //
    // This used to walk `objects/` with `read_dir` and take the first loose
    // file it met. `read_dir` order is unspecified, and instrumenting it showed
    // the victim was the worker's OWN commit in roughly three runs in ten — on
    // those runs the test proved a worker can delete an object it wrote three
    // lines earlier, which is not the property this file, the module header and
    // the public docs all claim. Worse, it failed that way only sometimes: a
    // partial bounding of the grant (per-worker fan-out for a worker's own new
    // objects — the natural first step toward the alternates design) would have
    // left this witness flaky at ~30% instead of red, misfiring exactly when
    // someone started fixing the thing it guards.
    //
    // So the victim is derived from the sibling's tip sha: `objects/XX/YYYY…`,
    // one path, the same one on every run and on every bench.
    let (fan, rest) = before.split_at(2);
    let loose = common.join("objects").join(fan).join(rest);
    assert!(
        loose.is_file(),
        "precondition: the sibling's tip commit {before} must be a LOOSE object          at {} — if git packed it, this test is measuring nothing",
        loose.display(),
    );
    assert_ne!(
        before, worker_sha,
        "precondition: the victim must be the sibling's object, not the worker's",
    );

    let granted = git_plumbing_paths(&worktree);
    let frozen = freeze_all_but(&common, &granted);
    let bites = freeze_verdict(&frozen);

    // Attack 1 — rewrite the sibling molecule's branch. `update-ref` is
    // plumbing and does not honour the "branch is checked out elsewhere"
    // guard that `git branch -f` applies, so the sibling worktree being live
    // buys nothing.
    let rewrite = Command::new("git")
        .current_dir(&repo)
        .args(["update-ref", &sibling_ref, &worker_sha])
        .output()
        .expect("git is on PATH");

    // Attack 2 — delete an object the sibling's history may depend on.
    let deleted = std::fs::remove_file(&loose).is_ok();

    for path in &frozen {
        set_mode(path, 0o700);
    }
    if let Err(complaint) = bites {
        panic!("{complaint}");
    }

    let after = String::from_utf8_lossy(&git(&repo, &["rev-parse", &sibling_ref]).stdout)
        .trim()
        .to_owned();

    let closed = "\n\nIf this went red because you bounded the grant to \
                  per-worker ref/object storage: that is the fix issue #20 \
                  has been waiting for. Delete this test and the \
                  \"What is *not* proven\" section of the module header, and \
                  put a refusal test in its place.";

    assert!(
        rewrite.status.success() && after == worker_sha && after != before,
        "the sibling-ref rewrite no longer succeeds ({before} -> {after}):\n{}{closed}",
        String::from_utf8_lossy(&rewrite.stderr),
    );
    assert!(
        deleted && !loose.exists(),
        "the sibling's commit object {before} at {} survived deletion.{closed}",
        loose.display(),
    );
}

/// The dispatcher's code paths are not in the transfer, at the level of the
/// derivation. Hermetic counterpart to the root test below: it cannot prove
/// what a foreign uid may *do*, but it does prove what cosmon *offers*, and it
/// runs on every bench.
#[test]
fn the_derivation_never_offers_the_dispatchers_hooks_or_config() {
    let tmp = TempDir::new().expect("tempdir");
    let (repo, worktree) = repo_with_linked_worktree(tmp.path());
    let common = repo.join(".git");

    let granted = git_plumbing_paths(&worktree);
    for forbidden in ["config", "hooks", "info", "packed-refs", "modules"] {
        let path = common.join(forbidden);
        assert!(
            !granted.iter().any(|g| path.starts_with(g)),
            "`{forbidden}` is reachable from the transfer roots {granted:?}",
        );
    }
    assert!(
        !granted.iter().any(|g| common.starts_with(g)),
        "the common dir itself is reachable from {granted:?}",
    );
}
