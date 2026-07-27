// SPDX-License-Identifier: AGPL-3.0-only

//! The filesystem port behind
//! [`cosmon_core::root_spawn_policy::enforce_demote_provisioning`]
//! — COSMON-DEV #20 defect A3.
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

use std::path::{Path, PathBuf};

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
}

impl RequiredAccess {
    /// The owner-triple mode mask this access needs; shifted per class in
    /// [`has_mode`] for a non-owning uid.
    const fn owner_mask(self) -> u32 {
        match self {
            Self::Write => 0o300,
            Self::ReadWrite => 0o700,
            Self::ReadWriteFile => 0o600,
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
#[must_use]
pub fn demote_resource_checks(
    uid: u32,
    config_home: Option<&Path>,
    worktree: &Path,
    state_dirs: &[PathBuf],
    consent_files: &[PathBuf],
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    demote_resource_checks(running_uid, config_home, worktree, state_dirs, &[])
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
        // Repair first…
        let _ = chown_tree_to_uid(&resources.worktree, to_uid);
        for dir in &resources.state_dirs {
            let _ = chown_tree_to_uid(dir, to_uid);
        }
        // …including the files cosmon wrote into the config home. Not the
        // config home itself: see `DemoteResources::config_home` for why the
        // line is drawn at authorship, and never `.credentials.json`, which is
        // never named here and never opened.
        for file in &resources.consent_files {
            let _ = chown_tree_to_uid(file, to_uid);
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
            &resources.consent_files,
        )
    })
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
    let decision = decide_root_spawn(running_uid, demote_target);
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

    use cosmon_core::root_spawn_policy::RootRefusalReason;
    use tempfile::TempDir;

    use super::*;

    /// A uid that owns nothing on any test host, so `other` bits decide.
    const FOREIGN: u32 = 4_294_967_000;

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

        let decision = provision_and_decide_root_spawn(
            0,
            Some(target),
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
        let decision = provision_and_decide_root_spawn(
            0,
            Some(owner),
            &DemoteResources {
                config_home: Some(home.clone()),
                worktree: tmp.path().to_path_buf(),
                state_dirs: vec![],
                consent_files: vec![],
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
        let decision = provision_and_decide_root_spawn(
            0,
            Some(owner),
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
