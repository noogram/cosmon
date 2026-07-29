// SPDX-License-Identifier: AGPL-3.0-only

//! Root-spawn policy — the I/O-free decision that forbids a live cognitive
//! worker from ever running as root (COSMON-DEV #20 / contract-20A).
//!
//! # The security fault this closes
//!
//! When `cs` runs as **root** (effective uid 0) and dispatches a cognitive
//! worker, three outcomes are conceivable:
//!
//! 1. **Demote** — spawn the worker as a **non-root** uid (the container's
//!    conventional `worker`, uid [`CONVENTIONAL_WORKER_UID`]).
//! 2. **Refuse** — decline to create a live worker **before** one exists,
//!    with a typed root-refusal.
//! 3. *(forbidden)* **spawn a live cognitive worker as uid 0** — an autonomous
//!    LLM with root's entire blast radius.
//!
//! **Outcome 1 is now refused too**, and [`decide_root_spawn`] carries the
//! argument: demoting across uids requires handing the worker the
//! repository's shared object store and shared refs, which is repository-wide
//! destructive authority over every sibling molecule. A root dispatcher
//! therefore always reaches outcome 2, and the way forward it names is the
//! nominal one — run `cs` as the non-root uid the workers run as.
//!
//! The pre-#20 spawn path reached the forbidden outcome: under a bypass
//! permission mode it forced `IS_SANDBOX=1` purely to survive Claude Code's
//! own root guard, *keeping the worker as root*. That optimises to preserve
//! the root bypass — exactly what a security hardening must not do. F8 of the
//! 2026-07-23 dogfooding findings proved empirically that a demoted (non-root)
//! worker runs fine **regardless** of `IS_SANDBOX`, so demotion is the
//! proven-robust fix and the root bypass earns nothing.
//!
//! # Why this is a pure function
//!
//! The real spawn site cannot be unit-tested without actually being root.
//! So the *decision* is factored out as [`decide_root_spawn`], a total
//! function over `(running_uid, demote_target)` that the spawn site consults
//! and that a test can exercise for `running_uid == 0` without any privilege.
//! The load-bearing invariant — **root never resolves to a live root
//! worker** — is then a property of this function, checkable in-process:
//! for `running_uid == 0` the decision is always [`RootSpawnDecision::Refuse`],
//! and [`RootSpawnDecision::SpawnAsIs`] is structurally reachable only for a
//! non-root dispatcher.
//!
//! # What the demote machinery below is now for
//!
//! Everything under this line describes a path [`decide_root_spawn`] no longer
//! takes. It is kept, unmodified in substance, for two reasons: it is the
//! substrate the per-worker ref/object lifecycle will re-enable, and its
//! checks are what an operator sees if that lifecycle ever lands. Read it as
//! documentation of a dormant capability, not of live behaviour.
//!
//! [`enforce_demote_provisioning`] *detects* that a demote target cannot reach
//! its config home, worktree, or state dir and turns that into a typed refusal.
//! It does not **provision** the identity. Making the demote path complete
//! needs three gestures cosmon does not perform today, in this order:
//!
//! 1. **Env rewrite on demote.** `HOME` (and `CLAUDE_CONFIG_DIR` when it points
//!    into root's home) must be re-pointed at a directory the target uid owns,
//!    emitted in the same env prefix as everything else. The demotion prefix
//!    deliberately omits `--reset-env`, so today the worker inherits root's
//!    `HOME=/root` and looks for credentials behind mode 0700.
//! 2. **Credential transfer.** The demoted identity needs a usable Claude
//!    login in that home. Copying root's credentials is one option; mounting
//!    the target uid's own is the better one, and is an operator decision, not
//!    a cosmon default.
//! 3. ~~**Ownership transfer of what the worker writes.**~~ **Landed** (issue
//!    #20 worktree-ownership catch-22). `cs tackle` as root creates the
//!    worktree and `.cosmon/state/` root-owned, and the demoted worker must own
//!    both or its own `cs evolve` / `cs complete` fail — `--add-dir` cannot
//!    help, it is a Claude authorization grant, not an OS `chown`. The demote
//!    path now performs that `chown` itself, between worktree creation and the
//!    spawn, in `cosmon_transport::demote_provisioning::provision_and_decide_root_spawn`
//!    (named, not linked: the domain core does not depend on the transport).
//!    The check below still runs **after** the chown and still refuses when it
//!    did not take: ordering was the bug, the guard was not.
//!
//! Until the remaining two land, a root dispatcher on an unprovisioned host
//! refuses with
//! [`RootRefusalReason::UnprovisionedTarget`] naming the path and the remedy.
//! That is strictly better than the pre-A3 behaviour (start, look live, wedge
//! on `EACCES`), and strictly less than a working root-container path.

/// The conventional non-root uid a demoted cognitive worker runs as.
///
/// Matches the `worker` user baked into the cosmon-dev clean-room image
/// (`spores/cosmon-dev/clean-room`) and the uid F8 verified runs a live
/// worker cleanly with and without `IS_SANDBOX`. The demote target is
/// configurable at the spawn site (see [`resolve_demote_target`]); this is
/// the default when the operator pins nothing.
pub const CONVENTIONAL_WORKER_UID: u32 = 10001;

/// Why a root dispatch refused to create a worker at all.
///
/// A refusal is the *fallback* outcome (contract-20A outcome 2), taken only
/// when demotion is impossible in the environment. It is a **typed** verdict,
/// never a silent no-op: the spawn site records it before returning so an
/// audit can tell a deliberate root-refusal apart from a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootRefusalReason {
    /// The dispatcher is root and no non-root demote target is available
    /// (the operator disabled demotion, or pinned the target back to uid 0).
    /// Spawning would produce a live root worker, so cosmon refuses instead.
    NoNonRootTarget,
    /// Demotion is possible, but the target uid cannot reach something the
    /// worker provably needs — its Claude config home, its worktree, or the
    /// out-of-worktree cosmon state it writes on `cs evolve` / `cs complete`.
    /// Spawning would produce a live worker that wedges on `EACCES` partway
    /// through, so cosmon refuses up front and says which path is the problem.
    UnprovisionedTarget {
        /// The uid the worker would have been demoted to.
        uid: u32,
        /// What the path is *for* — see [`DemoteResource`].
        resource: DemoteResource,
        /// The path the target uid cannot use.
        path: String,
    },
    /// Demotion is possible and would even work — and cosmon refuses anyway,
    /// because the hand-over it requires grants the demoted worker write
    /// authority over storage the **whole repository** shares.
    ///
    /// See [`decide_root_spawn`] for why this is a refusal of the path rather
    /// than a fourth narrowing of the grant.
    DemoteSharesRepositoryStorage {
        /// The uid the worker would have been demoted to. Carried so the
        /// refusal can name the nominal invocation — *be* this uid — rather
        /// than describing it in the abstract.
        uid: u32,
    },
}

/// What a path the demoted worker needs is *for*.
///
/// Named rather than free-text so the refusal message tells an operator which
/// provisioning step is missing, not merely that some path failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoteResource {
    /// The Claude config home the worker authenticates from (`CLAUDE_CONFIG_DIR`,
    /// or `$HOME/.claude`). Root's `HOME=/root` is mode 0700 and root-owned, so
    /// a worker demoted with the environment preserved looks for credentials it
    /// cannot read.
    ConfigHome,
    /// The git worktree the worker runs in. `cs tackle` as root creates it
    /// root-owned.
    Worktree,
    /// The out-of-worktree `.cosmon/` the worker writes on `cs evolve` /
    /// `cs complete`. `--add-dir` is a Claude *authorization* grant, not an OS
    /// `chown` — it cannot override `EACCES`.
    StateDir,
    /// One of the startup-consent files **cosmon itself wrote** into the config
    /// home before the spawn (`.claude.json`, `settings.json`).
    ///
    /// A root dispatcher writes them as root; the worker then opens them as the
    /// demote target. When it cannot, Claude Code does not fail — it concludes
    /// it is on a first run and *replaces* `.claude.json` wholesale, losing the
    /// pre-grant and rendering the onboarding wizard nobody is there to answer.
    /// The containing directory being usable says nothing about this: measured
    /// on 2.1.220, a worker-owned config home holding a root-owned
    /// `.claude.json` reproduces the hang exactly.
    ConsentFile,
    /// The git plumbing the worktree records commits *through* — the linked
    /// worktree's own gitdir (`<repo>/.git/worktrees/<name>`: HEAD, index,
    /// logs, `ORIG_HEAD`) and the repository's common dir (`<repo>/.git`:
    /// `objects/`, `refs/`, `logs/`, `packed-refs`, and the lock files git
    /// creates beside them).
    ///
    /// A linked worktree keeps almost nothing under the worktree directory —
    /// only a `.git` *file* pointing elsewhere. So a demote that chowns the
    /// worktree and stops there produces a worker that can edit every file and
    /// record none of them: `git add` fails on the index it cannot write, and
    /// git refuses the repository outright as *dubious ownership* because it
    /// resolves the gitdir to a directory owned by another uid. Measured by the
    /// external tester on issue #20 — two dispatches wrote their artefact and
    /// neither could commit.
    GitPlumbing,
    /// The adapter binary the worker execs — `claude`, resolved from the
    /// dispatcher's `PATH` or named explicitly.
    ///
    /// Judged and never repaired, which is the point of naming it: it belongs
    /// to whoever installed it, and cosmon taking ownership of an interpreter
    /// on the host would be a far worse default than refusing. The external
    /// tester's own recipe carries `chmod o+x /root && chmod -R o+rX
    /// /root/.local`, because the installer puts `claude` under a `0700` home
    /// that a demoted worker cannot traverse. Without that line the worker
    /// spawns into a pane whose command is not runnable — a dispatch that
    /// costs a molecule slot and produces nothing.
    WorkerBinary,
}

impl DemoteResource {
    /// A short human label used in the refusal message.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DemoteResource::ConfigHome => "claude config home",
            DemoteResource::Worktree => "worktree",
            DemoteResource::StateDir => "cosmon state dir",
            DemoteResource::ConsentFile => "claude startup-consent file",
            DemoteResource::GitPlumbing => "git plumbing the worktree commits through",
            DemoteResource::WorkerBinary => "adapter binary the worker execs",
        }
    }

    /// The concrete provisioning gesture that fixes this resource.
    #[must_use]
    pub fn remedy(self) -> &'static str {
        match self {
            DemoteResource::ConfigHome => {
                "point CLAUDE_CONFIG_DIR at a directory the uid owns (and set \
                 HOME accordingly), or run cs as that uid"
            }
            // NOT "chown it before tackling": `cs tackle` is what CREATES the
            // worktree, so for a freshly nucleated molecule that advice is
            // structurally impossible to follow (issue #20 catch-22). The
            // demote path now chowns both paths itself, so a refusal here means
            // the chown did not take.
            DemoteResource::Worktree => {
                "cosmon chowns the worktree to the uid on the demote path, so \
                 this one resisted it — check for a read-only mount, an ACL, a \
                 parent directory the uid cannot search, or a uid that does not \
                 exist on the host"
            }
            DemoteResource::StateDir => {
                "cosmon chowns the declared .cosmon state dirs to the uid on the \
                 demote path, so this one resisted it — check for a read-only \
                 mount, an ACL, or a parent directory the uid cannot search"
            }
            // Deliberately does NOT advise chowning the config home wholesale:
            // it can be an operator-supplied directory holding the operator's
            // own credential, and cosmon only ever takes ownership of the two
            // files it wrote there itself.
            DemoteResource::ConsentFile => {
                "cosmon chowns the consent files it wrote (.claude.json, \
                 settings.json) to the uid on the demote path, so this one \
                 resisted it — check for a read-only mount, an ACL, or a mode \
                 that denies the owner read (a worker that cannot read them \
                 replaces them and re-opens the onboarding wizard)"
            }
            // Same shape as the worktree remedy: cosmon transfers this itself
            // on the demote path, so a refusal here means the transfer did not
            // take. Advising "chown it yourself" would be the catch-22 all over
            // again — the linked worktree's gitdir is created by the very
            // `cs tackle` that is demoting.
            DemoteResource::GitPlumbing => {
                "cosmon chowns the worktree's gitdir and the repository's git \
                 common dir to the uid on the demote path, so this one resisted \
                 it — check for a read-only mount, an ACL, a bare-repo layout \
                 outside the checkout, or a parent directory the uid cannot \
                 search (a worker that cannot write these can edit files and \
                 never commit them)"
            }
            // The one resource cosmon judges and deliberately does NOT repair:
            // the binary belongs to whoever installed it, and chowning an
            // interpreter on the host is not a benign default. So this remedy,
            // alone among them, is genuinely the operator's to apply — and it
            // is followable, unlike the catch-22 the worktree advice used to be.
            DemoteResource::WorkerBinary => {
                "make the adapter binary reachable by the uid — `chmod o+x` \
                 every directory on the way to it and `chmod o+x` the binary \
                 itself (an installer that put it under a 0700 home is the \
                 usual cause), or install it somewhere the uid can already \
                 traverse"
            }
        }
    }
}

/// One resource the demoted worker needs, and whether the target uid can use
/// it.
///
/// The *verdict* is computed by the caller — resolving it requires `stat(2)`,
/// which is I/O and therefore belongs behind a port, not in this module. This
/// struct is the port's output: the pure policy in
/// [`enforce_demote_provisioning`] decides what to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoteResourceAccess {
    /// What the path is for.
    pub resource: DemoteResource,
    /// The path checked.
    pub path: String,
    /// Whether the demote target can use it (read+write as appropriate).
    pub usable: bool,
}

impl RootRefusalReason {
    /// A stable machine token for this reason, stamped on the typed
    /// root-refusal event so the container repro (and any audit) can assert
    /// on it. Always contains the substring `root` — the repro harness keys
    /// on that.
    #[must_use]
    pub fn as_token(&self) -> &'static str {
        match self {
            RootRefusalReason::NoNonRootTarget => "root-spawn-refused:no-non-root-target",
            RootRefusalReason::UnprovisionedTarget { .. } => {
                "root-spawn-refused:unprovisioned-demote-target"
            }
            RootRefusalReason::DemoteSharesRepositoryStorage { .. } => {
                "root-spawn-refused:demote-shares-repository-storage"
            }
        }
    }
}

/// The guide a root dispatcher is pointed at: the nominal, non-root pilot.
///
/// A refusal that names no way forward is an outage. This constant is the way
/// forward, and it is quoted in the refusal text rather than left for the
/// operator to find — §8z: a caveat the operator cannot read is not a control,
/// and neither is a remedy.
pub const NON_ROOT_PILOT_GUIDE: &str = "docs/guides/cosmon-mission-in-a-container.md";

impl std::fmt::Display for RootRefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RootRefusalReason::NoNonRootTarget => f.write_str(
                "refusing to spawn a cognitive worker as root and no non-root \
                 demote target is configured (set COSMON_WORKER_UID to a \
                 non-zero uid to enable privilege-drop demotion)",
            ),
            RootRefusalReason::UnprovisionedTarget {
                uid,
                resource,
                path,
            } => write!(
                f,
                "cannot provision uid {uid}: {} `{path}` is not usable by it \
                 (a worker demoted there would start and then wedge on EACCES) \
                 — {}",
                resource.label(),
                resource.remedy(),
            ),
            RootRefusalReason::DemoteSharesRepositoryStorage { uid } => write!(
                f,
                "refusing to demote a worker from root to uid {uid}: committing \
                 from a linked worktree needs write access to the repository's \
                 shared object store and shared refs/heads, which would let \
                 this worker rewrite another molecule's branch or delete an \
                 object another molecule's history depends on \
                 (reproduced at uid 10001 in a container and at uid 501 on \
                 macOS) — run cs as uid {uid} itself instead of as root \
                 (`docker exec -u {uid}:{uid} -e HOME=<that uid's home> … cs \
                 tackle …`), which needs no hand-over at all; see \
                 {NON_ROOT_PILOT_GUIDE}",
            ),
        }
    }
}

/// The decision the root-spawn policy reaches for one dispatch.
///
/// The three variants are the three conceivable outcomes, with the forbidden
/// one (`spawn a live worker as root`) made unrepresentable: there is no
/// `SpawnAsRoot` variant. When the dispatcher is root, [`decide_root_spawn`]
/// can only return [`Self::Demote`] or [`Self::Refuse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSpawnDecision {
    /// The dispatcher is **not** root; spawn the worker as-is with no
    /// privilege change. This is the entire non-root fleet path — byte
    /// identical to the pre-#20 behaviour.
    SpawnAsIs,
    /// The dispatcher **is** root; drop privileges to `to_uid` (a non-root
    /// uid) before exec so the live worker never holds root.
    ///
    /// **Dormant.** [`decide_root_spawn`] never returns this variant: the
    /// hand-over demotion requires grants repository-wide destructive git
    /// authority, so the path is refused rather than narrowed (see that
    /// function). The variant survives because it is the shape the bounded
    /// per-worker ref/object lifecycle will restore, and the machinery behind
    /// it — [`demotion_command_prefix`], the transport provisioning port —
    /// stays tested against it. Nothing on a live dispatch constructs it.
    Demote {
        /// The non-root uid the worker is demoted to. Guaranteed `!= 0`.
        to_uid: u32,
    },
    /// The dispatcher **is** root and demotion is impossible; refuse before
    /// any live worker exists, recording `reason` as a typed root-refusal.
    Refuse {
        /// Why the dispatch refused.
        reason: RootRefusalReason,
    },
}

/// Decide how a dispatch running at `running_uid` must spawn its worker.
///
/// - `running_uid` — the effective uid of the dispatcher. Production callers
///   pass `nix::unistd::Uid::effective().as_raw()`; a test passes `0` to
///   exercise the root path with no privilege.
/// - `demote_target` — the non-root uid to demote to, or `None` when the
///   operator disabled demotion. A `Some(0)` is treated as "no valid
///   target" (demoting to root is not demotion) and folds into a refusal.
///
/// # The invariant
///
/// For `running_uid == 0` the result is **always**
/// [`RootSpawnDecision::Refuse`]. That is stronger than the original
/// contract-20A guarantee (never [`RootSpawnDecision::SpawnAsIs`], i.e. never
/// a live root worker), and it subsumes it.
///
/// # Why root → uid is refused rather than narrowed a fourth time
///
/// The demote path was never only a privilege drop. A worker demoted to
/// another uid cannot write anything the root dispatcher created, so the path
/// carries a hand-over: cosmon `chown`s the worktree, the state dirs, the
/// consent files and the git plumbing to the target uid. That hand-over was
/// found incomplete three times and too generous once, in two days — and the
/// last residue does not yield to a fifth cut.
///
/// A linked worktree commits through the repository's **shared** object store
/// and its **shared** `refs/heads` directory. Git's files backend offers no
/// per-ref and no per-object delegation: writing a loose object means creating
/// the `objects/XX/` fan-out, which needs write on `objects` itself, and a
/// branch named `feat/task-…` lives in a directory that also holds every
/// sibling molecule's branch. So the grant that is small enough to be safe is
/// not large enough to commit, and the grant that is large enough to commit
/// lets a worker rewrite a sibling branch or delete an object another
/// molecule's history depends on. Both were **reproduced**, twice: at uid
/// 10001 in a Linux container, and again at uid 501 on macOS through a
/// mode-bit freeze (`demote_git_plumbing_scope.rs`, the test named
/// `the_grant_still_permits_a_sibling_ref_rewrite_and_a_shared_object_delete`,
/// which characterises the authority the hand-over would confer and is what
/// this refusal exists to avoid conferring).
///
/// The bounded design that would make demotion safe — per-worker ref and
/// object storage reaching the shared store read-only through
/// `objects/info/alternates`, with `cs done` *fetching* rather than merging in
/// place — is a different worktree lifecycle, not a tighter `chown`. It is
/// named and deferred, not half-built.
///
/// Meanwhile the nominal path costs nothing to prefer: run `cs` as the same
/// non-root uid the workers run as. Nothing is created by one identity and
/// handed to another, so there is nothing to hand over and nothing to get
/// wrong. An external tester has replicated that pilot end to end — two
/// consecutive dispatches to terminal state and a `cs done` that merged and
/// cleaned up — with no `safe.directory` exemption. Refusing the dangerous
/// path therefore blocks no one, which is what makes fail-closed the cheap
/// choice here rather than the expensive one.
///
/// # What survives, and why the `Demote` variant is still in the type
///
/// [`RootSpawnDecision::Demote`] is no longer reachable from this function.
/// It is retained — together with [`demotion_command_prefix`] and the
/// transport-side provisioning port — because it is the shape the per-worker
/// storage lifecycle above will re-enable, and because the tests that
/// characterise the grant are the evidence for this refusal. Nothing
/// constructs it on a live dispatch: see the transport funnel
/// `provision_and_decide_root_spawn`, whose repair step is now unreachable by
/// construction, and the test that asserts a refused root dispatch touches no
/// file at all.
#[must_use]
pub fn decide_root_spawn(running_uid: u32, demote_target: Option<u32>) -> RootSpawnDecision {
    if running_uid != 0 {
        // Non-root dispatcher: nothing to demote, no root blast radius.
        return RootSpawnDecision::SpawnAsIs;
    }
    match demote_target {
        // A valid non-root target. Demotion would *work* — and it is refused,
        // because making it work means handing this uid the repository's
        // shared object store and shared refs. See the section above.
        Some(uid) if uid != 0 => RootSpawnDecision::Refuse {
            reason: RootRefusalReason::DemoteSharesRepositoryStorage { uid },
        },
        // No target, or a target that is itself root: demotion is impossible,
        // so refuse before a live worker exists rather than spawn as root.
        _ => RootSpawnDecision::Refuse {
            reason: RootRefusalReason::NoNonRootTarget,
        },
    }
}

/// The environment switch that makes a non-root dispatcher take the root
/// dispatcher's decision. See [`effective_dispatch_uid`].
pub const SIMULATE_ROOT_DISPATCH_ENV: &str = "COSMON_SIMULATE_ROOT_DISPATCH";

/// The uid [`decide_root_spawn`] must be asked about, given the process's real
/// effective uid and the environment.
///
/// This exists for one reason: the property that matters about the root-spawn
/// refusal is **when it happens**, not what it says, and "before any write"
/// cannot be measured by a test that is not root. Root is the only identity
/// that can create the initial condition, so every existing test of this area
/// is `#[ignore]`d behind a root check and runs on nobody's machine — which is
/// how a refusal that fires seven thousand lines into `cs tackle` was pinned by
/// a green suite for a release.
///
/// So the *decision's input* is made injectable, and the injection is
/// **monotone**: [`SIMULATE_ROOT_DISPATCH_ENV`] can only substitute uid `0`,
/// and [`decide_root_spawn`] refuses uid `0` unconditionally. Setting it can
/// therefore only turn a permitted dispatch into a refused one. There is no
/// value of this variable — set by an operator, an attacker, or a stray export
/// in a worker's env — that permits a spawn the real uid would forbid, which is
/// the only property that makes a test seam in a privilege check acceptable.
///
/// Any value other than `"0"`, `"false"`, `"no"`, `"off"` or the empty string
/// enables the substitution; a real root dispatcher is unaffected either way.
///
/// `env_lookup` is injected so this is pure and unit-testable. Production
/// callers pass `|k| std::env::var(k).ok()`.
#[must_use]
pub fn effective_dispatch_uid<F>(running_uid: u32, env_lookup: F) -> u32
where
    F: Fn(&str) -> Option<String>,
{
    match env_lookup(SIMULATE_ROOT_DISPATCH_ENV) {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" => running_uid,
            _ => 0,
        },
        None => running_uid,
    }
}

/// Resolve the non-root demote target from an injected env lookup.
///
/// The operator override is `COSMON_WORKER_UID`:
/// - unset → [`CONVENTIONAL_WORKER_UID`] (the default demote target);
/// - a parseable non-zero uid → that uid;
/// - `"0"`, `"none"`, `"off"`, `"refuse"`, or an unparseable value → `None`,
///   which routes [`decide_root_spawn`] to a typed refusal.
///
/// `env_lookup` is injected so the resolver is pure and unit-testable without
/// touching the process environment. Production callers pass
/// `|k| std::env::var(k).ok()`.
#[must_use]
pub fn resolve_demote_target<F>(env_lookup: F) -> Option<u32>
where
    F: Fn(&str) -> Option<String>,
{
    match env_lookup("COSMON_WORKER_UID") {
        None => Some(CONVENTIONAL_WORKER_UID),
        Some(raw) => {
            let trimmed = raw.trim();
            match trimmed.to_ascii_lowercase().as_str() {
                "none" | "off" | "refuse" | "" => None,
                // A parseable non-zero uid enables demotion; uid 0 (root is
                // not a demotion) and unparseable values disable it.
                _ => match trimmed.parse::<u32>() {
                    Ok(uid) if uid != 0 => Some(uid),
                    _ => None,
                },
            }
        }
    }
}

/// Downgrade a [`Demote`](RootSpawnDecision::Demote) to a typed refusal when
/// the target uid cannot reach something the worker provably needs.
///
/// # The fault this closes (COSMON-DEV #20 defect A3)
///
/// [`demotion_command_prefix`] deliberately omits `--reset-env` so the env
/// prefix survives the `setpriv` exec. The cost is that the demoted worker also
/// keeps **root's `HOME`**: under `docker run -u 0` that is `/root`, mode 0700
/// and root-owned, so `claude` looks for `/root/.claude` and gets `EACCES`.
/// The same asymmetry hits state: `cs tackle` running as root creates the
/// worktree and `.cosmon/state/` entries root-owned, and the demoted worker
/// then fails its own `cs evolve` / `cs complete` writes. `--add-dir` cannot
/// repair either — it is a Claude *authorization* grant, not an OS `chown`
/// (task-20260723-d66d F2, task-20260723-7e12 F3).
///
/// The failure mode is the worst class in a fleet: the worker starts, the
/// readiness probe calls it live, and it wedges partway through on a syscall
/// error nobody is holding. This function converts that into a refusal the
/// operator can read, naming the uid, the path, and the gesture that fixes it.
///
/// **This is detection, not provisioning — and it runs *after* whatever
/// provisioning the caller performed.** Ownership transfer of the worktree and
/// the state dirs now happens on the demote path before these checks are
/// computed (issue #20); this function is what still refuses when that transfer
/// did not take. Cosmon still does not create the demoted identity's config
/// home, so a `ConfigHome` refusal remains a pure operator remedy. Ordering was
/// the bug the caller fixed; the guard below is deliberately unchanged.
///
/// Non-demote decisions pass through untouched, and an empty `checks` slice is
/// a no-op — a caller that cannot probe is not thereby refused.
#[must_use]
pub fn enforce_demote_provisioning(
    decision: RootSpawnDecision,
    checks: &[DemoteResourceAccess],
) -> RootSpawnDecision {
    let RootSpawnDecision::Demote { to_uid } = decision else {
        return decision;
    };
    match checks.iter().find(|c| !c.usable) {
        Some(blocked) => RootSpawnDecision::Refuse {
            reason: RootRefusalReason::UnprovisionedTarget {
                uid: to_uid,
                resource: blocked.resource,
                path: blocked.path.clone(),
            },
        },
        None => RootSpawnDecision::Demote { to_uid },
    }
}

/// Which identity a cognitive pre-flight must run **as**.
///
/// Handed to the pre-flight closure by [`gate_cognitive_preflight`] so the
/// probe cannot silently inherit the dispatcher's identity: the closure is told
/// who it is, and the type makes forgetting impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightIdentity {
    /// The dispatcher's own identity — the non-root fleet path, unchanged.
    AsIs,
    /// The demote target. The dispatcher is root here, so the probe **must**
    /// drop privileges to `to_uid` before exec (the same `setpriv` prefix
    /// [`demotion_command_prefix`] builds for the worker). Two things follow:
    /// no live root cognition (defect A2), and a verdict measured against the
    /// identity the worker will actually authenticate as.
    Demoted {
        /// The uid the probe — and later the worker — runs as.
        to_uid: u32,
    },
}

/// Gate a **cognitive pre-flight** on the root-spawn decision, so no live
/// cognitive process is ever created before the decision is known — and so the
/// one that *is* created runs as the right identity.
///
/// # The faults this closes (COSMON-DEV #20 defect A2, and its regression ND1)
///
/// [`decide_root_spawn`] answers *may this dispatch create a live cognitive
/// worker*. That answer is worthless if something cognitive has already run.
/// The `cs tackle` claude path did exactly that: it called the model
/// pre-flight probe — `claude --model <m> -p ping`, a real, paid, live Claude
/// invocation via `Command::spawn()` — under the dispatcher's **unchanged euid
/// 0**, and only afterwards computed the decision. On the refuse path a root
/// Claude process had already run to completion before cosmon declined.
///
/// The first fix bought that ordering by **skipping** the probe on the demote
/// path. That closed A2 and opened ND1: a demoted worker whose account cannot
/// reach the preferred model no longer got the probe's fallback, so it received
/// an unverified pin and could re-enter the false-active/idle symptom the model
/// pre-flight exists to prevent. Skipping was never the safe composition —
/// *demoting* was.
///
/// So the gate no longer chooses between "probe" and "no probe". It chooses the
/// **identity** the probe runs as, and hands it to the closure:
///
/// - [`Refuse`](RootSpawnDecision::Refuse) → `Err(reason)`, `preflight`
///   **never invoked**. Nothing cognitive precedes a refusal.
/// - [`Demote`](RootSpawnDecision::Demote) → `Ok(preflight(Demoted { to_uid }))`.
///   The probe runs, but as the demote target — never as root — so model
///   resolution survives and the verdict reflects the worker's real auth path.
/// - [`SpawnAsIs`](RootSpawnDecision::SpawnAsIs) → `Ok(preflight(AsIs))`.
///   The entire non-root fleet path, unchanged.
///
/// Ordering stays structural rather than a property of two adjacent statements:
/// the pre-flight is a closure this function owns, and the refuse arm is the one
/// arm that never calls it.
///
/// # Errors
///
/// Returns the [`RootRefusalReason`] when the decision is a refusal. The
/// caller records the typed refusal and aborts; it must not spawn.
pub fn gate_cognitive_preflight<T, F>(
    decision: &RootSpawnDecision,
    preflight: F,
) -> Result<T, RootRefusalReason>
where
    F: FnOnce(PreflightIdentity) -> T,
{
    match decision {
        RootSpawnDecision::Refuse { reason } => Err(reason.clone()),
        RootSpawnDecision::Demote { to_uid } => {
            Ok(preflight(PreflightIdentity::Demoted { to_uid: *to_uid }))
        }
        RootSpawnDecision::SpawnAsIs => Ok(preflight(PreflightIdentity::AsIs)),
    }
}

/// The shell fragment that drops privileges to `to_uid` before `exec`.
///
/// Prepended immediately in front of the worker binary so the demoted
/// worker never holds root. Uses `setpriv` (util-linux, present in the
/// Debian bookworm clean-room image): it replaces the real+effective uid
/// **and** gid and clears supplementary groups, then `exec`s the trailing
/// command with the environment preserved (no `--reset-env`), so the env
/// prefix assembled ahead of it still reaches the worker.
///
/// The gid is set to the same numeric value as the uid, matching the
/// `worker:worker` (10001:10001) convention of the cosmon-dev image. An
/// operator who pins a uid whose primary gid differs is responsible for
/// aligning it; this default follows the image the contract targets.
///
/// Returned as a trailing-space-terminated fragment so a caller can splice
/// it directly before the binary token: `format!("{prefix}{claude_bin} …")`.
#[must_use]
pub fn demotion_command_prefix(to_uid: u32) -> String {
    format!("setpriv --reuid {to_uid} --regid {to_uid} --clear-groups -- ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The load-bearing contract-20A invariant: a root dispatcher NEVER
    /// resolves to spawning a live worker as-is (i.e. as root). This is the
    /// property the pre-#20 `force_sandbox_escape` path violated by forcing
    /// `IS_SANDBOX=1` to keep the worker running at uid 0.
    #[test]
    fn root_never_spawns_as_is() {
        // With the default worker uid available → refused anyway, because
        // demoting to it means handing it the repository's shared storage.
        assert_eq!(
            decide_root_spawn(0, Some(CONVENTIONAL_WORKER_UID)),
            RootSpawnDecision::Refuse {
                reason: RootRefusalReason::DemoteSharesRepositoryStorage {
                    uid: CONVENTIONAL_WORKER_UID,
                },
            },
        );
        // With demotion disabled → refuse before a live worker exists.
        assert_eq!(
            decide_root_spawn(0, None),
            RootSpawnDecision::Refuse {
                reason: RootRefusalReason::NoNonRootTarget,
            },
        );
        // In no case does a root dispatcher spawn as-is.
        for target in [None, Some(0), Some(1), Some(CONVENTIONAL_WORKER_UID)] {
            assert_ne!(
                decide_root_spawn(0, target),
                RootSpawnDecision::SpawnAsIs,
                "root must never spawn a live worker as root (target={target:?})",
            );
        }
    }

    /// A root target is not a demotion — it folds into a refusal, never a
    /// `Demote { to_uid: 0 }`.
    #[test]
    fn root_demote_target_is_refused_not_demoted_to_root() {
        assert_eq!(
            decide_root_spawn(0, Some(0)),
            RootSpawnDecision::Refuse {
                reason: RootRefusalReason::NoNonRootTarget,
            },
        );
    }

    /// The property this refusal exists for: **no** root dispatch, for **any**
    /// demote target, resolves to a demotion.
    ///
    /// Stated over the whole target space rather than over the conventional
    /// uid, because the hazard is a property of the hand-over and not of a
    /// particular number. Red before the refusal landed: every non-zero target
    /// returned `Demote`.
    #[test]
    fn no_root_dispatch_resolves_to_a_demotion() {
        for target in [
            None,
            Some(0),
            Some(1),
            Some(501),
            Some(CONVENTIONAL_WORKER_UID),
            Some(u32::MAX),
        ] {
            let decision = decide_root_spawn(0, target);
            assert!(
                matches!(decision, RootSpawnDecision::Refuse { .. }),
                "root with demote target {target:?} must refuse, got {decision:?}",
            );
        }
    }

    /// The refusal is *typed* — an audit can key on the token — and it is
    /// *reachable*: the operator-facing text names the uid to run as and the
    /// guide that shows how. A refusal an operator cannot act on is an outage
    /// wearing a security hat.
    #[test]
    fn the_shared_storage_refusal_names_the_nominal_invocation_and_the_guide() {
        let RootSpawnDecision::Refuse { reason } = decide_root_spawn(0, Some(10001)) else {
            panic!("root with a non-zero demote target must refuse");
        };
        assert_eq!(
            reason,
            RootRefusalReason::DemoteSharesRepositoryStorage { uid: 10001 },
        );
        assert_eq!(
            reason.as_token(),
            "root-spawn-refused:demote-shares-repository-storage",
        );
        // The container repro harness keys on this substring for every reason.
        assert!(reason.as_token().contains("root"));

        let text = reason.to_string();
        for expected in [
            // why
            "shared object store",
            "rewrite another molecule's branch",
            // what to do instead, concretely enough to type
            "run cs as uid 10001 itself",
            "docker exec -u 10001:10001",
            // where to read the rest
            NON_ROOT_PILOT_GUIDE,
        ] {
            assert!(
                text.contains(expected),
                "the refusal text must contain {expected:?}, got: {text}",
            );
        }
    }

    /// A non-root dispatcher is untouched — the whole normal fleet path.
    #[test]
    fn non_root_spawns_as_is() {
        assert_eq!(decide_root_spawn(1000, None), RootSpawnDecision::SpawnAsIs);
        assert_eq!(
            decide_root_spawn(CONVENTIONAL_WORKER_UID, Some(CONVENTIONAL_WORKER_UID)),
            RootSpawnDecision::SpawnAsIs,
        );
    }

    /// The simulation switch is monotone: it can substitute uid 0 and nothing
    /// else, so it can only ever produce a refusal.
    #[test]
    fn simulate_root_dispatch_can_only_tighten_the_decision() {
        let on = |k: &str| (k == SIMULATE_ROOT_DISPATCH_ENV).then(|| "1".to_owned());
        assert_eq!(effective_dispatch_uid(1000, on), 0);
        assert!(matches!(
            decide_root_spawn(effective_dispatch_uid(1000, on), Some(10001)),
            RootSpawnDecision::Refuse { .. },
        ));

        // Every falsy spelling leaves the real uid alone, and so does absence.
        for raw in ["", "0", "false", "no", "off", " OFF "] {
            let env = |k: &str| (k == SIMULATE_ROOT_DISPATCH_ENV).then(|| raw.to_owned());
            assert_eq!(effective_dispatch_uid(1000, env), 1000, "raw = {raw:?}");
        }
        assert_eq!(effective_dispatch_uid(1000, |_| None), 1000);

        // And it cannot rescue a real root dispatcher, whatever it says.
        for raw in ["", "0", "off", "1", "yes"] {
            let env = |k: &str| (k == SIMULATE_ROOT_DISPATCH_ENV).then(|| raw.to_owned());
            assert_eq!(effective_dispatch_uid(0, env), 0, "raw = {raw:?}");
        }
    }

    #[test]
    fn resolve_demote_target_defaults_to_conventional_worker() {
        assert_eq!(
            resolve_demote_target(|_| None),
            Some(CONVENTIONAL_WORKER_UID)
        );
    }

    #[test]
    fn resolve_demote_target_honours_a_numeric_override() {
        assert_eq!(
            resolve_demote_target(|k| (k == "COSMON_WORKER_UID").then(|| "4242".to_owned())),
            Some(4242),
        );
    }

    #[test]
    fn resolve_demote_target_disables_on_zero_or_sentinel() {
        for raw in ["0", "none", "off", "refuse", "", "not-a-number"] {
            assert_eq!(
                resolve_demote_target(|k| (k == "COSMON_WORKER_UID").then(|| raw.to_owned())),
                None,
                "COSMON_WORKER_UID={raw:?} should disable demotion",
            );
        }
    }

    #[test]
    fn refusal_reason_token_contains_root() {
        assert!(RootRefusalReason::NoNonRootTarget
            .as_token()
            .contains("root"));
    }

    /// The demotion fragment drops both uid and gid to the target and clears
    /// supplementary groups — the worker cannot re-acquire root or a
    /// privileged group. It must NOT preserve any root bypass.
    #[test]
    fn demotion_prefix_drops_to_the_target_uid() {
        let prefix = demotion_command_prefix(CONVENTIONAL_WORKER_UID);
        assert!(prefix.contains("--reuid 10001"), "must set reuid: {prefix}");
        assert!(prefix.contains("--regid 10001"), "must set regid: {prefix}");
        assert!(
            prefix.contains("--clear-groups"),
            "must clear supplementary groups: {prefix}"
        );
        assert!(
            prefix.trim_end().ends_with("--"),
            "must exec-wrap: {prefix}"
        );
        // The whole point of #20: the demotion path never re-arms the root
        // bypass it replaces.
        assert!(
            !prefix.contains("IS_SANDBOX"),
            "demotion must not preserve the root bypass: {prefix}"
        );
    }

    /// COSMON-DEV #20 defect A2 — the ordering contract, made observable.
    ///
    /// Under a root dispatcher with demotion disabled, the decision is a
    /// refusal, and NO cognitive pre-flight may have run by the time the
    /// refusal is reached. The pre-#A2 `cs tackle` path ran a real
    /// `claude --model <m> -p ping` as uid 0 seventeen lines before computing
    /// this decision; the counter below is what that path could not satisfy.
    #[test]
    fn refuse_never_runs_a_cognitive_preflight() {
        let ran = std::cell::Cell::new(0_u32);
        let decision = decide_root_spawn(0, None);
        let outcome = gate_cognitive_preflight(&decision, |_identity| {
            ran.set(ran.get() + 1);
            "some-model".to_owned()
        });
        assert_eq!(
            ran.get(),
            0,
            "a refusal must precede every live cognitive process"
        );
        assert_eq!(outcome, Err(RootRefusalReason::NoNonRootTarget));
    }

    /// COSMON-DEV #20 regression ND1 — the demote path keeps its model
    /// resolution, and buys it by DEMOTING the probe rather than skipping it.
    ///
    /// Two properties in one observation: the pre-flight does run (so a demoted
    /// worker still gets the fallback the probe selects), and the identity it is
    /// handed is the demote target, never root (so A2 is not reopened). The
    /// previous fix satisfied the second by sacrificing the first.
    #[test]
    fn demote_runs_the_preflight_as_the_demoted_identity_never_as_root() {
        let seen = std::cell::RefCell::new(Vec::new());
        let decision = dormant_demote(CONVENTIONAL_WORKER_UID);
        let outcome = gate_cognitive_preflight(&decision, |identity| {
            seen.borrow_mut().push(identity);
            "probe-selected-fallback".to_owned()
        });
        assert_eq!(
            *seen.borrow(),
            vec![PreflightIdentity::Demoted {
                to_uid: CONVENTIONAL_WORKER_UID
            }],
            "the probe must run exactly once, as the demote target — a root \
             probe is defect A2, and no probe at all is regression ND1",
        );
        assert_eq!(
            outcome,
            Ok("probe-selected-fallback".to_owned()),
            "the demoted worker must receive the resolved model, not an \
             unverified pin",
        );
        // Stated as the property, not just the value: no arm may hand the
        // pre-flight root's identity when the dispatcher is root.
        assert!(
            !seen.borrow().contains(&PreflightIdentity::AsIs),
            "a root dispatcher must never run cognition as itself",
        );
    }

    /// The entire non-root fleet path is unchanged: the pre-flight runs
    /// exactly once, as the dispatcher, and its value is handed back.
    #[test]
    fn non_root_runs_the_cognitive_preflight_exactly_once() {
        let ran = std::cell::Cell::new(0_u32);
        let seen = std::cell::Cell::new(None);
        let decision = decide_root_spawn(1000, Some(CONVENTIONAL_WORKER_UID));
        let outcome = gate_cognitive_preflight(&decision, |identity| {
            ran.set(ran.get() + 1);
            seen.set(Some(identity));
            "some-model".to_owned()
        });
        assert_eq!(ran.get(), 1);
        assert_eq!(seen.get(), Some(PreflightIdentity::AsIs));
        assert_eq!(outcome, Ok("some-model".to_owned()));
    }

    // ── COSMON-DEV #20 defect A3: provisioning of the demoted identity ──

    /// A `Demote` decision, stated explicitly because [`decide_root_spawn`] no
    /// longer produces one.
    ///
    /// The tests below are about [`enforce_demote_provisioning`] and
    /// [`gate_cognitive_preflight`], whose contracts are *given a decision* and
    /// are unchanged. Building their input from `decide_root_spawn` would now
    /// feed them a refusal, and each assertion would pass for the wrong reason
    /// — a refusal refuses, a refusal runs no pre-flight. That is a false green
    /// of exactly the kind this lineage keeps producing, so the input is
    /// constructed here instead of derived.
    fn dormant_demote(to_uid: u32) -> RootSpawnDecision {
        RootSpawnDecision::Demote { to_uid }
    }

    fn access(resource: DemoteResource, path: &str, usable: bool) -> DemoteResourceAccess {
        DemoteResourceAccess {
            resource,
            path: path.to_owned(),
            usable,
        }
    }

    /// The load-bearing A3 property: a demote whose target cannot reach its
    /// credentials becomes a REFUSAL, never a live worker that wedges later.
    /// Before the fix nothing checked this at all — the worker started as
    /// uid 10001, looked for root's 0700 `/root/.claude`, and got EACCES with
    /// the readiness probe already calling it live.
    #[test]
    fn unreachable_config_home_refuses_instead_of_demoting() {
        let decision = dormant_demote(CONVENTIONAL_WORKER_UID);
        let out = enforce_demote_provisioning(
            decision,
            &[access(DemoteResource::ConfigHome, "/root/.claude", false)],
        );
        match out {
            RootSpawnDecision::Refuse {
                reason:
                    RootRefusalReason::UnprovisionedTarget {
                        uid,
                        resource,
                        ref path,
                    },
            } => {
                assert_eq!(uid, CONVENTIONAL_WORKER_UID);
                assert_eq!(resource, DemoteResource::ConfigHome);
                assert_eq!(path, "/root/.claude");
            }
            other => panic!("must refuse, not start a doomed worker: {other:?}"),
        }
    }

    /// The state dir is the other half of the same failure: `--add-dir` is a
    /// Claude authorization grant, so a root-owned `.cosmon/` still blocks the
    /// demoted worker's own `cs evolve` write.
    #[test]
    fn unwritable_state_dir_refuses_because_add_dir_is_not_chown() {
        let decision = dormant_demote(10001);
        let out = enforce_demote_provisioning(
            decision,
            &[
                access(DemoteResource::ConfigHome, "/home/worker/.claude", true),
                access(DemoteResource::StateDir, "/repo/.cosmon", false),
            ],
        );
        assert!(matches!(out, RootSpawnDecision::Refuse { .. }));
    }

    /// The refusal is TYPED and LOUD: a stable machine token an audit can key
    /// on, and a message naming the uid, the path, and the fix.
    #[test]
    fn provisioning_refusal_is_typed_and_names_the_remedy() {
        let reason = RootRefusalReason::UnprovisionedTarget {
            uid: 10001,
            resource: DemoteResource::Worktree,
            path: "/w/tree".to_owned(),
        };
        assert_eq!(
            reason.as_token(),
            "root-spawn-refused:unprovisioned-demote-target"
        );
        assert!(
            reason.as_token().contains("root"),
            "the repro harness keys on `root` in the token"
        );
        let msg = reason.to_string();
        assert!(msg.contains("10001"), "must name the uid: {msg}");
        assert!(msg.contains("/w/tree"), "must name the path: {msg}");
        assert!(msg.contains("chown"), "must name the remedy: {msg}");
    }

    /// A fully provisioned target still demotes — the check must not become a
    /// blanket refusal of the demote path.
    #[test]
    fn fully_provisioned_target_still_demotes() {
        let decision = dormant_demote(10001);
        let out = enforce_demote_provisioning(
            decision,
            &[
                access(DemoteResource::ConfigHome, "/home/worker/.claude", true),
                access(DemoteResource::Worktree, "/w/tree", true),
                access(DemoteResource::StateDir, "/repo/.cosmon", true),
            ],
        );
        assert_eq!(out, RootSpawnDecision::Demote { to_uid: 10001 });
    }

    /// Non-demote decisions pass through untouched, and a caller that could
    /// probe nothing is not thereby refused.
    #[test]
    fn provisioning_check_is_a_noop_off_the_demote_path() {
        assert_eq!(
            enforce_demote_provisioning(RootSpawnDecision::SpawnAsIs, &[]),
            RootSpawnDecision::SpawnAsIs
        );
        let refused = decide_root_spawn(0, None);
        assert_eq!(
            enforce_demote_provisioning(refused.clone(), &[]),
            refused,
            "an existing refusal keeps its own reason"
        );
        assert_eq!(
            enforce_demote_provisioning(RootSpawnDecision::Demote { to_uid: 10001 }, &[]),
            RootSpawnDecision::Demote { to_uid: 10001 }
        );
    }
}
