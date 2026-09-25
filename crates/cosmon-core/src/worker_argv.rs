// SPDX-License-Identifier: AGPL-3.0-only

//! The **one** builder for a claude worker's launch argv (COSMON-DEV #75).
//!
//! # Why this module exists
//!
//! A claude worker's launch posture — its permission mode, the browser MCP
//! servers stripped from its toolset, the directories it may write outside
//! its worktree, its briefing-receipt overlay, its harness pins, and whether
//! the real binary runs behind a privilege drop — is a property of the
//! *dispatch*, not of the door the dispatch came through. Every flag below
//! is a precondition for the worker being able to work at all: an unattended
//! pane has nobody to answer a permission prompt, nobody to unfreeze a
//! deadlocked MCP call, and nobody to grant the write its own `cs evolve`
//! performs outside the worktree.
//!
//! Until this module existed there were two builders. `cs tackle` assembled a
//! shell string in `cosmon_cli::tackle_env::build_claude_command` with all of
//! it; the in-process [`crate::transport::TransportBackend`] path taken by
//! `POST /v1/molecules/:id/tackle` built its [`AgentDefinition`] with
//! `args: Vec::new()` and launched claude bare. The remote worker's first
//! prompt hung in a detached pane nobody was attached to (issue #75, reported
//! against cs 0.6.0; the drift entered with the #54 U5 cut-over and is absent
//! from the v0.6.0 tag).
//!
//! The fix is not "emit the same flags in both places" — that is the same
//! defect with a longer fuse, because the next flag is added on one side
//! only. The fix is that both paths render **these** tokens: this module is
//! I/O-free and knows nothing about shells or tmux, so the string-building
//! path quotes what it returns and the argv path hands it to `execve` as-is.
//!
//! [`AgentDefinition`]: crate::transport::AgentDefinition

use std::path::{Path, PathBuf};

use crate::root_spawn_policy::{demotion_command_prefix, RootSpawnDecision};

/// MCP servers that drive a **browser attached to the operator's desktop**
/// and therefore can never respond inside a headless fleet worker.
///
/// `playwright-extension` and `claude-in-chrome` both speak to the operator's
/// logged-in Chrome through a browser extension. A dispatched worker runs
/// headless in a detached session with no attached Chrome, so the *first*
/// call into either server blocks waiting for a browser that will never
/// answer — the worker freezes indefinitely (observed as a session stuck for
/// hours on "Calling playwright-extension…") and never reaches `cs evolve`.
/// That is a silent deadlock, the worst failure class in a fleet: the worker
/// looks alive to the liveness probe while making no progress.
///
/// The servers are removed from the worker's toolset at the spawn boundary
/// via `--disallowedTools`, so a call **fails fast** (the model is told the
/// tool is unavailable and picks another path) instead of hanging. The
/// headless-safe `playwright-headless` MCP — which spawns its own isolated
/// Chromium — is intentionally *not* listed: it is the correct tool for a
/// worker that must screenshot a live URL for the visual-QA gate.
///
/// See `docs/guides/visual-qa-gate.md` and the fleet-headless bug
/// `task-20260704-f153`.
pub const OPERATOR_BOUND_BROWSER_MCPS: &[&str] =
    &["mcp__playwright-extension", "mcp__claude-in-chrome"];

/// The adapter name whose launch posture this module builds.
///
/// Named rather than spelled `"claude"` at each seam, because the string is
/// also the executable that gets exec'd: a typo in one of the two dispatch
/// paths would silently launch a *different* posture, which is the class of
/// drift this module exists to close.
pub const CLAUDE_ADAPTER: &str = "claude";

/// The permission mode a dispatch gets when its embedder states none.
///
/// All fleet workers run in bypass mode for full autonomy: the molecule kind
/// and formula steps provide the guardrails, and permission mode is not the
/// right place to add friction to a pane nobody is attached to. This is the
/// same value `cs tackle`'s `default_permission_mode` returns — named once so
/// the library path cannot pick a different default by accident.
pub const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";

/// The tool classes granted alongside an out-of-worktree `--add-dir`.
///
/// `--add-dir` declares a directory *addressable*; it does not grant the tool
/// that writes there. A worker whose `cs evolve` runs through Bash needs the
/// Bash class too, or the grant buys nothing and the worker still stops on a
/// prompt.
pub const WRITABLE_GRANT_TOOLS: &[&str] = &["Bash", "Edit", "Write"];

/// One claude worker's launch posture, in the order the flags are emitted.
///
/// Borrowed rather than owned throughout: every caller already holds these
/// values, and the builder's whole job is to decide the *sequence*, not to
/// take custody of the data.
#[derive(Debug, Clone, Copy)]
pub struct ClaudeLaunch<'a> {
    /// The `--permission-mode` value. Use [`DEFAULT_PERMISSION_MODE`] unless
    /// the caller has an operator override.
    pub permission_mode: &'a str,
    /// Directories the worker must be allowed to write **outside** its
    /// worktree cwd — in practice the main repo's `.cosmon/`, which holds the
    /// molecule state the worker writes on `cs evolve` / `cs complete`.
    ///
    /// Empty emits neither `--add-dir` nor the tool grant, leaving the launch
    /// byte-identical to a dispatch with no out-of-worktree state (a bare
    /// checkout with no resolvable `.cosmon/`).
    pub writable_roots: &'a [PathBuf],
    /// The briefing-receipt `--settings` overlay, when the embedder minted
    /// one. `None` leaves the launch byte-identical to the pre-receipt shape,
    /// which is what keeps a receipt from ever being able to fail a spawn.
    pub receipt_overlay: Option<&'a Path>,
    /// Harness settings (ADR-177 / issue #65), pre-rendered as
    /// `--<key> <value>` token pairs by
    /// [`crate::harness_settings::render_harness_args`] and appended
    /// **verbatim**. There is deliberately no allowlist: an unknown flag is
    /// rejected by Claude Code's own parser at launch, loudly. Empty (the
    /// common case) contributes nothing.
    pub harness_args: &'a [String],
    /// The model the dispatch's selection chain resolved, emitted as
    /// `--model <id>` (issue #81 point 2).
    ///
    /// The argv is the one channel both dispatch paths share. The in-process
    /// path has no other: its spawn port carries an argv and a cwd, and the
    /// environment the worker sees is whatever its embedder clamps on. When
    /// the model rode only on `ANTHROPIC_MODEL`, the selection was recorded
    /// and then replaced at the spawn by the embedder's own default. Claude
    /// Code ranks `--model` above `ANTHROPIC_MODEL`, so a model carried here
    /// is the model the worker runs, whatever the environment says.
    ///
    /// `None` (or a blank id) emits nothing: no pin, and the deployment's own
    /// default applies.
    pub model: Option<&'a str>,
}

impl<'a> ClaudeLaunch<'a> {
    /// A launch with nothing but a permission mode — the minimum posture.
    #[must_use]
    pub const fn new(permission_mode: &'a str) -> Self {
        Self {
            permission_mode,
            writable_roots: &[],
            receipt_overlay: None,
            harness_args: &[],
            model: None,
        }
    }

    /// Launch the worker on the model the selection chain resolved.
    #[must_use]
    pub const fn with_model(mut self, model: Option<&'a str>) -> Self {
        self.model = model;
        self
    }

    /// Declare directories writable beyond the worktree cwd.
    #[must_use]
    pub const fn with_writable_roots(mut self, roots: &'a [PathBuf]) -> Self {
        self.writable_roots = roots;
        self
    }

    /// Attach the briefing-receipt `--settings` overlay.
    #[must_use]
    pub const fn with_receipt_overlay(mut self, overlay: Option<&'a Path>) -> Self {
        self.receipt_overlay = overlay;
        self
    }

    /// Append the dispatch's resolved harness settings.
    #[must_use]
    pub const fn with_harness_args(mut self, args: &'a [String]) -> Self {
        self.harness_args = args;
        self
    }

    /// Render the argv tokens that follow the `claude` binary.
    ///
    /// The order is fixed: permission mode, model, writable grants, receipt
    /// overlay, harness pins, then the browser-MCP strip. Nothing here is shell-quoted — a token is one
    /// `argv` entry, and a caller assembling a shell string quotes each one
    /// as it splices it.
    #[must_use]
    pub fn render(&self) -> Vec<String> {
        let mut argv = vec![
            "--permission-mode".to_owned(),
            self.permission_mode.to_owned(),
        ];
        if let Some(model) = self.model.map(str::trim).filter(|m| !m.is_empty()) {
            argv.push("--model".to_owned());
            argv.push(model.to_owned());
        }
        if !self.writable_roots.is_empty() {
            argv.push("--add-dir".to_owned());
            for root in self.writable_roots {
                argv.push(root.to_string_lossy().into_owned());
            }
            argv.push("--allowedTools".to_owned());
            argv.extend(WRITABLE_GRANT_TOOLS.iter().map(|t| (*t).to_owned()));
        }
        if let Some(overlay) = self.receipt_overlay {
            argv.push("--settings".to_owned());
            argv.push(overlay.to_string_lossy().into_owned());
        }
        argv.extend(self.harness_args.iter().cloned());
        // Claude Code matches a bare `mcp__<server>` token against every tool
        // that server exposes, so one space-joined value disables all of them.
        // The list is a non-empty compile-time constant today; the guard is
        // the documented byte-identical-when-empty defence and costs nothing.
        #[allow(clippy::const_is_empty)]
        if !OPERATOR_BOUND_BROWSER_MCPS.is_empty() {
            argv.push("--disallowedTools".to_owned());
            argv.push(OPERATOR_BOUND_BROWSER_MCPS.join(" "));
        }
        argv
    }
}

/// Compose the executable and its leading arguments under a root-spawn
/// decision (COSMON-DEV #20 / contract-20A).
///
/// On [`Demote`](RootSpawnDecision::Demote) the returned command is the
/// privilege-dropping helper and `bin` becomes its first trailing argument,
/// so the real claude is exec'd **by** the drop and can never run as uid 0.
/// On [`SpawnAsIs`](RootSpawnDecision::SpawnAsIs) — the entire non-root fleet
/// path — the command is `bin` itself and `args` is returned untouched.
///
/// [`Refuse`](RootSpawnDecision::Refuse) is composed as `SpawnAsIs`
/// *defensively only*: the caller must have intercepted it before any live
/// worker could exist, and this function is not that gate.
///
/// The demotion is composed **at the binary token**, in the same call that
/// emits the binary, and never spliced into an assembled string. The
/// predecessor spliced it with `replacen` on the assumption that its anchor
/// was unique; it was not, and a hostile env value put the privilege drop
/// inside a quoted string where it was inert while the real claude exec'd as
/// root, silently. Returning the pair makes that collision unreachable by
/// construction: there is no search, so there is nothing to divert.
#[must_use]
pub fn compose_launch(
    decision: &RootSpawnDecision,
    bin: &str,
    args: Vec<String>,
) -> (String, Vec<String>) {
    match decision {
        RootSpawnDecision::Demote { to_uid } => {
            // The shared fragment is the shell rendering of the same tokens
            // (`setpriv --reuid N --regid N --clear-groups --`); splitting it
            // keeps ONE definition of the drop rather than a second literal
            // that could disagree with the string path about a flag.
            let mut tokens: Vec<String> = demotion_command_prefix(*to_uid)
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let mut rest = tokens.split_off(1);
            let command = tokens.remove(0);
            rest.push(bin.to_owned());
            rest.extend(args);
            (command, rest)
        }
        RootSpawnDecision::SpawnAsIs | RootSpawnDecision::Refuse { .. } => (bin.to_owned(), args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The minimum posture is never empty: a bare launch still carries the
    /// permission mode and the browser strip. This is the direct falsifier
    /// for issue #75 — `args: Vec::new()` cannot satisfy it.
    #[test]
    fn minimum_posture_carries_permission_mode_and_browser_strip() {
        let argv = ClaudeLaunch::new(DEFAULT_PERMISSION_MODE).render();
        assert_eq!(
            argv,
            vec![
                "--permission-mode",
                "bypassPermissions",
                "--disallowedTools",
                "mcp__playwright-extension mcp__claude-in-chrome",
            ],
        );
    }

    /// The resolved model rides the argv as `--model <id>`, right after the
    /// permission mode; a blank or absent model emits nothing (issue #81
    /// point 2).
    #[test]
    fn a_resolved_model_is_carried_as_the_model_flag() {
        let argv = ClaudeLaunch::new(DEFAULT_PERMISSION_MODE)
            .with_model(Some("claude-sonnet-5"))
            .render();
        assert_eq!(
            &argv[..4],
            &["--permission-mode", "bypassPermissions", "--model", "claude-sonnet-5"],
        );
        for absent in [None, Some(""), Some("  ")] {
            let argv = ClaudeLaunch::new(DEFAULT_PERMISSION_MODE)
                .with_model(absent)
                .render();
            assert!(
                !argv.iter().any(|t| t == "--model"),
                "{absent:?} must emit no model flag: {argv:?}"
            );
        }
    }

    /// A writable root declares the directory AND grants the tool classes —
    /// `--add-dir` alone buys nothing, because the grant that is missing is
    /// the one the worker's `cs evolve` runs through.
    #[test]
    fn writable_roots_declare_dirs_and_grant_tools() {
        let roots = vec![PathBuf::from("/repo/.cosmon"), PathBuf::from("/space dir")];
        let argv = ClaudeLaunch::new("acceptEdits")
            .with_writable_roots(&roots)
            .render();
        assert_eq!(
            argv,
            vec![
                "--permission-mode",
                "acceptEdits",
                "--add-dir",
                "/repo/.cosmon",
                "/space dir",
                "--allowedTools",
                "Bash",
                "Edit",
                "Write",
                "--disallowedTools",
                "mcp__playwright-extension mcp__claude-in-chrome",
            ],
        );
    }

    /// A path with a space is ONE argv token. This is the property the argv
    /// path gets for free and the string path has to buy with quoting — it is
    /// asserted here so a future "join with spaces" shortcut in this module
    /// fails immediately rather than in a worker that cannot find its dir.
    #[test]
    fn a_path_with_a_space_stays_one_token() {
        let roots = vec![PathBuf::from("/space dir/.cosmon")];
        let argv = ClaudeLaunch::new("plan")
            .with_writable_roots(&roots)
            .render();
        assert!(argv.contains(&"/space dir/.cosmon".to_owned()), "{argv:?}");
    }

    /// Overlay and harness pins land between the grants and the strip, in
    /// that order, and each harness token is passed through verbatim.
    #[test]
    fn overlay_and_harness_are_carried_verbatim() {
        let overlay = PathBuf::from("/run/receipts/w1/settings.json");
        let harness = vec!["--fallback-model".to_owned(), "sonnet".to_owned()];
        let argv = ClaudeLaunch::new("bypassPermissions")
            .with_receipt_overlay(Some(&overlay))
            .with_harness_args(&harness)
            .render();
        assert_eq!(
            argv,
            vec![
                "--permission-mode",
                "bypassPermissions",
                "--settings",
                "/run/receipts/w1/settings.json",
                "--fallback-model",
                "sonnet",
                "--disallowedTools",
                "mcp__playwright-extension mcp__claude-in-chrome",
            ],
        );
    }

    /// A non-root dispatch is composed exactly as it was handed in — the
    /// whole fleet path pays nothing for the demotion machinery.
    #[test]
    fn spawn_as_is_leaves_the_command_untouched() {
        let (cmd, args) = compose_launch(
            &RootSpawnDecision::SpawnAsIs,
            "claude",
            vec!["--permission-mode".to_owned(), "plan".to_owned()],
        );
        assert_eq!(cmd, "claude");
        assert_eq!(args, vec!["--permission-mode", "plan"]);
    }

    /// Under a demote the real binary is EXEC'D BY the drop: `claude` is an
    /// argument of `setpriv`, never the command. A composition that returned
    /// `claude` as the command would run the worker as root with the drop
    /// dangling — the silent third outcome contract-20A forbids.
    #[test]
    fn demote_execs_the_real_binary_behind_the_privilege_drop() {
        let (cmd, args) = compose_launch(
            &RootSpawnDecision::Demote { to_uid: 10001 },
            "/usr/local/bin/claude",
            vec![
                "--permission-mode".to_owned(),
                "bypassPermissions".to_owned(),
            ],
        );
        assert_eq!(cmd, "setpriv");
        assert_eq!(
            args,
            vec![
                "--reuid",
                "10001",
                "--regid",
                "10001",
                "--clear-groups",
                "--",
                "/usr/local/bin/claude",
                "--permission-mode",
                "bypassPermissions",
            ],
        );
    }
}
