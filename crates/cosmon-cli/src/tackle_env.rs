// SPDX-License-Identifier: AGPL-3.0-only

//! Env-propagation helpers used by `cs tackle` when assembling the
//! command string handed to the tmux backend for a `claude` worker.
//!
//! # Multi-account Claude support (`CLAUDE_CONFIG_DIR`)
//!
//! Resolution order for the Claude config directory:
//!
//! 1. **`cb next`** — if the `cb` binary is on PATH and exits 0, its
//!    stdout (trimmed) is treated as an email address. The config dir
//!    becomes `~/.claude-accounts/<email>/`. This integrates the
//!    round-robin account balancer without requiring the operator to
//!    pre-export `CLAUDE_CONFIG_DIR`.
//! 2. **`CLAUDE_CONFIG_DIR` env var** — fallback when `cb` is absent or
//!    returns non-zero. Preserves backward compat with `claude-account`
//!    and `pizzaiolo`.
//! 3. **Neither** — no prefix emitted; Claude uses its default config.
//!
//! Without explicit propagation the variable is **silently dropped** at
//! the tmux boundary: a long-lived cosmon tmux server captures the env
//! it was spawned with, and every subsequent `new-session` inherits the
//! **server's** snapshot — not the client's.
//!
//! [`build_claude_command`] threads the resolved value through the same
//! `VAR=value cmd` mechanism already used for `COSMON_MOL_DIR` and
//! `COSMON_PARENT_MOL_ID`.
//!
//! See `crates/cosmon-transport/src/tmux.rs` for why the variable does
//! not propagate by default through `tmux new-session`, and
//! `docs/architectural-invariants.md` §8m (multi-account adapter env)
//! for the structural rule this helper enforces.

/// MCP servers that drive a **browser attached to the operator's
/// desktop** and therefore can never respond inside a headless fleet
/// worker.
///
/// `playwright-extension` and `claude-in-chrome` both speak to the
/// operator's logged-in Chrome through a browser extension. A `cs tackle`
/// worker runs headless in a detached tmux session with no attached
/// Chrome, so the *first* call into either server blocks waiting for a
/// browser that will never answer — the worker freezes indefinitely
/// (observed as a session stuck for hours on "Frosting…" /
/// "Calling playwright-extension…") and never reaches `cs evolve`. That
/// is a silent deadlock, the worst failure class in a fleet: the worker
/// looks alive to the liveness probe but makes no progress.
///
/// We remove these servers from the worker's toolset at the spawn
/// boundary via `claude --disallowedTools`, so a call **fails fast**
/// (the model is told the tool is unavailable and picks another path)
/// instead of hanging. The headless-safe `playwright-headless` MCP —
/// which spawns its own isolated Chromium — is intentionally *not* in
/// this list: it is the correct tool for a worker that must screenshot a
/// live URL for the visual-QA gate.
///
/// See `docs/guides/visual-qa-gate.md` ("Headless only — never
/// `playwright-extension`") and the fleet-headless bug `task-20260704-f153`.
pub const OPERATOR_BOUND_BROWSER_MCPS: &[&str] =
    &["mcp__playwright-extension", "mcp__claude-in-chrome"];

/// Assemble the `--disallowedTools '<server> …'` fragment that strips
/// operator-bound browser MCP servers (see [`OPERATOR_BOUND_BROWSER_MCPS`])
/// from a headless worker's toolset.
///
/// Returns a fragment ending in a single trailing space so it slots
/// cleanly between `--permission-mode <mode>` and the `2> …` stderr
/// redirect in [`build_claude_command`]. Returns an empty string when
/// the list is empty (defensive — the list is non-empty today), keeping
/// the command byte-identical to the legacy shape in that degenerate
/// case. Claude Code matches a bare `mcp__<server>` token against every
/// tool that server exposes, so one token disables the whole server.
fn disallowed_browser_tools_fragment() -> String {
    // The list is a non-empty compile-time constant today, so clippy's
    // `const_is_empty` (stabilised on a newer stable than this guard landed
    // on) flags the check as always-false. Keep the guard: it is the
    // documented byte-identical-when-empty defence, and it costs nothing.
    #[allow(clippy::const_is_empty)]
    if OPERATOR_BOUND_BROWSER_MCPS.is_empty() {
        return String::new();
    }
    format!(
        "--disallowedTools {} ",
        shell_quote(&OPERATOR_BOUND_BROWSER_MCPS.join(" "))
    )
}

/// Shell-quote a string for safe embedding in a `VAR=value cmd`
/// expression. Mirrors `TmuxBackend::shell_quote` (which is module-
/// private to `cosmon-transport`) so the cosmon-cli side can quote
/// without taking a dependency on that internal helper.
///
/// Safe characters are returned verbatim; anything else is wrapped in
/// single quotes with embedded single quotes escaped as `'\''` (the
/// standard POSIX dance). A path with spaces, `$`, or quotes survives
/// the outer shell round-trip without losing or gaining tokens.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@'))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Resolve `CLAUDE_CONFIG_DIR` by trying `cb next` first, then the env.
///
/// Resolution chain:
/// 1. `cb_runner()` — calls `cb next`; on success returns the email and
///    we derive `~/.claude-accounts/<email>/`.
/// 2. `env_lookup("CLAUDE_CONFIG_DIR")` — direct env override (backward
///    compat with `claude-account` / `pizzaiolo`).
/// 3. `None` — neither source produced a value; no prefix emitted.
///
/// Both `cb_runner` and `env_lookup` are injectable for testing.
pub fn resolve_claude_config_dir<C, F>(cb_runner: C, env_lookup: &F) -> Option<String>
where
    C: FnOnce() -> Option<String>,
    F: Fn(&str) -> Option<String>,
{
    if let Some(email) = cb_runner() {
        let trimmed = email.trim();
        if !trimmed.is_empty() {
            let home = env_lookup("HOME").unwrap_or_else(|| "/tmp".to_owned());
            return Some(format!("{home}/.claude-accounts/{trimmed}/"));
        }
    }
    env_lookup("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty())
}

/// Whether the `cb next` probe should be suppressed entirely.
///
/// True when `COSMON_API_REQUEST=1` — the adapter-driven request path
/// running inside a tenant container where the host-side `cb`
/// (claude-reservoir) binary is absent. Probing there only yields a
/// misleading failed-exec trail; suppressing it is pure noise reduction
/// (the probe already returns `None` on a missing `cb`). `env_lookup`
/// is injected so the predicate is unit-testable without mutating the
/// process environment.
fn cb_probe_suppressed<F>(env_lookup: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    env_lookup("COSMON_API_REQUEST").as_deref() == Some("1")
}

/// Run `cb next` as a subprocess; return `Some(email)` on exit 0.
///
/// Returns `None` when:
/// - `COSMON_API_REQUEST=1` — the adapter-driven request path. `cb`
///   (claude-reservoir) is a host-side tool absent from tenant
///   containers; probing for it there only produces a misleading
///   failed-exec trail in strace and a noisy spawn attempt. The probe
///   already degrades gracefully via `.output().ok()?`, so skipping it
///   is a pure noise-reduction, not a behaviour change.
/// - `cb` is not on PATH (spawn fails)
/// - `cb next` exits non-zero
/// - stdout is empty after trimming
#[must_use]
pub fn run_cb_next() -> Option<String> {
    if cb_probe_suppressed(|k| std::env::var(k).ok()) {
        return None;
    }
    let output = std::process::Command::new("cb")
        .arg("next")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let email = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if email.is_empty() {
        None
    } else {
        Some(email)
    }
}

/// The role a session plays in the spawn hierarchy.
///
/// Propagated as the `CB_SESSION_ROLE` env var across `cs tackle`
/// boundaries. A broker orchestrates other molecules; a worker
/// executes leaf work. The guard layer uses this to prevent brokers
/// from being recursively spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    /// Session orchestrates other molecules (e.g. `cs run`, patrol).
    Broker,
    /// Session executes leaf work (standard `cs tackle` worker).
    Worker,
}

impl SessionRole {
    /// Parse from the `CB_SESSION_ROLE` env var value.
    #[must_use]
    pub fn from_env(s: &str) -> Option<Self> {
        match s {
            "broker" => Some(Self::Broker),
            "worker" => Some(Self::Worker),
            _ => None,
        }
    }

    /// Env-var representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Broker => "broker",
            Self::Worker => "worker",
        }
    }
}

/// Resolve the current spawn depth from `CB_DEPTH` env.
///
/// Returns 0 when the variable is absent or unparseable (root session).
#[must_use]
pub fn resolve_depth<F>(env_lookup: &F) -> u32
where
    F: Fn(&str) -> Option<String>,
{
    env_lookup("CB_DEPTH")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Resolve the current session role from `CB_SESSION_ROLE` env.
#[must_use]
pub fn resolve_session_role<F>(env_lookup: &F) -> Option<SessionRole>
where
    F: Fn(&str) -> Option<String>,
{
    env_lookup("CB_SESSION_ROLE").and_then(|v| SessionRole::from_env(&v))
}

/// Whether a claude worker spawn must force `IS_SANDBOX=1` to survive
/// Claude Code v2.x's root guard (task-20260720-18bb / BUG #6).
///
/// Claude Code refuses a bypass permission mode when `getuid() == 0` unless
/// `IS_SANDBOX=1` (or `CLAUDE_CODE_BUBBLEWRAP`) is set, then `exit(1)`. We
/// force the escape valve **only** in that exact intersection — running as
/// root *and* a bypass mode — so a non-root worker's env is untouched and a
/// non-bypass mode (which the guard never trips) gains nothing spurious.
///
/// Pure: `is_root` is injected so the decision is unit-testable without
/// actually being root. Production callers pass `geteuid() == 0`.
#[must_use]
pub fn force_sandbox_escape(perm_mode: &str, is_root: bool) -> bool {
    // The v2.x guard fires for `bypassPermissions` and the equivalent
    // `--dangerously-skip-permissions`; cosmon only ever passes the former,
    // but match both so a future mode change stays covered.
    is_root
        && matches!(
            perm_mode,
            "bypassPermissions" | "dangerously-skip-permissions"
        )
}

/// Assemble the shell command string passed to
/// `TransportBackend::spawn_worker` for a `claude` adapter worker.
///
/// The env-prefix block is built in this order: optional
/// `CLAUDE_CONFIG_DIR` (resolved via [`resolve_claude_config_dir`]),
/// optional `ANTHROPIC_MODEL` (model pin pass-through, see below),
/// then `CB_SESSION_ROLE=worker`, `CB_DEPTH=<parent+1>`, and the
/// always-present `COSMON_MOL_DIR` and `COSMON_PARENT_MOL_ID`. The
/// final command is
/// `<env> <claude_bin> --permission-mode <perm_mode> --disallowedTools '…' 2> <stderr>`,
/// where the `--disallowedTools` fragment removes the operator-bound
/// browser MCP servers (see [`OPERATOR_BOUND_BROWSER_MCPS`]) that would
/// otherwise deadlock a headless worker.
///
/// # Model pin pass-through (avatar-surface D1)
///
/// When `ANTHROPIC_MODEL` is present (non-empty) in the `cs tackle`
/// process env, it is re-emitted as a command prefix so it survives
/// the tmux boundary (same silent-drop mechanics as
/// `CLAUDE_CONFIG_DIR` above) and reaches the worker `claude`, which
/// reads it as its model setting. The variable is set by the
/// rpp-adapter's subprocess envelope from the instance-config key
/// `claude_model` (`rpp.toml`) — this helper carries the value
/// **opaquely**: no model-id literal lives here, and a host-side
/// operator can equally pin a model by exporting the variable before
/// `cs tackle`. Absent or empty → no prefix; the claude CLI resolves
/// its own default.
///
/// `env_lookup` is a pure function so the helper can be unit-tested
/// without manipulating the process environment (which would race
/// other tests). Production callers pass `|k| std::env::var(k).ok()`.
///
/// `cb_runner` is a closure that attempts to call `cb next` and returns
/// `Some(email)` on success. Production callers pass [`run_cb_next`].
///
/// # Root escape valve (`IS_SANDBOX`)
///
/// When `env_lookup("IS_SANDBOX")` yields a non-empty value it is re-emitted
/// as a command prefix (value-agnostic, like the model pin). The production
/// caller forces `Some("1")` only when the worker will run as root under a
/// bypass permission mode — see [`force_sandbox_escape`] — so Claude Code
/// v2.x does not refuse the spawn under root. Absent → no prefix.
pub fn build_claude_command<C, F>(
    mol_dir_str: &str,
    parent_id_str: &str,
    claude_bin: &str,
    perm_mode: &str,
    cb_runner: C,
    env_lookup: F,
) -> String
where
    C: FnOnce() -> Option<String>,
    F: Fn(&str) -> Option<String>,
{
    let mut prefix = String::new();
    if let Some(value) = resolve_claude_config_dir(cb_runner, &env_lookup) {
        prefix.push_str("CLAUDE_CONFIG_DIR=");
        prefix.push_str(&shell_quote(&value));
        prefix.push(' ');
    }
    // Model pin pass-through (avatar-surface D1) — value-agnostic
    // re-emission across the tmux boundary; see the doc comment.
    if let Some(model) = env_lookup("ANTHROPIC_MODEL").filter(|v| !v.is_empty()) {
        prefix.push_str("ANTHROPIC_MODEL=");
        prefix.push_str(&shell_quote(&model));
        prefix.push(' ');
    }
    // Root-under-bypassPermissions escape valve (task-20260720-18bb / BUG #6).
    // Claude Code v2.x refuses `--permission-mode bypassPermissions` (and
    // `--dangerously-skip-permissions`) when the process euid is 0, printing
    // "cannot be used with root/sudo privileges for security reasons" to
    // stderr and calling `process.exit(1)` — verified against the shipped
    // 2.1.215 binary, whose guard is literally
    //   process.getuid()===0 && process.env.IS_SANDBOX!=="1" && !CLAUDE_CODE_BUBBLEWRAP
    // A worker that exits at spawn never reaches its composer, so the tmux
    // send-keys briefing lands in a dead pane — the "runtime hang" the tester
    // reported. `IS_SANDBOX=1` is Claude Code's own documented bypass for the
    // check; the caller sets it (via `env_lookup`) ONLY when the worker will
    // run as root under a bypass mode, so the common non-root fleet path is
    // byte-identical. Re-emitted value-agnostically across the tmux boundary,
    // exactly like the model pin above (the tmux server freezes its env at
    // startup and drops later shell overrides).
    if let Some(sandbox) = env_lookup("IS_SANDBOX").filter(|v| !v.is_empty()) {
        prefix.push_str("IS_SANDBOX=");
        prefix.push_str(&shell_quote(&sandbox));
        prefix.push(' ');
    }
    // Gödel self-reference guards: propagate role and depth.
    // Spawned workers always inherit role=worker and depth=parent+1.
    let child_depth = resolve_depth(&env_lookup) + 1;
    let _ = std::fmt::Write::write_fmt(
        &mut prefix,
        format_args!("CB_SESSION_ROLE=worker CB_DEPTH={child_depth} "),
    );
    // Capture the grand-child's stderr to `<mol_dir>/worker.stderr` (C2,
    // delib-20260614-98f2). The worker `claude` is a detached grand-child
    // — `cs tackle` spawns it via tmux and returns; nobody is left holding
    // its stderr, so a crash backtrace or the model-unavailable error
    // (`isApiErrorMessage`, observed 2026-06-12) vanishes and the forensic
    // pass has to scrape `~/.claude/projects/*.jsonl` instead. Redirecting
    // ONLY fd 2 leaves stdout (the TUI the readiness probe captures via
    // `capture-pane`) untouched — claude renders its interface to stdout,
    // so the `❯`-prompt liveness check still sees it. `>` truncates per
    // spawn so the file is the post-mortem of *this* worker, not a pileup.
    let worker_stderr = shell_quote(&format!("{mol_dir_str}/worker.stderr"));
    // Strip operator-bound browser MCP servers from the headless worker's
    // toolset so a call fails fast instead of deadlocking (task-20260704-f153).
    let disallowed = disallowed_browser_tools_fragment();
    format!(
        "{prefix}COSMON_MOL_DIR={mol_dir_str} COSMON_PARENT_MOL_ID={parent_id_str} \
         {claude_bin} --permission-mode {perm_mode} {disallowed}2> {worker_stderr}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: a `cb_runner` that always fails (simulates cb absent).
    fn cb_absent() -> Option<String> {
        None
    }

    #[test]
    fn no_claude_config_dir_yields_unchanged_command() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260522-62c3",
            "/usr/local/bin/claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert_eq!(
            cmd,
            "CB_SESSION_ROLE=worker CB_DEPTH=1 \
             COSMON_MOL_DIR=/tmp/state/mol-X \
             COSMON_PARENT_MOL_ID=task-20260522-62c3 \
             /usr/local/bin/claude --permission-mode bypassPermissions \
             --disallowedTools 'mcp__playwright-extension mcp__claude-in-chrome' \
             2> /tmp/state/mol-X/worker.stderr"
        );
        assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn operator_bound_browser_mcps_are_disallowed_for_headless_worker() {
        // task-20260704-f153: a headless worker has no attached Chrome,
        // so the extension-bound browser MCPs can never answer and would
        // deadlock the worker. They must be stripped at the spawn
        // boundary; the headless-safe `playwright-headless` MCP must NOT
        // be — it is the correct tool for a live-URL screenshot.
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260704-f153",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(
            cmd.contains("--disallowedTools 'mcp__playwright-extension mcp__claude-in-chrome'"),
            "extension-bound browser MCPs must be disallowed: {cmd}"
        );
        assert!(
            !cmd.contains("playwright-headless"),
            "headless-safe browser MCP must stay available: {cmd}"
        );
        // The disallowed fragment sits before the stderr redirect.
        assert!(
            cmd.ends_with("2> /tmp/state/mol-X/worker.stderr"),
            "got: {cmd}"
        );
    }

    #[test]
    fn worker_stderr_is_captured_to_mol_dir() {
        // C2: the spawn command must redirect fd 2 to
        // `<mol_dir>/worker.stderr` so the detached grand-child's crash
        // trail survives. fd 1 (the TUI) must stay on the pane.
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260522-62c3",
            "/usr/local/bin/claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(
            cmd.ends_with("2> /tmp/state/mol-X/worker.stderr"),
            "stderr must be redirected to <mol_dir>/worker.stderr, got: {cmd}"
        );
        assert!(
            !cmd.contains("1>") && !cmd.contains("&>"),
            "stdout must NOT be redirected — the readiness probe reads it: {cmd}"
        );
    }

    #[test]
    fn worker_stderr_path_is_shell_quoted_when_mol_dir_has_spaces() {
        let cmd = build_claude_command(
            "/tmp/My State/mol-X",
            "task-20260522-62c3",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(
            cmd.ends_with("2> '/tmp/My State/mol-X/worker.stderr'"),
            "a mol_dir with spaces must be single-quoted in the redirect: {cmd}"
        );
    }

    #[test]
    fn force_sandbox_escape_only_under_root_and_bypass() {
        // BUG #6: the escape valve is forced ONLY in the exact intersection
        // (root AND a bypass mode). Every other combination leaves the
        // worker env untouched.
        assert!(force_sandbox_escape("bypassPermissions", true));
        assert!(force_sandbox_escape("dangerously-skip-permissions", true));
        assert!(!force_sandbox_escape("bypassPermissions", false));
        assert!(!force_sandbox_escape("acceptEdits", true));
        assert!(!force_sandbox_escape("default", true));
        assert!(!force_sandbox_escape("acceptEdits", false));
    }

    #[test]
    fn is_sandbox_is_emitted_when_env_lookup_forces_it() {
        // BUG #6: when the caller forces `IS_SANDBOX=1` (root + bypass), the
        // value is re-emitted across the tmux boundary so Claude Code v2.x
        // does not `exit(1)` under root. It precedes the claude binary.
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260720-18bb",
            "/usr/local/bin/claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "IS_SANDBOX" {
                    Some("1".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(
            cmd.contains("IS_SANDBOX=1 "),
            "IS_SANDBOX must be re-emitted in the env prefix: {cmd}"
        );
        let sandbox_at = cmd.find("IS_SANDBOX=1").expect("present");
        let claude_at = cmd.find("/usr/local/bin/claude").expect("present");
        assert!(
            sandbox_at < claude_at,
            "IS_SANDBOX must precede the claude binary: {cmd}"
        );
    }

    #[test]
    fn is_sandbox_absent_when_env_lookup_yields_nothing() {
        // The common non-root fleet path: no IS_SANDBOX forced, none exported
        // → the prefix must be byte-identical to today (no IS_SANDBOX token).
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260720-18bb",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(
            !cmd.contains("IS_SANDBOX"),
            "IS_SANDBOX must not appear when neither forced nor exported: {cmd}"
        );
    }

    #[test]
    fn empty_is_sandbox_is_treated_as_absent() {
        // A defensively empty value (operator exported `IS_SANDBOX=`) must
        // not emit a bare `IS_SANDBOX=` token — same guard as the model pin.
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260720-18bb",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "IS_SANDBOX" {
                    Some(String::new())
                } else {
                    None
                }
            },
        );
        assert!(
            !cmd.contains("IS_SANDBOX"),
            "an empty IS_SANDBOX must be treated as absent: {cmd}"
        );
    }

    #[test]
    fn empty_claude_config_dir_is_treated_as_absent() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260522-62c3",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "CLAUDE_CONFIG_DIR" {
                    Some(String::new())
                } else {
                    None
                }
            },
        );
        assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn env_fallback_when_cb_absent() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260522-62c3",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "CLAUDE_CONFIG_DIR" {
                    Some("/Users/you/.claude-forfait-2".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(cmd.starts_with("CLAUDE_CONFIG_DIR=/Users/you/.claude-forfait-2 "));
        assert!(cmd.contains("COSMON_MOL_DIR=/tmp/state/mol-X"));
        assert!(cmd.contains("COSMON_PARENT_MOL_ID=task-20260522-62c3"));
    }

    #[test]
    fn cb_next_success_overrides_env() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260522-62c3",
            "claude",
            "bypassPermissions",
            || Some("user-b@example.org".to_owned()),
            |k| match k {
                "HOME" => Some("/Users/you".to_owned()),
                "CLAUDE_CONFIG_DIR" => Some("/should/be/ignored".to_owned()),
                _ => None,
            },
        );
        assert!(
            cmd.starts_with("CLAUDE_CONFIG_DIR=/Users/you/.claude-accounts/user-b@example.org/ ")
        );
        assert!(!cmd.contains("/should/be/ignored"));
    }

    #[test]
    fn cb_next_failure_falls_through_to_env() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            || None, // cb failed
            |k| {
                if k == "CLAUDE_CONFIG_DIR" {
                    Some("/Users/you/.claude-forfait-2".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(cmd.starts_with("CLAUDE_CONFIG_DIR=/Users/you/.claude-forfait-2 "));
    }

    #[test]
    fn cb_next_empty_output_falls_through_to_env() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            || Some("  \n".to_owned()), // whitespace-only
            |k| {
                if k == "CLAUDE_CONFIG_DIR" {
                    Some("/fallback".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(cmd.starts_with("CLAUDE_CONFIG_DIR=/fallback "));
    }

    #[test]
    fn path_with_spaces_is_single_quoted() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "CLAUDE_CONFIG_DIR" {
                    Some("/Users/Foo Bar/.claude".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(cmd.starts_with("CLAUDE_CONFIG_DIR='/Users/Foo Bar/.claude' "));
    }

    #[test]
    fn path_with_embedded_quote_is_escaped() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "CLAUDE_CONFIG_DIR" {
                    Some("/Users/it's/me".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(cmd.starts_with("CLAUDE_CONFIG_DIR='/Users/it'\\''s/me' "));
    }

    #[test]
    fn cb_derived_path_with_special_chars_is_quoted() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            || Some("user+tag@example.com".to_owned()),
            |k| {
                if k == "HOME" {
                    Some("/Users/test".to_owned())
                } else {
                    None
                }
            },
        );
        // '+' triggers quoting
        assert!(
            cmd.contains("CLAUDE_CONFIG_DIR='/Users/test/.claude-accounts/user+tag@example.com/'")
        );
    }

    #[test]
    fn neither_cb_nor_env_yields_no_prefix() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
        assert!(cmd.starts_with("CB_SESSION_ROLE=worker CB_DEPTH=1 COSMON_MOL_DIR="));
    }

    #[test]
    fn resolve_uses_home_from_env_lookup() {
        let resolved = resolve_claude_config_dir(|| Some("test@example.com".to_owned()), &|k| {
            if k == "HOME" {
                Some("/home/custom".to_owned())
            } else {
                None
            }
        });
        assert_eq!(
            resolved.unwrap(),
            "/home/custom/.claude-accounts/test@example.com/"
        );
    }

    // -- Model pin pass-through (avatar-surface D1) --

    #[test]
    fn anthropic_model_in_env_is_threaded_through_tmux_boundary() {
        // The adapter exports ANTHROPIC_MODEL into the `cs tackle`
        // env (from the rpp.toml `claude_model` key); this helper must
        // re-emit it as a command prefix or the tmux server snapshot
        // silently drops it before the worker `claude` starts.
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-20260610-3791",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "ANTHROPIC_MODEL" {
                    Some("pinned-model-id".to_owned())
                } else {
                    None
                }
            },
        );
        assert!(
            cmd.contains("ANTHROPIC_MODEL=pinned-model-id "),
            "got: {cmd}"
        );
    }

    #[test]
    fn absent_anthropic_model_emits_no_prefix() {
        // No pin in the env → no prefix; the claude CLI resolves its
        // own default (documented fallback).
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(!cmd.contains("ANTHROPIC_MODEL"), "got: {cmd}");
    }

    #[test]
    fn empty_anthropic_model_is_treated_as_absent() {
        let cmd = build_claude_command(
            "/tmp/state/mol-X",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| {
                if k == "ANTHROPIC_MODEL" {
                    Some(String::new())
                } else {
                    None
                }
            },
        );
        assert!(!cmd.contains("ANTHROPIC_MODEL"), "got: {cmd}");
    }

    // -- Gödel self-reference guard tests --

    #[test]
    fn depth_defaults_to_zero_when_absent() {
        assert_eq!(resolve_depth(&|_| None), 0);
    }

    #[test]
    fn depth_parses_from_env() {
        assert_eq!(
            resolve_depth(&|k| (k == "CB_DEPTH").then(|| "3".to_owned())),
            3
        );
    }

    #[test]
    fn depth_returns_zero_on_garbage() {
        assert_eq!(
            resolve_depth(&|k| (k == "CB_DEPTH").then(|| "abc".to_owned())),
            0
        );
    }

    #[test]
    fn child_depth_increments_parent() {
        let cmd = build_claude_command(
            "/tmp/mol",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |k| (k == "CB_DEPTH").then(|| "2".to_owned()),
        );
        assert!(cmd.contains("CB_DEPTH=3"), "got: {cmd}");
    }

    #[test]
    fn root_session_produces_depth_one() {
        let cmd = build_claude_command(
            "/tmp/mol",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(cmd.contains("CB_DEPTH=1"), "got: {cmd}");
    }

    #[test]
    fn session_role_always_worker_in_child() {
        let cmd = build_claude_command(
            "/tmp/mol",
            "task-Y",
            "claude",
            "bypassPermissions",
            cb_absent,
            |_| None,
        );
        assert!(cmd.contains("CB_SESSION_ROLE=worker"), "got: {cmd}");
    }

    #[test]
    fn session_role_parse_roundtrip() {
        assert_eq!(SessionRole::from_env("broker"), Some(SessionRole::Broker));
        assert_eq!(SessionRole::from_env("worker"), Some(SessionRole::Worker));
        assert_eq!(SessionRole::from_env("unknown"), None);
        assert_eq!(SessionRole::Broker.as_str(), "broker");
        assert_eq!(SessionRole::Worker.as_str(), "worker");
    }

    // -- cb-probe suppression under COSMON_API_REQUEST (task-20260602-ef26) --

    #[test]
    fn cb_probe_suppressed_when_api_request_set() {
        assert!(cb_probe_suppressed(
            |k| (k == "COSMON_API_REQUEST").then(|| "1".to_owned())
        ));
    }

    #[test]
    fn cb_probe_not_suppressed_by_default() {
        assert!(!cb_probe_suppressed(|_| None));
    }

    #[test]
    fn cb_probe_not_suppressed_for_other_values() {
        // Only the exact "1" sentinel suppresses; "0"/"true"/empty do not.
        assert!(!cb_probe_suppressed(
            |k| (k == "COSMON_API_REQUEST").then(|| "0".to_owned())
        ));
        assert!(!cb_probe_suppressed(
            |k| (k == "COSMON_API_REQUEST").then(|| "true".to_owned())
        ));
        assert!(!cb_probe_suppressed(
            |k| (k == "COSMON_API_REQUEST").then(String::new)
        ));
    }

    #[test]
    fn resolve_session_role_from_env() {
        assert_eq!(
            resolve_session_role(&|k| (k == "CB_SESSION_ROLE").then(|| "broker".to_owned())),
            Some(SessionRole::Broker)
        );
        assert_eq!(resolve_session_role(&|_| None), None);
    }
}
