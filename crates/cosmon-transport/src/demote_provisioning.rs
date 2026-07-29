// SPDX-License-Identifier: AGPL-3.0-only

//! The filesystem port behind
//! [`cosmon_core::root_spawn_policy::enforce_demote_provisioning`]
//! — COSMON-DEV #20 defect A3.
//!
//! # Dormant — read this first
//!
//! **No live dispatch enters this module's repair path any more.**
//! `cosmon_core::root_spawn_policy::decide_root_spawn` refuses every root
//! dispatch, because the grant a demotion needs — the repository's shared
//! object store and shared `refs/heads` — is authority over every sibling
//! molecule, reproduced twice (see [`git_plumbing_paths`]). The refusal names
//! the nominal invocation instead: run `cs` as the same non-root uid the
//! workers run as, which needs no hand-over at all.
//!
//! Everything below describes that hand-over faithfully and is kept for two
//! reasons — it is the substrate of the bounded per-worker ref/object
//! lifecycle, and it is the measured evidence the refusal rests on. Read it as
//! a description of a capability, not of live behaviour. The one thing here
//! that *is* live is the judge on the non-root path
//! ([`as_is_reachability_refusal`]), which chowns nothing.
//!
//! # Why this lives here and not in the CLI
//!
//! The first fix installed the provisioning refusal at **one** of the two demote
//! call sites: interactive `cs tackle`. The other one —
//! [`spawn_claude_session`](crate::claude::spawn_claude_session), which `cs thaw`
//! and the patrol respawn backstop both reach — computed the root-spawn decision
//! and acted on it with no provisioning check at all. A root container thawing a
//! paused worker therefore still demoted to a uid that cannot read root's
//! `/root/.claude` or write the root-owned `.cosmon/state/`, and the worker
//! started, was declared live by the readiness probe, and wedged on `EACCES`
//! mid-run: the exact wedge A3 exists to prevent, reached by a different door.
//! That is the same CLI-vs-transport asymmetry that produced A1.
//!
//! One shared port, used by both call sites, is what makes the asymmetry
//! impossible to reintroduce by editing one crate. The transport crate is the
//! natural home: the domain core is I/O-free, and this is `stat(2)`.
//!
//! # The load-bearing rule
//!
//! Every question is asked **about the target uid**, never about the identity
//! holding the file descriptor. The dispatcher is root, so a trial write would
//! succeed and prove nothing. The answers are therefore mode-bit arithmetic
//! against `to_uid`, with the group bits checked against the same numeric value
//! because `setpriv --regid <uid>` sets the primary gid to it.
//!
//! # The resource set: what is enumerable, and what is not
//!
//! What cosmon repairs and what cosmon judges are one list. That rule held
//! three times and the *list* failed three times — the out-of-worktree state
//! dirs, the startup-consent files, and the git plumbing a linked worktree
//! commits through — each found by an external tester rather than by us. So the
//! list itself is the thing that needs a discipline, not another entry.
//!
//! Walking the filesystem-touching code paths a demoted worker takes across a
//! full lifecycle gives five kinds, and only five:
//!
//! | phase | what the worker touches | resource |
//! |---|---|---|
//! | exec | runs the adapter binary, and traverses every directory above it | [`DemoteResource::WorkerBinary`] |
//! | spawn | reads its credential and the consent cosmon pre-granted | [`DemoteResource::ConfigHome`], [`DemoteResource::ConsentFile`] |
//! | work | edits files in its checkout | [`DemoteResource::Worktree`] |
//! | commit | writes the index, HEAD, reflog, objects, refs — *none of them inside the checkout* | [`DemoteResource::GitPlumbing`] |
//! | evolve / note / collapse / complete | writes molecule state, events, the fleet lock | [`DemoteResource::StateDir`] |
//!
//! Two candidates were walked and **cleared**, which is part of the enumeration
//! and not a gap in it. The briefing temp file is opened by the *dispatcher's*
//! shell (`{ rm -f f; setpriv … claude -p; } < f`), so the demoted process
//! inherits an open fd and never resolves the path — a root-owned `0600` temp
//! file is correct there, not a bug. The tmux socket belongs to the server
//! cosmon started; the worker talks to cosmon through the filesystem state
//! dirs, never through that socket.
//!
//! The kinds are closed; the **paths behind them are not**, and pretending
//! otherwise is what produced three incidents. `.claude.json` is a Claude Code
//! implementation detail that a release can rename; git's layout moved once
//! already inside this very issue (worktree → gitdir → `commondir`) and moves
//! again with `--separate-git-dir`, submodule gitdirs, and the reftable backend.
//! A literal path list is a snapshot of somebody else's internals.
//!
//! Two things replace prediction, and both are structural rather than
//! remembered:
//!
//! - **Derive, don't recall.** Every path is computed from a primitive the
//!   caller genuinely knows, by [`DemoteResources::for_dispatch`] — one
//!   function, both call sites. The git roots come out of git's own published
//!   pointers ([`git_plumbing_paths`]), not out of a format string.
//! - **Transfer roots, not leaves — where the root belongs to one worker.**
//!   The repair is a *recursive* chown, so everything a tool keeps beneath a
//!   root is covered without cosmon naming it. That is what makes the
//!   enumeration's incompleteness survivable for the worktree, the molecule
//!   state dirs and the linked gitdir: each belongs to this molecule and to no
//!   other, so being generous inside it costs nothing.
//!
//!   The rule inverts the moment a root is **shared**. The repository's common
//!   dir is one tree serving every worktree, and handing it over whole gave a
//!   single worker `config`, `hooks/` and every other molecule's plumbing —
//!   which is not a commit capability but authority over the repository, and
//!   over the dispatcher that runs git in it as root afterwards. So the common
//!   dir is *entered* rather than transferred, and only the three entries a
//!   commit writes go across (`SHARED_COMMIT_SUBPATHS`, private to this
//!   module and enumerated at its definition). Generosity inside a
//!   shared root is not survivable, and the discipline has to say which kind of
//!   root it is looking at.
//!
//! The residue is honest and worth stating: a resource **outside** all four
//! roots — a tool's cache under a different `HOME`, a credential helper's
//! socket, an object store reached through `objects/info/alternates` — is still
//! invisible here, and the static check is a necessary condition, never a
//! sufficient one. The end state we can promise is the one the refusal names:
//! a resource cosmon knows about and cannot hand over stops the dispatch before
//! a live worker exists. A resource cosmon does not know about will still fail
//! at the worker — which is why the worker-side discipline (diagnose, `cs note`,
//! `cs collapse --reason-kind blocker_stuck`) is the other half of this, and is
//! exactly what happened on the tester's bench.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cosmon_core::root_spawn_policy::{
    decide_root_spawn, enforce_demote_provisioning, DemoteResource, DemoteResourceAccess,
    RootSpawnDecision,
};

/// What a demoted worker must be able to *do* with a path.
///
/// The distinction is not cosmetic. A config home the worker only writes to is
/// useless: `claude` **reads** its credentials from it, so a target-owned
/// `0300` directory (write + search set, read clear) passes a write-only check
/// and still yields `EACCES` on the credential read — a survivor the reviewers
/// found in the first fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredAccess {
    /// Create and modify entries: write + search (`w+x`). Worktrees, state dirs.
    Write,
    /// Read entries *and* write them: `r+w+x`. The Claude config home, which is
    /// read for credentials and written for session state.
    ReadWrite,
    /// Read and rewrite a **regular file**: `r+w`, with no search bit.
    ///
    /// Separate from [`Self::ReadWrite`] because the executable bit is a
    /// directory's search permission and means nothing on a config file. A
    /// consent file is written mode 0600; asking 0700 of it would refuse every
    /// correctly-provisioned dispatch, which is the mirror image of the bug
    /// this variant exists to catch.
    ReadWriteFile,
    /// Run a program: the search/execute bit alone (`x`), on the file and on
    /// every directory above it.
    ///
    /// Deliberately does **not** ask for read. An ELF binary needs only `x`,
    /// and demanding `r` would refuse a legitimately `0711` interpreter — the
    /// mirror-image mistake of the one this whole issue is about. The value is
    /// almost entirely in the ancestor walk: the external tester's own recipe
    /// contains `chmod o+x /root`, because `claude` installs under a `0700`
    /// home and a demoted worker cannot traverse to it.
    Execute,
}

impl RequiredAccess {
    /// The owner-triple mode mask this access needs; shifted per class in
    /// [`has_mode`] for a non-owning uid.
    const fn owner_mask(self) -> u32 {
        match self {
            Self::Write => 0o300,
            Self::ReadWrite => 0o700,
            Self::ReadWriteFile => 0o600,
            Self::Execute => 0o100,
        }
    }
}

/// Whether `uid` has `need` on `path` — or, when `path` does not exist yet, on
/// the nearest existing ancestor it would have to be created in.
///
/// Also walks every ancestor for the search (`x`) bit: a perfectly-moded leaf
/// under an unreachable parent is unreachable, and `stat`ing only the leaf as
/// root hides that completely.
///
/// This is a *necessary* condition, not a sufficient one — it cannot see ACLs,
/// mount flags, or `SELinux` labels. A `true` verdict is never a promise that
/// nothing else can go wrong; a `false` one is a promise that something will.
#[must_use]
pub fn path_usable_by_uid(path: &Path, uid: u32, need: RequiredAccess) -> bool {
    // The nearest existing ancestor: creating a missing dir is a write to it.
    let mut probe = path;
    let target = loop {
        if std::fs::metadata(probe).is_ok() {
            break probe;
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            // No existing ancestor at all — nothing usable to report.
            None => return false,
        }
    };

    if !has_mode(target, uid, need.owner_mask()) {
        return false;
    }

    // Every ancestor must be traversable, or the leaf's own bits are moot.
    let mut ancestor = target.parent();
    while let Some(dir) = ancestor {
        // Search only (`x`); an ancestor need not be writable to be walked.
        if std::fs::metadata(dir).is_ok() && !has_mode(dir, uid, 0o100) {
            return false;
        }
        ancestor = dir.parent();
    }
    true
}

/// Does `uid` hold every bit of `owner_mask` on `path`, using the permission
/// triple that applies to it (owner, then group, then other)?
fn has_mode(path: &Path, uid: u32, owner_mask: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let shift = if meta.uid() == uid {
        0
    } else if meta.gid() == uid {
        3
    } else {
        6
    };
    let mask = owner_mask >> shift;
    meta.mode() & mask == mask
}

/// Probe every path a demoted worker must be able to use.
///
/// `config_home` is the Claude config dir the worker authenticates from —
/// `CLAUDE_CONFIG_DIR` when set, else `$HOME/.claude`. Under `docker run -u 0`
/// with the environment preserved (the demotion prefix deliberately omits
/// `--reset-env`) that is root's `/root/.claude`, mode 0700: the reviewers'
/// predicted `EACCES`, and the reason the config home is probed for **read**
/// access, not merely write.
///
/// `consent_files` are the files cosmon wrote into that config home before the
/// spawn (`.claude.json`, `settings.json`). They are probed **individually**,
/// not inferred from the directory: the measured failure (2.1.220, no
/// credential involved) is a worker-owned config home containing a root-owned
/// `.claude.json`, where every directory-level question answers yes and the
/// worker still cannot open the file. A check that passes over an unreadable
/// file is worse than no check — it is a gate reporting green on the broken
/// state.
///
/// `git_plumbing` is the set [`git_plumbing_paths`] derives from git's own
/// on-disk layout: the worktree's gitdir and the repository's common dir. It is
/// listed here for the same reason the consent files are — a linked worktree
/// that is perfectly worker-owned still commits through directories that are
/// not inside it.
#[must_use]
pub fn demote_resource_checks(
    uid: u32,
    config_home: Option<&Path>,
    worktree: &Path,
    state_dirs: &[PathBuf],
    git_plumbing: &[PathBuf],
    consent_files: &[PathBuf],
    worker_binary: Option<&Path>,
) -> Vec<DemoteResourceAccess> {
    let mut checks = Vec::new();
    let mut push = |resource: DemoteResource, path: &Path, need: RequiredAccess| {
        checks.push(DemoteResourceAccess {
            resource,
            path: path.to_string_lossy().into_owned(),
            usable: path_usable_by_uid(path, uid, need),
        });
    };
    if let Some(home) = config_home {
        push(DemoteResource::ConfigHome, home, RequiredAccess::ReadWrite);
    }
    push(DemoteResource::Worktree, worktree, RequiredAccess::Write);
    for dir in state_dirs {
        push(DemoteResource::StateDir, dir, RequiredAccess::Write);
    }
    for path in git_plumbing {
        push(DemoteResource::GitPlumbing, path, RequiredAccess::Write);
    }
    if let Some(bin) = worker_binary {
        push(DemoteResource::WorkerBinary, bin, RequiredAccess::Execute);
    }
    for file in consent_files {
        // Only files that are actually there: `path_usable_by_uid` falls back
        // to the nearest existing ancestor for a missing path, which would turn
        // "cosmon has not written it yet" into a directory verdict wearing a
        // `ConsentFile` label.
        if std::fs::symlink_metadata(file).is_ok() {
            push(
                DemoteResource::ConsentFile,
                file,
                RequiredAccess::ReadWriteFile,
            );
        }
    }
    checks
}

/// The git directories a worker running in `worktree` records commits
/// **through**, read out of git's own on-disk layout rather than guessed.
///
/// # Why this is derived and not a constant (issue #20, third instance)
///
/// A *linked* worktree — what `cs tackle` creates — holds no git state of its
/// own beyond a `.git` **file** whose single line points elsewhere:
///
/// ```text
/// gitdir: /srv/acc/.git/worktrees/task-20260727-339d
/// ```
///
/// HEAD, the index, `logs/`, `ORIG_HEAD` and the commit-message buffer live at
/// that target; the objects the commit creates and the branch ref it moves live
/// one level further out, in the repository's *common* dir. Neither is under
/// the worktree, so chowning the worktree gives the worker the power to edit
/// files and not the power to record them — which is exactly what the external
/// tester measured: two dispatches, both artefacts written, neither committed.
///
/// Hard-coding `<repo>/.git/worktrees/<name>` would work today and be a fourth
/// incident the first time git changes where it puts things (`commondir`
/// indirection, a `--separate-git-dir` checkout, a submodule's gitdir under
/// `.git/modules/`). So the layout is *read*: the `.git` file names the gitdir,
/// and the gitdir's `commondir` file names the common dir. Both are git's own
/// published pointers, which is the nearest thing to asking git without paying
/// for a subprocess — and without the irony of shelling out to a `git` that may
/// itself refuse the repository as dubiously owned.
///
/// # Why the common dir is entered and not handed over whole
///
/// "Transfer roots, not leaves" is right about the *gitdir*, which belongs to
/// one worktree and therefore to one molecule. It is wrong about the **common
/// dir**, and the first version of this function did not draw the line: it
/// returned `<repo>/.git` and a recursive chown followed. One demoted worker
/// then owned `config`, `hooks/`, every branch under `refs/`, and every other
/// live worktree's plumbing under `worktrees/`. That is not a bounded
/// per-molecule commit capability; it is authority over the repository, and two
/// of those entries are worse than data:
///
/// - `hooks/` is **executed** by whoever runs git next. The dispatcher runs git
///   next, as root, at `cs done`. A worker that owns `hooks/pre-merge-commit`
///   chooses what root runs.
/// - `config` is read by that same git, and `core.hooksPath` / `alias.*` /
///   `core.fsmonitor` each name a command. Owning the file is owning the
///   dispatcher's next invocation.
///
/// Neither is needed to record a commit, so neither is transferred. What a
/// commit in a linked worktree actually writes outside its own gitdir is a
/// short list, and it is the list below: the object it creates, the branch ref
/// it moves, and that ref's reflog. Everything else in the common dir —
/// `config`, `hooks/`, `info/`, `packed-refs`, `worktrees/`, `modules/` — is
/// read by the worker and stays root-owned.
///
/// # The residue that closed the path — stated rather than implied, and measured
///
/// **The set this function returns is repository-wide destructive authority,
/// and that is why no live dispatch is handed it any more.** Two of the three
/// subpaths carry it, and neither can be closed by a tighter `chown`:
///
/// - `refs/heads` is transferred as a **directory**, because a loose-ref store
///   creates `<branch>.lock` and `<branch>` inside it, and a cosmon branch is
///   `feat/task-…`, so the subdirectory holding it holds the sibling molecules'
///   branches too. A worker can therefore rewrite or delete a sibling branch
///   ref. Git's files backend gives no per-ref delegation, a sibling ref moved
///   into `packed-refs` is shadowed by a loose ref the worker may create, and
///   `git update-ref` is plumbing that ignores the "checked out in another
///   worktree" guard.
/// - `objects` is likewise a **whole store**. A worker can delete or replace
///   any object in it, including one another molecule's history depends on.
///   Narrowing fails for the same structural reason: writing a loose object
///   means creating the `objects/XX/` fan-out directory, which needs write on
///   `objects` itself, and owning `objects/XX/` is owning every object in it.
///
/// This is not inferred from the code. Round 2 of `converge-20260727-a302`
/// reproduced both in a Linux container at uid 10001 — `sibling_rewrite`
/// and `shared_object_delete` both `SUCCEEDED` — and round 3 reproduced them
/// again by a second mechanism on an ordinary uid-501 account. The test
/// `the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete`
/// in `demote_git_plumbing_scope.rs` is that second reproduction, kept in the
/// suite so the residue goes red when — and only when — someone closes it.
///
/// The shape that closes it is per-worker ref **and** object storage: a
/// per-worker repository reaching the shared store read-only through
/// `objects/info/alternates`, with `cs done` **fetching** rather than merging
/// in place. That is a different worktree lifecycle, not a tighter `chown`,
/// and it is deliberately not attempted as a patch.
///
/// What was done instead is the fourth possible move, after three narrowings:
/// **the path is refused.** `cosmon_core::root_spawn_policy::decide_root_spawn`
/// declines every root dispatch, so nothing computes this set on the way to a
/// live worker — and the honest claim stops being "the grant is narrow enough"
/// (it is not) and becomes "the grant is never made". A caveat an operator
/// cannot read is not a control (§8z); a refusal they cannot miss is.
///
/// This function therefore still describes, faithfully, what a demotion would
/// need. It is kept for the bounded lifecycle above and for the tests that
/// characterise the grant — see
/// [`provision_demote_resources`] for why the dormant machinery is public.
///
/// Returns the paths deduplicated, and an empty vector when `worktree` is not
/// in a git repository at all (which is not an error: a caller with no
/// repository has no plumbing to repair, and the empty list is judged as such).
#[must_use]
pub fn git_plumbing_paths(worktree: &Path) -> Vec<PathBuf> {
    let dot_git = worktree.join(".git");
    let Ok(meta) = std::fs::metadata(&dot_git) else {
        return Vec::new();
    };

    // A normal (non-linked) checkout: `.git` IS the gitdir and the common dir,
    // it is *inside* the worktree, and no other worktree shares it. The
    // recursive worktree transfer already covers it; naming it here keeps it in
    // the judge's list, which is the half that must not be skipped.
    if meta.is_dir() {
        return vec![dot_git];
    }

    // A linked worktree: `.git` is a one-line pointer file.
    let Some(gitdir) = linked_gitdir(worktree) else {
        return Vec::new();
    };

    // The gitdir belongs to this worktree alone — HEAD, the index, `logs/HEAD`,
    // `ORIG_HEAD`, `COMMIT_EDITMSG`. Whole-tree transfer is bounded here.
    let mut paths = vec![gitdir.clone()];
    // `commondir` is written by `git worktree add` and holds a path relative to
    // the gitdir (`../..` in the usual layout). Absent on a repo that has no
    // separate common dir, in which case the gitdir already is one.
    if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) {
        let common = resolve_against(&gitdir, common.trim());
        if common != gitdir {
            paths.extend(SHARED_COMMIT_SUBPATHS.iter().map(|sub| common.join(sub)));
        }
    }
    paths
}

/// The only entries of a repository's **common** dir a demoted worker is given.
///
/// Derived from what `git commit` in a linked worktree writes outside its own
/// gitdir, and nothing else:
///
/// - `objects` — the blob, tree and commit it creates, plus git's temp files
///   and any pack it writes.
/// - `refs/heads` — the branch ref, and the `<branch>.lock` beside it.
/// - `logs/refs/heads` — that ref's reflog, which git writes because
///   `core.logAllRefUpdates` defaults on outside a bare repo.
///
/// The list is short on purpose and is checked empirically, not reasoned about:
/// `a_commit_needs_only_the_shared_subpaths_this_module_names` in
/// `demote_git_plumbing_scope.rs` makes every *other* entry of a real common
/// dir unwritable and then runs a real `git commit`. If git grows a fourth
/// write, that test goes red here rather than an operator's worker wedging on
/// `EACCES` in a container.
const SHARED_COMMIT_SUBPATHS: [&str; 3] = ["objects", "refs/heads", "logs/refs/heads"];

/// The gitdir a *linked* worktree points at, or `None` for a normal checkout
/// (where `.git` is the directory itself) and for anything that is not a
/// worktree at all.
fn linked_gitdir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if std::fs::metadata(&dot_git).ok()?.is_dir() {
        return None;
    }
    std::fs::read_to_string(&dot_git)
        .ok()?
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))
        .map(|p| resolve_against(worktree, p.trim()))
}

/// The paths whose ownership cosmon deliberately makes foreign, and which the
/// **dispatcher's** git must therefore be told to accept.
///
/// Not the same list as [`git_plumbing_paths`], and the difference is the point
/// of the narrowing. `safe.directory` is a statement about a *repository*, so
/// it takes repository paths — the worktree and its gitdir — never
/// `objects/`. And the repository's **common** dir is absent because it is no
/// longer given away: root still owns it, so git never calls it dubious.
#[must_use]
fn ownership_exemption_paths(worktree: &Path) -> Vec<PathBuf> {
    let mut paths = vec![worktree.to_path_buf()];
    if let Some(gitdir) = linked_gitdir(worktree) {
        paths.push(gitdir);
    }
    paths
}

/// Join `relative` onto `base` unless it is already absolute, then collapse the
/// `..` components textually.
///
/// Textually rather than via `canonicalize`, because the result is handed to a
/// chown: resolving symlinks would silently move the ownership transfer onto
/// whatever a link points at, which is the same reason [`chown_tree_to_uid`]
/// uses `lchown`.
fn resolve_against(base: &Path, relative: &str) -> PathBuf {
    let joined = if Path::new(relative).is_absolute() {
        PathBuf::from(relative)
    } else {
        base.join(relative)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Resolve `name` the way the shell in the worker's pane will: as-is when it
/// already contains a separator, otherwise by walking the dispatcher's `PATH`.
///
/// `None` when nothing matches, which is not an error here — a binary cosmon
/// cannot locate is one it must not blame in a refusal. The pane's own `sh`
/// makes the final call and will say so loudly if it disagrees.
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return direct.exists().then(|| direct.to_path_buf());
    }
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(name))
        .find(|candidate| candidate.exists())
}

/// Resolve the Claude config home a demoted worker would authenticate from.
///
/// `config_dir` when the spawn path resolved one, else `$HOME/.claude` — `HOME`
/// being the *dispatcher's*, because the demotion prefix preserves the
/// environment. `None` when neither is knowable, in which case the check is
/// skipped rather than guessed at. `env_lookup` is injected so the resolver
/// stays testable without mutating the process environment.
#[must_use]
pub fn demote_config_home<F>(config_dir: Option<&str>, env_lookup: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<String>,
{
    config_dir
        .map(PathBuf::from)
        .or_else(|| env_lookup("HOME").map(|h| Path::new(&h).join(".claude")))
}

/// Everything a demoted worker must be able to use, as *paths* rather than as
/// a closure.
///
/// # Why a struct and not a closure (issue #20)
///
/// The previous entry point took a `checks_for` closure, which could only ever
/// *observe*. Repairing the ownership catch-22 needs the port to **act** on the
/// same paths it judges — chown them to the target uid — and a closure hides
/// them from it. Naming the paths in a struct is what makes the repair and the
/// verdict share one input, so no caller can apply one without the other. That
/// is the same "no forgettable intermediate step" discipline that put the
/// privilege drop at the binary token rather than in an env splice.
/// `#[non_exhaustive]` is the enforcement, not decoration: it makes
/// [`Self::for_dispatch`] the only way any other crate can build one, so the
/// fourth resource cannot be forgotten at a call site the way the first three
/// were. Fields stay public because this crate's own tests isolate one resource
/// at a time, which is a different job from dispatching.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DemoteResources {
    /// The Claude config home the worker authenticates from. The **directory
    /// itself is never chowned**: under `docker run -u 0` this is root's own
    /// `/root/.claude`, it can be an operator-supplied directory holding the
    /// operator's own `.credentials.json`, and taking ownership of a human's
    /// files is not a benign default. It is probed, and an unusable one still
    /// refuses.
    ///
    /// What cosmon *does* take ownership of is [`Self::consent_files`] — the
    /// files it wrote there itself. That is the deliberate line: a file cosmon
    /// authored and the worker must read is cosmon's to hand over; everything
    /// else in that directory belongs to whoever put it there.
    pub config_home: Option<PathBuf>,
    /// The git worktree the worker runs in — created by the very `cs tackle`
    /// that is demoting, hence root-owned, hence chowned here.
    pub worktree: PathBuf,
    /// The out-of-worktree state roots the worker writes on `cs evolve` /
    /// `cs complete` (the `.cosmon` dir, plus the molecule's own state dir).
    /// Also created root-owned, also chowned here.
    pub state_dirs: Vec<PathBuf>,
    /// The startup-consent files cosmon wrote into [`Self::config_home`] before
    /// this spawn — `crate::claude_trust::ConsentPaths`' two members.
    ///
    /// Chowned **and** probed. A root dispatcher writes them as root; the
    /// worker opens them as the demote target, and a `.claude.json` it cannot
    /// read is not an error it reports — Claude Code treats the unreadable file
    /// as a first run and replaces it, discarding the pre-grant and rendering
    /// the onboarding wizard nobody is there to answer. `settings.json`
    /// survives that only because Claude Code never rewrites it, which is what
    /// made the failure look selective in the field report.
    ///
    /// Empty is legitimate: a caller that has not (yet) written any consent
    /// file declares none, and nothing is chowned or judged.
    pub consent_files: Vec<PathBuf>,
    /// The git directories the worktree records commits **through** —
    /// [`git_plumbing_paths`]' output. Outside the worktree by construction, so
    /// chowning the worktree never reaches them.
    ///
    /// Chowned **and** probed, like the consent files and for the same reason:
    /// the worker must write them and cosmon's own `git worktree add` created
    /// them as root.
    pub git_plumbing: Vec<PathBuf>,
    /// The adapter binary the worker execs, when it is resolvable.
    ///
    /// Judged and **never** chowned — the one resource on the list whose
    /// remedy is genuinely the operator's, because the binary belongs to
    /// whoever installed it. `None` when it cannot be resolved from the
    /// dispatcher's `PATH`, in which case nothing is judged rather than a
    /// guessed path being blamed.
    pub worker_binary: Option<PathBuf>,
}

impl DemoteResources {
    /// The **one** place the resource set is enumerated. Both demote call sites
    /// construct through here.
    ///
    /// # Why a constructor and not two literal structs (issue #20, the class)
    ///
    /// The list has now been found incomplete three times — the state dirs, the
    /// consent files, and the worktree's git plumbing — and every time it was an
    /// external tester who found it, because every time the list was written out
    /// by hand at a call site and the missing entry was a resource nobody
    /// pictured. Struct literals invite exactly that: they compile perfectly
    /// while under-declaring, and `..Default::default()` makes under-declaring
    /// the path of least resistance.
    ///
    /// So the primitives a caller genuinely knows — where the worktree is, which
    /// state roots it granted, which config home and consent files it wrote — go
    /// in, and everything *derivable* from them is derived here, once. Adding a
    /// fourth resource is then an edit to this function and to the judge it
    /// feeds, and both call sites get it without being touched.
    ///
    /// What this cannot do is make the enumeration complete; see the module docs
    /// for what is knowable and what is not.
    #[must_use]
    pub fn for_dispatch(
        worktree: &Path,
        state_dirs: Vec<PathBuf>,
        config_home: Option<PathBuf>,
        consent_files: Vec<PathBuf>,
        worker_binary: Option<&str>,
    ) -> Self {
        Self {
            config_home,
            git_plumbing: git_plumbing_paths(worktree),
            worker_binary: worker_binary.and_then(resolve_on_path),
            worktree: worktree.to_path_buf(),
            state_dirs,
            consent_files,
        }
    }
}

/// How many times **this process** has entered the ownership-repair machinery
/// since it started.
static OWNERSHIP_TRANSFERS: AtomicU64 = AtomicU64::new(0);

/// The environment variable naming a file every repair-path entry is appended
/// to, one line per event.
///
/// The in-process counter cannot be read across a process boundary, and the
/// property worth asserting — *no repair fired anywhere along a dispatch* — is
/// asked of a `cs` the asker did not link against. So the counter has a durable
/// sink: set this to a path and every event writes one line to it. Unset,
/// nothing is written and the only observer is the counter.
pub const OWNERSHIP_TRANSFER_JOURNAL_ENV: &str = "COSMON_OWNERSHIP_TRANSFER_JOURNAL";

/// How many times this process has entered the ownership-repair path.
///
/// # Why entry and not effect (ADR-165, the measurement problem)
///
/// Final-state ownership is not evidence that no transfer happened: a `chown`
/// onto the owner a path already had leaves no trace a `stat` can see. An
/// assertion built on `stat` therefore cannot tell *the repair never ran* apart
/// from *the repair ran and changed nothing*, and those are different claims —
/// the first is the nominal identity consuming what it created, the second is
/// the hand-over still being on the path, silently succeeding.
///
/// So the count is taken at **entry**, at two granularities and both before any
/// precondition is examined:
///
/// - once for the repair path as a whole, the moment
///   [`provision_and_decide_root_spawn`] decides a demote is on the table — so a
///   run that traverses the compatibility machinery and finds nothing to do
///   still reads non-zero;
/// - once per path handed to [`chown_tree_to_uid`], before it looks at the
///   current owner, counting every node a recursive transfer walks.
///
/// The claim this instrument supports is **"the nominal path invoked no
/// ownership repair at all"** — not "the final owners are correct", which is
/// the neighbouring property and is compatible with a repair having fired.
///
/// The counter is per **process**. A dispatch is a `cs` the observer did not
/// link against, so the cross-process reading is the journal named by
/// [`OWNERSHIP_TRANSFER_JOURNAL_ENV`]: point it at a *different file per
/// dispatch* and every line carries the pid that wrote it, so no number is
/// attributable to the wrong process or the wrong dispatch.
#[must_use]
pub fn ownership_transfers_attempted() -> u64 {
    OWNERSHIP_TRANSFERS.load(Ordering::Relaxed)
}

/// Count entry into the repair path as a whole, before any precondition.
///
/// Separate from the per-path count so that a repair which enters and then
/// finds every path already correct — or finds no paths at all — is still
/// visible. A counter placed after such a guard reads zero over a run that did
/// traverse the compatibility mechanism, which is the one reading the container
/// capture must be unable to produce.
fn record_repair_entry(to_uid: u32) {
    journal_line(&format!("enter-repair-path to_uid={to_uid}"));
}

/// Increment the counter and append one attributable line to the journal, when
/// one is configured.
///
/// A journal write failure is deliberately silent: the instrument must never be
/// able to change the outcome of the dispatch it observes.
fn journal_line(event: &str) {
    use std::io::Write as _;

    OWNERSHIP_TRANSFERS.fetch_add(1, Ordering::Relaxed);
    let Some(journal) = std::env::var_os(OWNERSHIP_TRANSFER_JOURNAL_ENV) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal)
    {
        // The pid is what makes a number attributable to a process; the journal
        // path is what makes it attributable to a dispatch.
        let _ = writeln!(f, "pid={} {event}", std::process::id());
    }
}

/// Give `path` and everything beneath it to `uid` (and to `uid` as gid, which
/// is what `setpriv --regid <uid>` gives the worker).
///
/// Symlinks are chowned with `lchown`, never followed: a symlink out of the
/// worktree must not drag an unrelated tree into the ownership transfer.
/// Entries already owned by `uid` are skipped, so re-tackling in a warm
/// container is a walk and not a syscall storm.
///
/// # Errors
///
/// Returns the first `chown` or directory-read failure. Callers on the demote
/// path deliberately do **not** treat that as fatal: the provisioning checks
/// run afterwards and turn a transfer that did not take into the typed refusal
/// an operator can act on. Failing here directly would report `EPERM` where the
/// interesting fact is "the uid still cannot write this path".
pub fn chown_tree_to_uid(path: &Path, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    // Counted here, before the owner is looked at: a transfer onto the owner a
    // path already had is invisible in the final state, and "no repair fired"
    // is the claim being evidenced. See `ownership_transfers_attempted`.
    journal_line(&format!("chown tree uid={uid} {}", path.display()));
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        // Nothing to give away. A missing path is judged by the checks, which
        // fall back to its nearest existing ancestor.
        return Ok(());
    };
    if meta.uid() != uid || meta.gid() != uid {
        std::os::unix::fs::lchown(path, Some(uid), Some(uid))?;
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree_to_uid(&entry?.path(), uid)?;
        }
    }
    Ok(())
}

/// Hand `path` to `uid`, and say out loud that the failure is not consulted.
///
/// The discard is a decision, not an oversight, and it used to be spelled
/// `let _ =` at four call sites, which is the spelling of an oversight. The
/// verdict on this dispatch is the **checks**, which run afterwards: an
/// operator needs to know that the uid cannot write the path, not which errno
/// the repair attempt returned on the way there. A `chown` that fails and a
/// `chown` that was never needed are indistinguishable in the only terms that
/// matter, and both are answered by the same typed refusal.
///
/// This is therefore the one place a `chown` error may be dropped. Anything
/// that wants to *act* on the failure must call [`chown_tree_to_uid`] and
/// handle its `Result`.
fn transfer_best_effort(path: &Path, uid: u32) {
    let _ = chown_tree_to_uid(path, uid);
}

/// Create a derived plumbing directory that git has not needed yet, so the
/// judge asks about the path cosmon actually granted.
///
/// Best-effort for the same reason as [`transfer_best_effort`]: if the
/// directory cannot be created, the check that follows says so in the
/// operator's terms.
fn ensure_plumbing_dir(path: &Path) {
    if path.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(path);
}

/// Tell the **dispatcher's** git that the repository it just handed to the
/// worker is still one it may operate on.
///
/// # Why the demote target does *not* get this, and the dispatcher does
///
/// `safe.directory` is git's answer to "this repository is owned by somebody
/// else" — it suppresses a refusal, it grants no access. So the question is
/// always *whose* ownership is wrong.
///
/// For the **worker** it is now right by construction: the worktree, its gitdir
/// and the common dir are all transferred to the target uid before it starts, so
/// git's check passes on its own and `safe.directory` would be redundant. It
/// would also be actively harmful. The external tester's *dubious ownership*
/// message was a **true report of a real defect** — the plumbing really was
/// root-owned and the worker really could not commit. Configuring the exemption
/// for the target would have silenced that message and left the `EACCES`
/// underneath it, converting a diagnosable refusal into the mute hang this whole
/// issue is about. cosmon therefore never writes `safe.directory` for the
/// worker, deliberately.
///
/// For the **dispatcher** it is the transfer itself that creates the need. Once
/// the plumbing belongs to uid 10001, root is the foreign uid, and git's check —
/// unlike the mode bits — is not waived for root. `cs done` merging the
/// molecule's branch back would meet the refusal we just moved off the worker.
/// So the exemption goes exactly where the ownership is now deliberately
/// foreign: on the dispatcher, scoped to the paths cosmon transferred, never as
/// the blanket `*` a container image reaches for.
///
/// `--global` is the narrowest scope that works: git ignores `safe.directory`
/// from a repository's own config (a repo may not vouch for itself), and the
/// process-environment alternative (`GIT_CONFIG_COUNT`) would mean
/// `std::env::set_var`, a process-wide racy write this workspace rules out.
///
/// # Errors
///
/// Returns the first failure to read or write the dispatcher's git config.
/// Callers on the demote path swallow it for the same reason they swallow a
/// chown error — the provisioning verdict is about the *worker*, and this is
/// about the dispatcher's own later convenience. A failure surfaces as git's
/// ordinary, actionable *dubious ownership* message at `cs done` time, which is
/// a loud failure and not a wedge.
pub fn exempt_dispatcher_from_ownership_check(paths: &[PathBuf]) -> std::io::Result<()> {
    let existing = std::process::Command::new("git")
        .args(["config", "--global", "--get-all", "safe.directory"])
        .output()?;
    // A missing key exits non-zero with empty stdout; that is "none yet", not an
    // error worth reporting.
    let known = String::from_utf8_lossy(&existing.stdout)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();

    for path in paths {
        let value = path.to_string_lossy().into_owned();
        if known.contains(&value) {
            continue;
        }
        std::process::Command::new("git")
            .args(["config", "--global", "--add", "safe.directory", &value])
            .status()?;
    }
    Ok(())
}

/// The first resource a worker spawning **as its dispatcher's own uid** cannot
/// use, or `None` when every one of them is reachable.
///
/// # The parity hole this closes (issue #20, `@jdthaler`'s non-root container)
///
/// The `--add-dir` grant itself has no parity hole: both spawn paths emit it for
/// every root decision, pinned by
/// `grant_is_structural_across_permission_modes`. What is *not* symmetric is the
/// question behind it. `--add-dir` grants Claude Code **authorization**, never
/// OS **ownership**, so the grant is only worth anything if the uid the worker
/// runs as can actually write those directories. A `Demote` has that verified by
/// [`provision_and_decide_root_spawn`] and is refused when it fails. A
/// [`RootSpawnDecision::SpawnAsIs`] — the whole non-root fleet, including
/// `cs` launched directly under an unprivileged container uid — verified
/// nothing.
///
/// That is the *same wedge* the demote check exists to prevent, reached by the
/// door nobody guarded: a container image built as root and then dropped to
/// `USER 10001` leaves the repo's `.cosmon/` root-owned, the worker is granted
/// the dir, starts, is declared live, and fails `EACCES` the first time it runs
/// `cs evolve` — a hang the operator cannot tell apart from the trust-dialog
/// one. Checking it is cheap (`stat(2)` on a handful of paths) and the refusal
/// happens before a live worker exists.
///
/// The `Demote*` names on the shared types are historical: they describe
/// *resource kinds*, not the root path, and both call sites now ask the same
/// question about whichever uid the worker will hold.
#[must_use]
pub fn as_is_reachability_refusal(
    running_uid: u32,
    config_home: Option<&Path>,
    worktree: &Path,
    state_dirs: &[PathBuf],
) -> Option<DemoteResourceAccess> {
    // No consent files are declared here on purpose. This is the *non-root*
    // door: whoever wrote those files wrote them as the very uid that is about
    // to read them, so there is no ownership gap to find. The gap this closes
    // is a repo whose `.cosmon/` was built as root and then handed to
    // `USER 10001`, which is a directory question.
    //
    // The git plumbing IS asked about here, unlike the consent files: an image
    // built as root leaves `.git/` root-owned exactly as it leaves `.cosmon/`
    // root-owned, and a non-root worker that cannot write it hits the same
    // wrote-it-cannot-commit end state the demote path just had to fix. Nothing
    // is repaired on this door — an unprivileged dispatcher cannot chown — so
    // the refusal names the path and the remedy is the operator's.
    demote_resource_checks(
        running_uid,
        config_home,
        worktree,
        state_dirs,
        &git_plumbing_paths(worktree),
        &[],
        // The binary is a dispatcher-side fact and this door has no demotion:
        // whatever `PATH` resolves for the running uid is what it will exec, so
        // there is no second identity for the answer to differ under.
        None,
    )
    .into_iter()
    .find(|c| !c.usable)
}

/// What can be known about a root dispatch **before** cosmon writes anything
/// into the operator's Claude Code config.
///
/// # Why this exists (issue #20, the consent-ownership door)
///
/// Two orderings are both load-bearing and they pull against each other:
///
/// - a dispatch cosmon is about to refuse must leave **no trace** in the
///   operator's config, so the refusals come before the consent pre-grant;
/// - the ownership repair must come **after** the pre-grant, or it hands over
///   files that do not exist yet.
///
/// The repair and the provisioning verdict are one indivisible step
/// ([`provision_and_decide_root_spawn`]), so the verdict cannot precede the
/// write. This function is the part that can: everything decidable from the
/// uids alone. `UnprovisionedTarget` is the one refusal that necessarily
/// arrives after the pre-grant, because it is a fact about paths cosmon has to
/// have written and repaired before it can be asked.
///
/// The return type is deliberately **not** a [`RootSpawnDecision`]: a caller
/// must not be able to spawn on it. The demote arm carries no uid.
#[must_use]
pub fn pre_write_verdict(running_uid: u32, demote_target: Option<u32>) -> PreWriteVerdict {
    match decide_root_spawn(running_uid, demote_target) {
        RootSpawnDecision::Refuse { reason } => PreWriteVerdict::Refuse(reason),
        RootSpawnDecision::SpawnAsIs => PreWriteVerdict::AsIs,
        RootSpawnDecision::Demote { .. } => PreWriteVerdict::DemotePending,
    }
}

/// The outcome of [`pre_write_verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreWriteVerdict {
    /// Refuse now, before the config is touched. Already final.
    Refuse(cosmon_core::root_spawn_policy::RootRefusalReason),
    /// The worker will run as the dispatcher's own uid. Nothing will be
    /// chowned, so [`as_is_reachability_refusal`] is answerable now.
    AsIs,
    /// A demote is on the table and its resources are not judged yet. The
    /// target uid is withheld: the only legitimate next step is to pre-grant
    /// the consent and then call [`provision_and_decide_root_spawn`], which
    /// repairs and judges in one gesture.
    ///
    /// **Unreachable.** `decide_root_spawn` refuses every root dispatch, so a
    /// root dispatcher now yields [`Self::Refuse`] here — which is strictly
    /// better on the ordering this type exists for: the refusal lands before
    /// the consent pre-grant, so it leaves nothing at all in the operator's
    /// config. The variant is kept alongside
    /// [`cosmon_core::root_spawn_policy::RootSpawnDecision::Demote`], for the
    /// same reason.
    DemotePending,
}

/// The **one** entry point every demote call site must use: decide the root
/// spawn, transfer ownership of what the worker writes, then downgrade a
/// `Demote` to a typed refusal when the target still cannot reach what it
/// needs.
///
/// # The ordering this fixes (issue #20)
///
/// The external tester's repro: a freshly nucleated molecule, tackled by a root
/// container with `COSMON_WORKER_UID` set, refused with *"worktree … is not
/// usable by it — chown the worktree to the uid before tackling"*. But
/// `cs tackle` is what **creates** that worktree; there is no "before" in which
/// an operator could have chowned it. The guard was right and fail-closed; the
/// order of operations was wrong. So the transfer happens here, on the demote
/// path, after the worktree exists and before any live worker does — and the
/// guard still runs after it, unchanged, so a transfer that silently failed
/// (read-only mount, ACL, a uid absent from the host) still refuses rather than
/// spawning a worker that wedges on `EACCES`.
///
/// # The second half of the same hole (issue #20, consent ownership)
///
/// Three resources were judged and only two repaired: `config_home` was in the
/// judge list and in no repair list. The asymmetry was invisible because the
/// judge answered *yes* — it stats the directory, which is worker-owned, and
/// never looked at the `.claude.json` cosmon had just written into it as root.
/// The worker then could not read that file, Claude Code took it for a first
/// run, replaced it, and rendered the onboarding wizard: a green gate over a
/// broken state. The consent files are now in **both** lists, and the config
/// home stays in the judge list only, by an argued decision rather than by
/// omission.
///
/// A chown error is intentionally swallowed: the *checks* are the verdict. The
/// operator wants to know that the uid cannot write the path, not which errno
/// the repair attempt returned.
///
/// Non-demote decisions never touch the filesystem — neither to chown nor to
/// `stat`. Any dispatch that reaches a live worker must route through here;
/// calling [`decide_root_spawn`] directly is the A3 defect.
#[must_use]
pub fn provision_and_decide_root_spawn(
    running_uid: u32,
    demote_target: Option<u32>,
    resources: &DemoteResources,
) -> RootSpawnDecision {
    decide_provisioned_with(running_uid, demote_target, |to_uid| {
        provision_demote_resources(to_uid, resources)
    })
}

/// Transfer ownership of everything the demoted worker writes, then judge what
/// it can reach — the body of the demote arm, as one indivisible step.
///
/// Repair and judge are one function on purpose: a resource that is judged and
/// not repaired is the exact shape of issue #20, three times over. The two
/// lists are literally the same list.
///
/// # Dormant, and public anyway
///
/// No live dispatch reaches this. `decide_root_spawn` refuses every root
/// dispatch — the hand-over it would need is repository-wide destructive
/// authority — so [`provision_and_decide_root_spawn`] never enters the arm this
/// function is. It stays public for two reasons, and neither is "somebody might
/// want it":
///
/// - it is the substrate of the bounded per-worker ref/object lifecycle, and
///   deleting the only measured description of what a demoted worker needs
///   would mean rediscovering it a fifth time;
/// - the tests that characterise the grant have to be able to *build* it, and
///   a test that can only reach the machinery through a policy that refuses it
///   measures the refusal instead. That substitution is how three green suites
///   sat on top of an open hole in this very lineage.
///
/// It returns [`DemoteResourceAccess`] and **not** a
/// [`RootSpawnDecision`], for the same reason [`PreWriteVerdict`] does not:
/// no caller can spawn on the return value. Only
/// [`provision_and_decide_root_spawn`] produces a decision, and only
/// `decide_root_spawn` decides.
#[must_use]
pub fn provision_demote_resources(
    to_uid: u32,
    resources: &DemoteResources,
) -> Vec<DemoteResourceAccess> {
    {
        // Counted before anything is examined: entering the compatibility
        // machinery at all is what the nominal path must never do, whether or
        // not a single `chown` turns out to be needed.
        record_repair_entry(to_uid);
        // Repair first…
        transfer_best_effort(&resources.worktree, to_uid);
        for dir in &resources.state_dirs {
            transfer_best_effort(dir, to_uid);
        }
        // …including the files cosmon wrote into the config home. Not the
        // config home itself: see `DemoteResources::config_home` for why the
        // line is drawn at authorship, and never `.credentials.json`, which is
        // never named here and never opened.
        for file in &resources.consent_files {
            transfer_best_effort(file, to_uid);
        }
        // …including the git plumbing the worktree commits *through*, which is
        // outside the worktree by construction, so the transfer above never
        // reached it (issue #20, third instance). These are the narrowed
        // entries of `SHARED_COMMIT_SUBPATHS`, never the common dir itself.
        for path in &resources.git_plumbing {
            // Create before transferring. `logs/refs/heads` is absent in a repo
            // whose refs have never moved, and a missing path makes the judge
            // fall back to its nearest existing ancestor — which here is the
            // common dir, the one thing this narrowing exists to keep. Cosmon
            // would then refuse a perfectly good dispatch on the ownership of a
            // directory it deliberately did not ask for.
            ensure_plumbing_dir(path);
            transfer_best_effort(path, to_uid);
        }
        // Handing the plumbing over is what makes the DISPATCHER the foreign
        // uid, so the dispatcher — not the worker — is the one that now needs
        // git's ownership exemption. See
        // `exempt_dispatcher_from_ownership_check` for why the worker
        // deliberately never gets one.
        //
        // Keyed on the *real* effective uid, not on the `running_uid`
        // parameter: this writes the dispatcher's own global git config, which
        // is a fact about the process, not about the decision being modelled.
        // A test drives the root branch by passing `running_uid = 0` on an
        // ordinary account (that injection is what makes the root path testable
        // at all — defect ND3), and it must not thereby edit the developer's
        // `~/.gitconfig`.
        if !resources.git_plumbing.is_empty() && nix::unistd::Uid::effective().is_root() {
            let _ = exempt_dispatcher_from_ownership_check(&ownership_exemption_paths(
                &resources.worktree,
            ));
        }

        // …then judge. Never the reverse, and never one without the other —
        // and the two lists must be THE SAME LIST. A resource that is judged
        // and not repaired is the shape of this whole issue: the judge answers
        // yes about the directory while the file under it is unopenable.
        demote_resource_checks(
            to_uid,
            resources.config_home.as_deref(),
            &resources.worktree,
            &resources.state_dirs,
            &resources.git_plumbing,
            &resources.consent_files,
            resources.worker_binary.as_deref(),
        )
    }
}

/// The ordering skeleton of [`provision_and_decide_root_spawn`], with the
/// filesystem work injected.
///
/// Deliberately **private**. It exists so the "a non-demote decision touches no
/// filesystem at all" property stays observable in a unit test — the closure
/// records whether it ran — without exposing a public entry point that lets a
/// caller judge the paths while forgetting to repair them. That forgettable
/// step is the whole shape of defect A3, and of issue #20 after it.
fn decide_provisioned_with<F>(
    running_uid: u32,
    demote_target: Option<u32>,
    act_and_check: F,
) -> RootSpawnDecision
where
    F: FnOnce(u32) -> Vec<DemoteResourceAccess>,
{
    repair_and_enforce(decide_root_spawn(running_uid, demote_target), act_and_check)
}

/// The repair-then-judge half of [`decide_provisioned_with`], taking the
/// decision rather than computing it.
///
/// Split out for one reason, and it is a testing reason worth stating: since
/// [`decide_root_spawn`] refuses every root dispatch (it will not hand a worker
/// the repository's shared object store and refs), no input to
/// [`decide_provisioned_with`] reaches the `Demote` arm any more. Feeding the
/// old tests a refusal would keep them green while measuring nothing — they
/// would assert "a refusal refuses". So the dormant repair path is exercised by
/// handing this function the decision explicitly, which is honest about what is
/// being tested: the machinery, not the policy that no longer selects it.
///
/// Still private, and still the only way in: a caller cannot judge the paths
/// while forgetting to repair them, which is the shape of defect A3.
fn repair_and_enforce<F>(decision: RootSpawnDecision, act_and_check: F) -> RootSpawnDecision
where
    F: FnOnce(u32) -> Vec<DemoteResourceAccess>,
{
    match decision {
        RootSpawnDecision::Demote { to_uid } => {
            enforce_demote_provisioning(decision, &act_and_check(to_uid))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use cosmon_core::root_spawn_policy::{RootRefusalReason, CONVENTIONAL_WORKER_UID};
    use tempfile::TempDir;

    use super::*;

    /// A uid that owns nothing on any test host, so `other` bits decide.
    const FOREIGN: u32 = 4_294_967_000;

    /// Drive the **dormant** demote arm — repair, then judge — with the
    /// decision supplied instead of decided.
    ///
    /// `decide_root_spawn` refuses every root dispatch now, so
    /// `provision_and_decide_root_spawn(0, Some(uid), …)` returns a refusal
    /// without touching a file. Every test below that asks "which resource does
    /// the judge name?" would then pass while measuring nothing — the answer
    /// would always be the same refusal, whatever the paths look like. So they
    /// enter the arm explicitly. What they test is unchanged: the repair and
    /// the judge. What they no longer claim is that a live dispatch gets here;
    /// [`a_root_dispatch_refuses_without_touching_the_filesystem`] is the test
    /// that pins the opposite.
    fn dormant_provision(to_uid: u32, resources: &DemoteResources) -> RootSpawnDecision {
        repair_and_enforce(RootSpawnDecision::Demote { to_uid }, |uid| {
            provision_demote_resources(uid, resources)
        })
    }

    /// The property the HIGH finding asked for, at the funnel every live
    /// dispatch goes through: a root dispatcher with a perfectly good demote
    /// target gets a **typed refusal**, no worker, and **no `chown`**.
    ///
    /// # What this does NOT establish
    ///
    /// It says nothing about what `cs tackle` wrote *on the way here*, and
    /// ADR-166 used to read it as if it did ("a refused dispatch leaves no
    /// trace on the filesystem"). It did not: the refusal used to live seven
    /// thousand lines into `cs tackle`, and by the time this funnel was
    /// reached the command had already created the config home's
    /// `.claude.json`, the galaxy's `.worktrees/`, `fleet.json` and the rest —
    /// which is COSMON-DEV #20 as reported against v0.4.0. This test could not
    /// have seen any of it: its whole fixture is one directory in a tempdir.
    ///
    /// The end-to-end property is carried by
    /// `a_refused_root_dispatch_leaves_the_galaxy_and_config_home_byte_identical`
    /// in `crates/cosmon-cli/tests/refused_root_dispatch_leaves_no_residue.rs`,
    /// which snapshots every path under a real galaxy and a real config home
    /// rather than naming one.
    ///
    /// Red before the refusal landed: the funnel returned `Demote { to_uid }`
    /// after chowning the worktree away to that uid.
    ///
    /// The no-write half is asserted by ownership rather than by a spy: the
    /// repair's whole job is to change owners, so an unchanged owner is proof
    /// the repair did not run. A spy could be wired past; `stat(2)` cannot.
    #[test]
    fn a_root_dispatch_refuses_without_touching_the_filesystem() {
        let tmp = TempDir::new().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let owner_before = std::fs::metadata(&worktree).unwrap().uid();
        assert_ne!(
            owner_before, CONVENTIONAL_WORKER_UID,
            "the fixture must not already be owned by the demote target, or \
             `stat` cannot tell a skipped repair from a no-op one",
        );

        let decision = provision_and_decide_root_spawn(
            0,
            Some(CONVENTIONAL_WORKER_UID),
            &DemoteResources {
                worktree: worktree.clone(),
                ..DemoteResources::default()
            },
        );

        assert_eq!(
            decision,
            RootSpawnDecision::Refuse {
                reason: RootRefusalReason::DemoteSharesRepositoryStorage {
                    uid: CONVENTIONAL_WORKER_UID,
                },
            },
            "a root dispatcher must not be handed a demotion it can only make \
             work by giving away the repository's shared objects and refs",
        );
        assert_eq!(
            std::fs::metadata(&worktree).unwrap().uid(),
            owner_before,
            "the worktree was chowned on a dispatch that refused",
        );
        // Deliberately NOT asserted through `ownership_transfers_attempted()`:
        // that counter is per *process* and every test in this binary shares
        // it, so a parallel neighbour makes the reading non-deterministic. A
        // flaky witness for a security property is the MEDIUM finding of this
        // same round, and it is not reintroduced here. `stat` is deterministic
        // and, given the assertion above, decisive.
    }

    /// Issue #20 point 2 — the non-root twin of the demote provisioning check.
    ///
    /// Env-free companion to the call-site test in `claude.rs`: every input is
    /// explicit, so this pins *which* resource the refusal names rather than
    /// merely that one fired.
    #[test]
    fn as_is_refusal_names_the_unreachable_state_dir() {
        use std::os::unix::fs::PermissionsExt as _;

        // Rooted in `/tmp`, not the per-user temp dir. `path_usable_by_uid`
        // walks every ancestor for the search bit, and macOS puts the default
        // temp dir under a 0700 `/var/folders/<user>/…`: below that, EVERY path
        // is unreachable by a foreign uid, so the first-unusable-wins verdict
        // would name the config home and this test would pass without ever
        // exercising the state-dir case. `/tmp` is world-traversable on both
        // macOS and Linux, which is what makes the one closed directory below
        // the only reason for the refusal.
        let tmp = TempDir::new_in("/tmp").unwrap();
        let config_home = tmp.path().join("cfg");
        let worktree = tmp.path().join("worktree");
        let state_dir = tmp.path().join("main").join(".cosmon");
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        for d in [&config_home, &worktree, &state_dir] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
        std::fs::set_permissions(
            state_dir.parent().expect("main/"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        // Only the out-of-worktree state dir is closed to the worker's uid —
        // exactly the container shape: image built as root, `USER 10001` after.
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let blocked = as_is_reachability_refusal(
            FOREIGN,
            Some(&config_home),
            &worktree,
            std::slice::from_ref(&state_dir),
        )
        .expect("an unwritable granted state dir must be refused");
        assert_eq!(blocked.resource, DemoteResource::StateDir);
        assert_eq!(blocked.path, state_dir.to_string_lossy().into_owned());
    }

    /// The normal fleet dispatch must not be refused: when every resource is
    /// reachable by the running uid there is no verdict to report. A check that
    /// cried wolf here would ground the whole fleet.
    #[test]
    fn as_is_refusal_is_silent_when_everything_is_reachable() {
        let tmp = TempDir::new().unwrap();
        let worktree = tmp.path().join("worktree");
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        assert!(as_is_reachability_refusal(
            nix::unistd::Uid::effective().as_raw(),
            None,
            &worktree,
            std::slice::from_ref(&state_dir),
        )
        .is_none());
    }

    /// COSMON-DEV #20 defect A3, iteration 2 — the surviving call site, frozen.
    ///
    /// This is the transport-side twin of the interactive-tackle test. A root
    /// dispatcher (`cs thaw`, patrol respawn) with a valid demote target whose
    /// state dir it cannot write must **refuse, typed, before a live worker
    /// exists** — not demote and let the worker wedge on `EACCES` after the
    /// readiness probe has already called it live.
    #[test]
    fn transport_demote_refuses_when_the_target_cannot_write_the_state_dir() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join(".cosmon");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        // Only the out-of-worktree state dir blocks — the root-owned `.cosmon`
        // shape a root `cs tackle` leaves behind, which `--add-dir` cannot fix.
        // Modelled as read+search but not writable, so the target reaches it and
        // still cannot do the `cs evolve` write.
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
        let target = std::fs::metadata(tmp.path()).unwrap().uid();

        let decision = dormant_provision(
            target,
            &DemoteResources {
                worktree: tmp.path().to_path_buf(),
                state_dirs: vec![state.clone()],
                ..DemoteResources::default()
            },
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
                assert_eq!(uid, target);
                assert_eq!(resource, DemoteResource::StateDir);
                assert!(
                    path.contains(".cosmon"),
                    "must name the blocked path: {path}"
                );
            }
            other => panic!("expected a typed provisioning refusal, got {other:?}"),
        }
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The reviewers' second surviving scenario: a config home the target can
    /// write and search but **not read** (`0300`). `claude` reads its
    /// credentials from there, so a write-only verdict is a start-then-EACCES.
    #[test]
    fn a_write_only_config_home_is_not_usable_because_credentials_are_read() {
        let tmp = TempDir::new().unwrap();
        let owner = std::fs::metadata(tmp.path()).unwrap().uid();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o300)).unwrap();

        assert!(
            path_usable_by_uid(tmp.path(), owner, RequiredAccess::Write),
            "0300 is writable+searchable, so a worktree-style check passes",
        );
        assert!(
            !path_usable_by_uid(tmp.path(), owner, RequiredAccess::ReadWrite),
            "0300 cannot be READ, so a credential home must not read as usable",
        );

        // And it reaches the decision: a 0300 config home refuses the demote.
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let home = tmp.path().join("dot-claude");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o300)).unwrap();
        let decision = dormant_provision(
            owner,
            &DemoteResources {
                config_home: Some(home.clone()),
                worktree: tmp.path().to_path_buf(),
                ..DemoteResources::default()
            },
        );
        assert!(
            matches!(
                decision,
                RootSpawnDecision::Refuse {
                    reason: RootRefusalReason::UnprovisionedTarget {
                        resource: DemoteResource::ConfigHome,
                        ..
                    }
                }
            ),
            "a write-only config home must refuse the demote, got {decision:?}",
        );
        // Restore so TempDir can clean up.
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// An unreachable **ancestor** makes a perfectly-moded leaf unreachable.
    /// Probing only the leaf as root hides this entirely.
    #[test]
    fn an_unsearchable_ancestor_makes_a_permissive_leaf_unusable() {
        let tmp = TempDir::new().unwrap();
        let owner = std::fs::metadata(tmp.path()).unwrap().uid();
        let gate = tmp.path().join("gate");
        let leaf = gate.join("worktree");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(path_usable_by_uid(&leaf, owner, RequiredAccess::Write));

        // Close the gate: the leaf is still 0777, but nobody can walk to it.
        std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            !path_usable_by_uid(&leaf, owner, RequiredAccess::Write),
            "a leaf behind an unsearchable ancestor must not read as usable",
        );
        std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The check is not a blanket refusal: a fully provisioned target still
    /// demotes, on the transport path too.
    #[test]
    fn transport_provisioned_demote_still_demotes() {
        let tmp = TempDir::new().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        // The owner, not a foreign uid: the per-user temp root is itself 0700 on
        // macOS, so a foreign uid legitimately cannot traverse to this leaf —
        // that IS the ancestor rule, asserted separately below.
        let owner = std::fs::metadata(tmp.path()).unwrap().uid();
        let decision = dormant_provision(
            owner,
            &DemoteResources {
                worktree: tmp.path().to_path_buf(),
                ..DemoteResources::default()
            },
        );
        assert_eq!(decision, RootSpawnDecision::Demote { to_uid: owner });
    }

    /// The leaf's own bits are not the whole answer, and the check must not be a
    /// blanket refusal either: the same 0777 leaf reads usable for the uid that
    /// can walk to it and unusable for one that cannot.
    #[test]
    fn a_world_writable_leaf_is_judged_together_with_its_chain() {
        let tmp = TempDir::new().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let owner = std::fs::metadata(tmp.path()).unwrap().uid();
        assert!(
            path_usable_by_uid(tmp.path(), owner, RequiredAccess::ReadWrite),
            "a 0777 leaf under a chain the uid can walk is usable",
        );
        let blocked = tmp
            .path()
            .ancestors()
            .find(|a| !has_mode(a, FOREIGN, 0o100))
            .map(Path::to_path_buf);
        if let Some(blocked) = blocked {
            assert!(
                !path_usable_by_uid(tmp.path(), FOREIGN, RequiredAccess::Write),
                "an ancestor ({}) the uid cannot search makes the leaf unusable",
                blocked.display(),
            );
        }
    }

    /// The ownership-transfer instrument counts the **call**, not the result.
    ///
    /// A `chown` onto the owner a path already has leaves nothing a `stat` can
    /// see, so a final-state assertion cannot distinguish "the repair never ran"
    /// from "the repair ran and changed nothing". The container capture's
    /// no-repair claim rests on this counter, so the property it needs is pinned
    /// here: transferring a path to the uid that already owns it still counts.
    #[test]
    fn an_ownership_transfer_is_counted_even_when_it_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let already_ours = std::fs::metadata(tmp.path()).unwrap().uid();

        let before = ownership_transfers_attempted();
        // The io result is deliberately discarded: the instrument must be
        // independent of whether the syscall was needed, permitted, or a no-op.
        // (On macOS this call also asks for a gid change the test account may
        // not be allowed to make — precisely a case where the effect is nil and
        // the traversal still happened.)
        let _ = chown_tree_to_uid(tmp.path(), already_ours);
        // A monotone comparison, not an equality: the counter is process-wide
        // and this test binary runs its tests concurrently.
        assert!(
            ownership_transfers_attempted() > before,
            "a transfer with no visible effect must still be observable — \
             otherwise the capture's `no repair fired` claim is unfalsifiable"
        );
    }

    /// Entering the repair path counts even when there is nothing to repair.
    ///
    /// The demote arm below declares no state dirs, no consent files and a
    /// worktree that is already the running uid's, so every individual transfer
    /// is a no-op. A counter placed after such a guard would read zero over a run
    /// that did traverse the compatibility mechanism — the one reading the
    /// container capture must be unable to produce.
    #[test]
    fn entering_the_repair_path_counts_even_with_nothing_to_repair() {
        let tmp = TempDir::new_in("/tmp").unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let owner = std::fs::metadata(tmp.path()).unwrap().uid();
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::set_permissions(&worktree, std::fs::Permissions::from_mode(0o777)).unwrap();
        let resources = DemoteResources::for_dispatch(&worktree, vec![], None, vec![], None);

        let before = ownership_transfers_attempted();
        // `running_uid = 0` is the injection that makes the root path testable
        // without privilege; the demote target is the account running the test,
        // which already owns everything named above.
        assert_eq!(
            dormant_provision(owner, &resources),
            RootSpawnDecision::Demote { to_uid: owner },
        );
        assert!(
            ownership_transfers_attempted() > before,
            "traversing the repair path must be observable on its own"
        );

        // The complementary half — that the nominal path does not traverse it —
        // is `non_root_never_probes_the_filesystem`, asserted on the injected
        // closure rather than on this counter. The counter is process-wide and
        // this test binary runs its tests concurrently, so an equality read here
        // would be another test's increment away from flaking; the closure flag
        // is local to the call and cannot be.
    }

    /// The non-root fleet path never touches the filesystem — neither to chown
    /// nor to `stat`. Asserted on the private skeleton, which is where the
    /// filesystem work is injectable.
    #[test]
    fn non_root_never_probes_the_filesystem() {
        let probed = std::cell::Cell::new(false);
        let decision = decide_provisioned_with(1000, Some(FOREIGN), |_| {
            probed.set(true);
            vec![]
        });
        assert_eq!(decision, RootSpawnDecision::SpawnAsIs);
        assert!(!probed.get(), "a non-root dispatch must not stat anything");
    }

    /// A refusal decided upstream (no non-root target) passes through untouched
    /// and, likewise, never probes.
    #[test]
    fn a_root_refusal_passes_through_without_probing() {
        let probed = std::cell::Cell::new(false);
        let decision = decide_provisioned_with(0, None, |_| {
            probed.set(true);
            vec![]
        });
        assert!(matches!(
            decision,
            RootSpawnDecision::Refuse {
                reason: RootRefusalReason::NoNonRootTarget
            }
        ));
        assert!(!probed.get());
    }

    /// Build a repository with a **linked worktree**, the shape `cs tackle`
    /// creates, and return `(tempdir, repo, worktree)`.
    ///
    /// `None` when git is unavailable or refuses — the test that uses it then
    /// skips rather than passing vacuously.
    fn repo_with_linked_worktree() -> Option<(TempDir, PathBuf, PathBuf)> {
        let tmp = TempDir::new_in("/tmp").ok()?;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).ok()?;
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).ok()?;
        let git = |args: &[&str], cwd: &Path| -> bool {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&["init", "-q", "-b", "main"], &repo)
            || !git(&["commit", "-qm", "root", "--allow-empty"], &repo)
            || !git(&["worktree", "add", "-q", "-b", "wt", "../wt"], &repo)
        {
            return None;
        }
        // Canonicalised: git records the real path in the worktree's `.git`
        // pointer, and on macOS `/tmp` is a symlink to `/private/tmp`, so the
        // paths this returns must be the ones git wrote or the comparison
        // below would be about symlinks rather than about the fix.
        let worktree = tmp.path().join("wt").canonicalize().ok()?;
        let repo = repo.canonicalize().ok()?;
        Some((tmp, repo, worktree))
    }

    /// Attempt a real commit in `worktree`. The property under test is stated
    /// in git's own terms, not in cosmon's.
    fn can_commit(worktree: &Path) -> bool {
        std::process::Command::new("git")
            .args(["commit", "-qm", "probe", "--allow-empty"])
            .current_dir(worktree)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Issue #20, third instance — **the property that broke**: after
    /// provisioning, the demote target can *commit* in its worktree.
    ///
    /// # Why it is written this way
    ///
    /// The two obvious assertions both pass against the bug and are therefore
    /// worthless: "a chown was called" (it was — on the worktree) and "the
    /// worktree directory is writable" (it is — that was repaired two fixes
    /// ago). The tester's workers hit neither; they wrote their artefact into a
    /// perfectly writable worktree and then could not record it, because a
    /// linked worktree commits through `<repo>/.git/worktrees/<name>` and the
    /// object store, neither of which is inside it.
    ///
    /// So the test asserts cosmon's verdict **tracks git's own answer**, in both
    /// directions, over the one state that differs: the gitdir writable or not.
    /// Ownership is not the lever here — a test cannot chown to a foreign uid
    /// without being root — but the mode bit produces the same end state the
    /// tester measured, `git` unable to write the plumbing, and it is that end
    /// state the gate has to catch.
    ///
    /// Against the code before this fix the second half fails: the git plumbing
    /// is in neither list, so a repo git demonstrably cannot commit in demotes
    /// cleanly, which is the bug wearing a green gate.
    #[test]
    fn a_demote_is_refused_exactly_when_the_target_cannot_commit() {
        let Some((_tmp, repo, worktree)) = repo_with_linked_worktree() else {
            eprintln!("skipped: git unavailable");
            return;
        };
        // `git worktree add` names the gitdir after the worktree directory.
        let gitdir = repo.join(".git").join("worktrees").join("wt");
        assert!(
            gitdir.is_dir(),
            "expected a linked-worktree gitdir at {gitdir:?}"
        );
        let target = std::fs::metadata(&worktree).expect("worktree").uid();

        // Baseline: a healthy checkout commits, and cosmon lets the demote run.
        assert!(can_commit(&worktree), "the baseline worktree must commit");
        let resources = DemoteResources::for_dispatch(&worktree, vec![], None, vec![], None);
        assert!(
            resources.git_plumbing.contains(&gitdir),
            "the enumeration must find the linked worktree's gitdir, got {:?}",
            resources.git_plumbing,
        );
        assert_eq!(
            dormant_provision(target, &resources),
            RootSpawnDecision::Demote { to_uid: target },
            "a fully provisioned demote must still demote",
        );

        // Now the tester's end state: the worktree is untouched and perfectly
        // usable, and the plumbing it commits through is not writable.
        std::fs::set_permissions(&gitdir, std::fs::Permissions::from_mode(0o500))
            .expect("close the gitdir");
        assert!(
            path_usable_by_uid(&worktree, target, RequiredAccess::Write),
            "the worktree itself stays writable — that is what made this invisible",
        );
        assert!(
            !can_commit(&worktree),
            "precondition: git must be unable to commit through a read-only gitdir",
        );

        let decision = dormant_provision(target, &resources);
        std::fs::set_permissions(&gitdir, std::fs::Permissions::from_mode(0o700))
            .expect("restore for cleanup");
        match decision {
            RootSpawnDecision::Refuse {
                reason:
                    RootRefusalReason::UnprovisionedTarget {
                        resource: DemoteResource::GitPlumbing,
                        ref path,
                        ..
                    },
            } => assert!(
                path.contains("worktrees"),
                "the refusal must name the plumbing, got {path}"
            ),
            other => panic!(
                "a worker that cannot commit must be refused before it spawns, got {other:?}"
            ),
        }
    }

    /// The enumeration reads git's pointers rather than assuming a layout: a
    /// plain checkout's `.git` is itself the gitdir, and there is no second
    /// path to hand over.
    #[test]
    fn a_plain_checkout_enumerates_its_own_dot_git() {
        let tmp = TempDir::new().unwrap();
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(dot_git.join("objects")).unwrap();
        assert_eq!(git_plumbing_paths(tmp.path()), vec![dot_git]);
    }

    /// A `.git` **file** is followed to its target, and the `commondir` beside
    /// it is *entered* — not appended. Both pointers are relative in the layout
    /// git writes, and the `..` components are collapsed textually so a chown
    /// cannot be redirected through a symlink.
    ///
    /// This assertion used to read `vec![gitdir, common]`, and that was the
    /// finding: the common dir handed to a recursive chown is the repository's
    /// `config`, `hooks/` and every other worktree's plumbing. The list is now
    /// the gitdir plus the three subpaths a commit writes, and the negative
    /// half below is the load-bearing one.
    #[test]
    fn a_linked_worktree_enters_the_common_dir_instead_of_taking_it() {
        let tmp = TempDir::new().unwrap();
        let common = tmp.path().join("repo").join(".git");
        let gitdir = common.join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let paths = git_plumbing_paths(&worktree);
        assert_eq!(
            paths,
            vec![
                gitdir.clone(),
                common.join("objects"),
                common.join("refs/heads"),
                common.join("logs/refs/heads"),
            ],
        );
        assert!(
            !paths.contains(&common),
            "the common dir itself must never be handed to a recursive chown: {paths:?}",
        );
        // The gitdir is per-worktree and stays a whole-tree transfer; nothing
        // that the dispatcher later *executes* or *reads as configuration* may
        // be under any of these.
        for forbidden in ["config", "hooks", "worktrees", "modules", "info"] {
            let path = common.join(forbidden);
            assert!(
                !paths.iter().any(|p| p == &path || path.starts_with(p)),
                "`{forbidden}` must stay with the dispatcher: {paths:?}",
            );
        }
    }

    /// The adapter binary is judged against the target uid, and the ancestor
    /// walk is where the value is: the tester's own recipe has to `chmod o+x
    /// /root` because `claude` installs under a 0700 home. A worker that cannot
    /// traverse to its binary execs nothing, and the pane looks alive.
    ///
    /// Judged, never repaired — so the assertion is that the refusal *names* it,
    /// which is the whole difference between an operator who knows what to
    /// chmod and one watching a silent pane.
    #[test]
    fn an_unreachable_adapter_binary_refuses_before_the_pane_exists() {
        let tmp = TempDir::new_in("/tmp").unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let owner = std::fs::metadata(tmp.path()).unwrap().uid();
        let home = tmp.path().join("home");
        let bin = home.join("claude");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        // Open to everyone, so the first-unusable-wins verdict below can only
        // be about the binary — a worktree a foreign uid cannot write would
        // shadow it and the test would pass without exercising anything.
        std::fs::set_permissions(&worktree, std::fs::Permissions::from_mode(0o777)).unwrap();

        let build =
            |b: &str| DemoteResources::for_dispatch(&worktree, vec![], None, vec![], Some(b));
        let bin_str = bin.to_string_lossy().into_owned();
        assert_eq!(
            dormant_provision(owner, &build(&bin_str)),
            RootSpawnDecision::Demote { to_uid: owner },
            "a reachable binary must not ground the dispatch",
        );

        // The installer's 0700 home: the binary is still 0755 and still there.
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let decision = dormant_provision(FOREIGN, &build(&bin_str));
        assert!(
            matches!(
                decision,
                RootSpawnDecision::Refuse {
                    reason: RootRefusalReason::UnprovisionedTarget {
                        resource: DemoteResource::WorkerBinary,
                        ..
                    }
                }
            ),
            "a binary the target cannot traverse to must be named, got {decision:?}",
        );
    }

    /// A binary cosmon cannot locate is one it must not blame: an unresolvable
    /// name declares nothing rather than refusing on a guessed path.
    #[test]
    fn an_unresolvable_binary_name_is_declared_as_nothing() {
        let tmp = TempDir::new().unwrap();
        let resources = DemoteResources::for_dispatch(
            tmp.path(),
            vec![],
            None,
            vec![],
            Some("cosmon-no-such-adapter-binary"),
        );
        assert_eq!(resources.worker_binary, None);
    }

    /// No repository, no plumbing — and no panic. A caller outside a git repo
    /// declares nothing rather than having a guessed `.git` chowned into
    /// existence.
    #[test]
    fn a_directory_outside_any_repository_enumerates_nothing() {
        let tmp = TempDir::new().unwrap();
        assert!(git_plumbing_paths(tmp.path()).is_empty());
    }

    #[test]
    fn config_home_falls_back_to_home_dot_claude() {
        assert_eq!(
            demote_config_home(Some("/explicit"), |_| None),
            Some(PathBuf::from("/explicit")),
        );
        assert_eq!(
            demote_config_home(None, |k| (k == "HOME").then(|| "/root".to_owned())),
            Some(PathBuf::from("/root/.claude")),
        );
        assert_eq!(demote_config_home(None, |_| None), None);
    }
}
