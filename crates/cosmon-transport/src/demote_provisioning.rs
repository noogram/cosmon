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
}

impl RequiredAccess {
    /// The owner-triple mode mask this access needs; shifted per class in
    /// [`has_mode`] for a non-owning uid.
    const fn owner_mask(self) -> u32 {
        match self {
            Self::Write => 0o300,
            Self::ReadWrite => 0o700,
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
#[must_use]
pub fn demote_resource_checks(
    uid: u32,
    config_home: Option<&Path>,
    worktree: &Path,
    state_dirs: &[PathBuf],
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

/// The **one** entry point every demote call site must use: decide the root
/// spawn, then downgrade a `Demote` to a typed refusal when the target cannot
/// reach what the worker needs.
///
/// `checks_for` is a closure so the `stat(2)` happens only on the demote path
/// and stays out of the ordering logic. Non-demote decisions never touch the
/// filesystem.
///
/// Any dispatch that reaches a live worker must route through here. Calling
/// [`decide_root_spawn`] directly is the A3 defect.
#[must_use]
pub fn decide_root_spawn_provisioned<F>(
    running_uid: u32,
    demote_target: Option<u32>,
    checks_for: F,
) -> RootSpawnDecision
where
    F: FnOnce(u32) -> Vec<DemoteResourceAccess>,
{
    let decision = decide_root_spawn(running_uid, demote_target);
    match decision {
        RootSpawnDecision::Demote { to_uid } => {
            enforce_demote_provisioning(decision, &checks_for(to_uid))
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

        let decision = decide_root_spawn_provisioned(0, Some(target), |uid| {
            demote_resource_checks(uid, None, tmp.path(), &[state.clone()])
        });

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
        let decision = decide_root_spawn_provisioned(0, Some(owner), |uid| {
            demote_resource_checks(uid, Some(&home), tmp.path(), &[])
        });
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
        let decision = decide_root_spawn_provisioned(0, Some(owner), |uid| {
            demote_resource_checks(uid, None, tmp.path(), &[])
        });
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

    /// The non-root fleet path never touches the filesystem — the provisioning
    /// closure is not even called.
    #[test]
    fn non_root_never_probes_the_filesystem() {
        let probed = std::cell::Cell::new(false);
        let decision = decide_root_spawn_provisioned(1000, Some(FOREIGN), |_| {
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
        let decision = decide_root_spawn_provisioned(0, None, |_| {
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
