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
//! for `running_uid == 0` the decision is always [`RootSpawnDecision::Demote`]
//! or [`RootSpawnDecision::Refuse`], and [`RootSpawnDecision::SpawnAsIs`] is
//! structurally reachable only for a non-root dispatcher.
//!
//! # What this module does NOT yet do (named follow-up)
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
//! 3. **Ownership transfer of what the worker writes.** `cs tackle` as root
//!    creates the worktree and `.cosmon/state/` root-owned; the demoted worker
//!    must own (or be able to write) both, or its own `cs evolve` /
//!    `cs complete` fail. `--add-dir` cannot help — it is a Claude
//!    authorization grant, not an OS `chown`.
//!
//! Until those land, a root dispatcher on an unprovisioned host refuses with
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
}

impl DemoteResource {
    /// A short human label used in the refusal message.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DemoteResource::ConfigHome => "claude config home",
            DemoteResource::Worktree => "worktree",
            DemoteResource::StateDir => "cosmon state dir",
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
            DemoteResource::Worktree => "chown the worktree to the uid before tackling",
            DemoteResource::StateDir => "chown the .cosmon state dir to the uid",
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
        }
    }
}

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
/// For `running_uid == 0` the result is **never** [`RootSpawnDecision::SpawnAsIs`]:
/// it is [`RootSpawnDecision::Demote`] with a non-zero uid, or
/// [`RootSpawnDecision::Refuse`]. This is the contract-20A guarantee that a
/// live cognitive worker never runs as root.
#[must_use]
pub fn decide_root_spawn(running_uid: u32, demote_target: Option<u32>) -> RootSpawnDecision {
    if running_uid != 0 {
        // Non-root dispatcher: nothing to demote, no root blast radius.
        return RootSpawnDecision::SpawnAsIs;
    }
    match demote_target {
        // A valid non-root target: drop privileges to it before exec.
        Some(uid) if uid != 0 => RootSpawnDecision::Demote { to_uid: uid },
        // No target, or a target that is itself root: demotion is impossible,
        // so refuse before a live worker exists rather than spawn as root.
        _ => RootSpawnDecision::Refuse {
            reason: RootRefusalReason::NoNonRootTarget,
        },
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
/// **This is detection, not provisioning.** Cosmon still does not create the
/// demoted identity's config home or chown its worktree; it now declines
/// loudly instead of starting a worker that cannot finish. Full provisioning
/// (a `--reset-env`-style env rewrite plus ownership transfer on the demote
/// path) is the named follow-up.
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
        // With the default worker uid available → demote to it.
        assert_eq!(
            decide_root_spawn(0, Some(CONVENTIONAL_WORKER_UID)),
            RootSpawnDecision::Demote {
                to_uid: CONVENTIONAL_WORKER_UID
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

    /// A non-root dispatcher is untouched — the whole normal fleet path.
    #[test]
    fn non_root_spawns_as_is() {
        assert_eq!(decide_root_spawn(1000, None), RootSpawnDecision::SpawnAsIs);
        assert_eq!(
            decide_root_spawn(CONVENTIONAL_WORKER_UID, Some(CONVENTIONAL_WORKER_UID)),
            RootSpawnDecision::SpawnAsIs,
        );
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
        let decision = decide_root_spawn(0, Some(CONVENTIONAL_WORKER_UID));
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
        let decision = decide_root_spawn(0, Some(CONVENTIONAL_WORKER_UID));
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
        let decision = decide_root_spawn(0, Some(10001));
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
        let decision = decide_root_spawn(0, Some(10001));
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
