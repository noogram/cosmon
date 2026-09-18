// SPDX-License-Identifier: AGPL-3.0-only

//! The adapter's worker launch policy — the environment-dependent half of a
//! dispatched worker's posture (COSMON-DEV #75).
//!
//! [`cosmon_runtime::LibraryExecutor`] renders everything it can derive:
//! permission mode, the out-of-worktree writable grant, the browser-MCP strip,
//! the harness pins. Two things it cannot derive, because `cosmon-runtime` is
//! on the I/O-free side of the architectural boundary and a multi-tenant
//! server must not let its own process environment silently decide a tenant's
//! dispatch:
//!
//! 1. **The briefing-receipt overlay.** A per-worker `--settings` file
//!    registering the `UserPromptSubmit` hook, so each briefing this worker is
//!    sent can be *signed* by Claude Code rather than inferred from its
//!    composer. Minting it writes files and needs a `cs` binary to name in the
//!    hook command.
//! 2. **The root-spawn decision** (COSMON-DEV #20 / contract-20A), which reads
//!    the dispatching process's effective uid.
//!
//! Both are best-effort in opposite directions, and the asymmetry is
//! deliberate. A receipt that cannot be minted costs a *signal*: the dispatch
//! proceeds exactly as it did before receipts existed, because a worker that
//! cannot acknowledge a briefing is still a worker that received one. A root
//! dispatch that cannot demote costs the *dispatch*: the decision is passed
//! through verbatim, including a [`RootSpawnDecision::Refuse`], which the
//! executor turns into a typed refusal before any live worker can exist.

use std::path::{Path, PathBuf};

use cosmon_core::root_spawn_policy::{decide_root_spawn, resolve_demote_target, RootSpawnDecision};
use cosmon_runtime::{LaunchContext, LaunchPosture, WorkerLaunchPolicy};

/// The launch policy every adapter-side dispatch installs.
///
/// Resolved once per dispatch route rather than per spawn: the `cs` lookup is
/// a `PATH` scan and the uid reads are syscalls, and neither answer changes
/// between the workers of one drain.
#[derive(Debug, Clone)]
pub struct RppWorkerLaunch {
    /// The root under which per-worker receipt stations live.
    receipt_root: PathBuf,
    /// The `cs` binary the receipt hook command invokes, when one resolves on
    /// `PATH`. `None` disables the overlay — see the module docs for why that
    /// is a lost signal and not a failed dispatch.
    cs_bin: Option<PathBuf>,
    /// The contract-20A decision for this dispatcher's identity.
    root_spawn: RootSpawnDecision,
}

impl RppWorkerLaunch {
    /// Resolve the policy from the adapter process's own identity and `PATH`.
    #[must_use]
    pub fn resolve() -> Self {
        let running_uid = nix::unistd::Uid::effective().as_raw();
        let demote_target = resolve_demote_target(|k| std::env::var(k).ok());
        Self {
            receipt_root: cosmon_transport::briefing_receipt::receipt_root(),
            cs_bin: which_on_path("cs"),
            root_spawn: decide_root_spawn(running_uid, demote_target),
        }
    }

    /// Mint this worker's receipt station and settings overlay.
    ///
    /// Returns `None` on every failure — a missing `cs`, an unwritable root,
    /// anything. Nothing here may fail a spawn: the hook is an extra signal on
    /// top of the composer read, never a replacement for it, and a worker that
    /// spawns without the overlay keeps the pre-receipt behaviour exactly.
    fn mint_overlay(&self, ctx: &LaunchContext<'_>) -> Option<PathBuf> {
        use cosmon_transport::briefing_receipt as receipt;

        let cs_bin = self.cs_bin.as_ref()?;
        let station = receipt::ReceiptStation::for_worker(&self.receipt_root, ctx.worker);
        station.ensure().ok()?;
        // A fresh worker inherits no receipts. Sweeping on the mint rather
        // than only on the send path means a re-tackled worker name starts
        // clean even if its predecessor died mid-dispatch.
        station.prune(std::time::Duration::from_secs(0));
        let overlay = station.dir().join("settings.json");
        receipt::write_settings_overlay(&overlay, cs_bin, &station).ok()?;
        Some(overlay)
    }
}

impl WorkerLaunchPolicy for RppWorkerLaunch {
    fn posture(&self, ctx: &LaunchContext<'_>) -> LaunchPosture {
        let receipt_overlay = self.mint_overlay(ctx);
        if receipt_overlay.is_none() {
            tracing::warn!(
                target: "cosmon_rpp_adapter::launch",
                molecule = %ctx.molecule,
                worker = %ctx.worker.name(),
                cs_on_path = self.cs_bin.is_some(),
                "briefing receipt hook not installed; this dispatch cannot be \
                 acknowledged and a missing receipt proves nothing about the prompt"
            );
        }
        LaunchPosture {
            // No override: the fleet default applies, named once in
            // `cosmon_core::worker_argv` so the two dispatch paths cannot pick
            // different defaults.
            permission_mode: None,
            receipt_overlay,
            root_spawn: Some(self.root_spawn.clone()),
        }
    }
}

/// Find `name` on `PATH`, as a shell would.
///
/// Hand-rolled rather than a dependency: the whole question is "is there an
/// executable file of this name on the search path", and the answer is used
/// only to decide whether a best-effort hook can be registered.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Whether `path` is a regular file the current process could exec.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name that is not on `PATH` resolves to nothing rather than to a
    /// plausible-looking path that would land in a hook command.
    #[test]
    fn an_absent_binary_resolves_to_nothing() {
        assert!(which_on_path("cosmon-no-such-binary-75").is_none());
    }

    /// A non-executable file of the right name is not a binary. The hook
    /// command would otherwise name a file that cannot run, and the receipt
    /// would never arrive with nothing to attribute the absence to.
    #[test]
    fn a_non_executable_file_is_not_a_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("cs");
        std::fs::write(&file, "not a binary").expect("write");
        assert!(!is_executable_file(&file));
    }
}
