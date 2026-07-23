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

/// Gate a **cognitive pre-flight** on the root-spawn decision, so no live
/// cognitive process is ever created before the decision is known.
///
/// # The fault this closes (COSMON-DEV #20 defect A2)
///
/// [`decide_root_spawn`] answers *may this dispatch create a live cognitive
/// worker*. That answer is worthless if something cognitive has already run.
/// The `cs tackle` claude path did exactly that: it called the model
/// pre-flight probe — `claude --model <m> -p ping`, a real, paid, live Claude
/// invocation via `Command::spawn()` — under the dispatcher's **unchanged euid
/// 0**, and only afterwards computed the decision. On the refuse path a root
/// Claude process had already run to completion before cosmon declined.
/// Contract-20A requires the refusal to precede any live cognitive process,
/// not merely the *tmux* worker (task-20260723-d66d F3, task-20260723-7e12 F1).
///
/// Ordering is not a property a reader can check by looking at two adjacent
/// statements six months from now — so it is made structural here: the
/// pre-flight is a closure this function owns, and the only path that calls it
/// is the one where the dispatcher is not root.
///
/// - [`Refuse`](RootSpawnDecision::Refuse) → `Err(reason)`, `preflight`
///   **never invoked**.
/// - [`Demote`](RootSpawnDecision::Demote) → `Ok(None)`, `preflight`
///   **never invoked**. The dispatcher is still root here, so probing would
///   itself be a live root cognitive process — and it would measure the wrong
///   identity anyway: the probe would authenticate as root while the worker
///   runs as `to_uid`. Skipping is both the safe and the honest answer; the
///   caller falls back to passing its preferred model through unprobed,
///   exactly as the `COSMON_MODEL_FALLBACK=0` hatch already does.
/// - [`SpawnAsIs`](RootSpawnDecision::SpawnAsIs) → `Ok(Some(preflight()))`.
///   The entire non-root fleet path, unchanged.
///
/// # Errors
///
/// Returns the [`RootRefusalReason`] when the decision is a refusal. The
/// caller records the typed refusal and aborts; it must not spawn.
pub fn gate_cognitive_preflight<T, F>(
    decision: &RootSpawnDecision,
    preflight: F,
) -> Result<Option<T>, RootRefusalReason>
where
    F: FnOnce() -> T,
{
    match decision {
        RootSpawnDecision::Refuse { reason } => Err(reason.clone()),
        RootSpawnDecision::Demote { .. } => Ok(None),
        RootSpawnDecision::SpawnAsIs => Ok(Some(preflight())),
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
        let outcome = gate_cognitive_preflight(&decision, || {
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

    /// The demote path is still a ROOT dispatcher, so the pre-flight is
    /// skipped there too: probing would be a live root cognitive process, and
    /// it would authenticate as root while the worker runs as the demote
    /// target — the wrong identity measured at root privilege.
    #[test]
    fn demote_never_runs_a_cognitive_preflight_either() {
        let ran = std::cell::Cell::new(0_u32);
        let decision = decide_root_spawn(0, Some(CONVENTIONAL_WORKER_UID));
        let outcome = gate_cognitive_preflight(&decision, || {
            ran.set(ran.get() + 1);
            "some-model".to_owned()
        });
        assert_eq!(ran.get(), 0, "no cognitive process may run as root");
        assert_eq!(
            outcome,
            Ok(None),
            "the caller falls back to an unprobed pin"
        );
    }

    /// The entire non-root fleet path is unchanged: the pre-flight runs
    /// exactly once and its value is handed back.
    #[test]
    fn non_root_runs_the_cognitive_preflight_exactly_once() {
        let ran = std::cell::Cell::new(0_u32);
        let decision = decide_root_spawn(1000, Some(CONVENTIONAL_WORKER_UID));
        let outcome = gate_cognitive_preflight(&decision, || {
            ran.set(ran.get() + 1);
            "some-model".to_owned()
        });
        assert_eq!(ran.get(), 1);
        assert_eq!(outcome, Ok(Some("some-model".to_owned())));
    }
}
