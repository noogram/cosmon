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
//! It also pre-grants Claude Code's startup consent for the worker's worktree
//! (issue #81 point 4), through the routine `cs tackle` uses, against the
//! config files the **enveloped** worker environment names. That one is
//! fail-closed: a worker stopped on the folder-trust dialog is a hang.
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

use crate::worker_env::WorkerEnvelope;

/// Adapter names whose worker is Claude Code and therefore asks for folder
/// trust at startup.
const CLAUDE_ADAPTERS: &[&str] = &["claude"];

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
    /// The environment the worker will be spawned under — the envelope's
    /// `build_env` over this process's environment, the same compilation
    /// [`crate::worker_env::EnvelopedBackend`] performs at spawn time. The
    /// consent pre-grant resolves `CLAUDE_CONFIG_DIR` / `HOME` from it and
    /// not from the adapter's own environment, or it would write a config
    /// the worker never reads.
    worker_env: Vec<(String, String)>,
}

impl RppWorkerLaunch {
    /// Resolve the policy from the adapter process's own identity and `PATH`,
    /// for workers spawned under `envelope`.
    #[must_use]
    pub fn resolve(envelope: &WorkerEnvelope) -> Self {
        let running_uid = nix::unistd::Uid::effective().as_raw();
        let demote_target = resolve_demote_target(|k| std::env::var(k).ok());
        Self {
            receipt_root: cosmon_transport::briefing_receipt::receipt_root(),
            cs_bin: which_on_path("cs"),
            root_spawn: decide_root_spawn(running_uid, demote_target),
            worker_env: envelope.build_env(std::env::vars()),
        }
    }

    /// Look `key` up in the worker's environment.
    fn worker_var(&self, key: &str) -> Option<String> {
        self.worker_env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
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

    fn pregrant_startup_consent(&self, ctx: &LaunchContext<'_>) -> Result<(), String> {
        if !CLAUDE_ADAPTERS.contains(&ctx.adapter) {
            return Ok(());
        }
        let config_dir = self.worker_var("CLAUDE_CONFIG_DIR");
        let (paths, outcome) = cosmon_transport::claude_trust::pregrant_worker_consent(
            config_dir.as_deref(),
            |k| self.worker_var(k),
            ctx.worktree,
        )
        .map_err(|e| e.to_string())?;
        tracing::debug!(
            target: "cosmon_rpp_adapter::launch",
            molecule = %ctx.molecule,
            config_file = %paths.config_file.display(),
            outcome = ?outcome,
            "claude startup consent pre-granted"
        );
        Ok(())
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

    /// A policy whose worker environment names `home` as `HOME` and nothing
    /// else — the adapter's own environment must play no part.
    fn policy_with_worker_home(home: &Path) -> RppWorkerLaunch {
        RppWorkerLaunch {
            receipt_root: home.join("receipts"),
            cs_bin: None,
            root_spawn: RootSpawnDecision::SpawnAsIs,
            worker_env: vec![("HOME".to_owned(), home.to_string_lossy().into_owned())],
        }
    }

    /// Run the policy's consent pre-grant for `adapter` in `worktree`.
    fn pregrant(policy: &RppWorkerLaunch, adapter: &str, worktree: &Path) -> Result<(), String> {
        let molecule = cosmon_core::id::MoleculeId::new("task-20260925-bd51").expect("id");
        let worker = cosmon_core::id::WorkerId::new("worker-bd51").expect("worker");
        policy.pregrant_startup_consent(&LaunchContext {
            molecule: &molecule,
            adapter,
            worker: &worker,
            worktree,
        })
    }

    /// Issue #81 point 4: an API-dispatched Claude worker finds folder trust
    /// already granted for its worktree, in the `.claude.json` its own
    /// environment points at. Before the fix this path wrote nothing and the
    /// first worker of a fresh deployment stopped on the trust dialog.
    #[test]
    fn a_claude_dispatch_pregrants_trust_in_the_worker_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let worktree = dir.path().join("galaxy").join(".worktrees").join("mol");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&worktree).expect("worktree");

        pregrant(&policy_with_worker_home(&home), "claude", &worktree).expect("pre-grant");

        let config: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude.json")).expect("config written"),
        )
        .expect("json");
        let key = std::fs::canonicalize(&worktree).expect("canonical");
        assert_eq!(
            config["projects"][key.to_string_lossy().as_ref()]["hasTrustDialogAccepted"],
            serde_json::Value::Bool(true),
            "the worktree must be trusted in the worker's config: {config}"
        );
    }

    /// Only a Claude worker has the dialog; another adapter's dispatch must not
    /// write into a Claude config at all.
    #[test]
    fn a_non_claude_dispatch_writes_no_claude_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");

        pregrant(&policy_with_worker_home(&home), "codex", dir.path()).expect("no-op");

        assert!(!home.join(".claude.json").exists());
    }

    /// A worker environment naming no config home is a refusal, not a guess:
    /// a pre-grant written elsewhere would report success while the worker
    /// still stops on the dialog.
    #[test]
    fn no_worker_home_refuses_the_dispatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut policy = policy_with_worker_home(dir.path());
        policy.worker_env.clear();

        assert!(pregrant(&policy, "claude", dir.path()).is_err());
    }
}
