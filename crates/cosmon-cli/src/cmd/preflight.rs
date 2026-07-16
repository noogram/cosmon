// SPDX-License-Identifier: AGPL-3.0-only

//! Runtime-prerequisite preflight for `cs tackle`.
//!
//! `cs tackle` spawns a worker into a tmux session running an adapter CLI,
//! inside a git worktree. On a stranger's machine missing any of those three
//! tools the dispatch used to die in an opaque
//! `TransportError::SpawnFailed("tmux new-session failed: …")` (no tmux) or a
//! dead `[exited]` carcass pane (no adapter binary) — a stack trace exactly
//! where a first-time user's hour dies — *the first ten minutes must not
//! fail opaquely*.
//!
//! This module detects the missing dependency **before any side effect**
//! (worktree, tmux session, fleet write) and turns it into one actionable
//! line per tool: what is missing and how to get it. It is a preflight check
//! inside the existing `cs tackle` verb — **no new subcommand**, and it never
//! auto-installs anything (the tool reports, the user installs). The
//! richer `cs doctor` surface stays the
//! home for full environment diagnostics.

use std::path::Path;

use super::tackle::adapter_uses_tmux;
use cosmon_core::config::GatesConfig;
use cosmon_core::egress::AutonomyPosture;
use cosmon_core::spawn_seam::ValidatedAdapterName;

/// A runtime prerequisite that is absent from the operator's `PATH`,
/// paired with the one-line remediation `cs tackle` prints for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingDep {
    /// The binary name that was looked for and not found.
    pub bin: String,
    /// One-line, copy-pasteable install hint.
    pub hint: String,
}

/// Preflight the runtime prerequisites for tackling with `adapter`.
///
/// `git` is checked whenever a worktree will be created (`needs_git`), since
/// `git worktree add` is the first side effect. `tmux` and the adapter CLI are
/// checked only for tmux-backed adapters (`claude` / `aider` / `codex`);
/// in-process Direct-API adapters (`openai` / `anthropic` / `llama-cpp`) drive
/// the agent loop inside the `cs tackle` process and need neither.
///
/// Returns `Ok(())` when every required tool is on `PATH`, or an actionable
/// multi-line error naming each missing dependency and how to install it.
///
/// # Errors
///
/// Returns an error listing every missing prerequisite when at least one of
/// the required tools is not found on `PATH`.
pub(crate) fn check(adapter: &ValidatedAdapterName, needs_git: bool) -> anyhow::Result<()> {
    // Structural fail-loud guard BEFORE any missing-dep probe (both run
    // before the first side effect). A strict-local egress posture on a
    // tmux-backed adapter would silently fail OPEN — refuse the dispatch.
    if let Some(breach) = egress_invariant_breach(adapter.as_str(), adapter_uses_tmux(adapter)) {
        anyhow::bail!("{breach}");
    }
    let missing = collect_missing(
        adapter.as_str(),
        adapter_uses_tmux(adapter),
        needs_git,
        on_path,
    );
    if missing.is_empty() {
        return Ok(());
    }
    Err(anyhow::anyhow!("{}", render(&missing)))
}

/// Refuse a configured Rust gate when its toolchain is absent.
///
/// The check is deliberately a PATH probe rather than a worker-issued shell
/// search.  A missing `cargo` is an operator configuration error, never a
/// reason for an untrusted local model to traverse the host filesystem.
pub(crate) fn check_configured_toolchain(gates: &GatesConfig) -> anyhow::Result<()> {
    let commands = [
        gates.setup_command.as_deref(),
        gates.build_command.as_deref(),
        gates.typecheck_command.as_deref(),
        gates.test_command.as_deref(),
        gates.lint_command.as_deref(),
        gates.format_command.as_deref(),
    ];
    if commands.into_iter().flatten().any(command_mentions_cargo) && !on_path("cargo") {
        anyhow::bail!(
            "cs tackle: cargo/toolchain missing — this project configures a cargo verification gate, \
             but `cargo` is not on PATH. Install Rust via https://rustup.rs/ or select a code-only \
             formula whose configured gates do not require cargo."
        );
    }
    Ok(())
}

fn command_mentions_cargo(command: &str) -> bool {
    let words: Vec<_> = command.split_whitespace().collect();
    matches!(words.as_slice(), ["cargo", ..])
        || matches!(words.as_slice(), ["timeout", _, "cargo", ..])
}

/// Fail-loud guard for the load-bearing `strict-local ⇒ in-process`
/// invariant that the egress jail's safety silently rests on.
///
/// The egress jail (delib-20260530-0877) makes a [`AutonomyPosture::StrictLocal`]
/// worker's `exec_command` shell egress-denied *by construction* — but ONLY
/// because the strict posture is handed to the worker through the
/// `COSMON_EGRESS_POLICY` env var set in the `cs tackle` process, and every
/// strict-local adapter shipping today drives its agent loop **in-process**,
/// so its `exec_command` reads that env directly. A tmux-backed adapter
/// instead inherits the cosmon tmux server's environment, which froze at
/// server startup (CLAUDE.md §multi-account) and does **not** carry a variable
/// exported just before this spawn. The jail's safety therefore rests on the
/// *coincidence* `strict-local ⇒ in-process` — a coincidence enforced nowhere
/// (adversarial finding C1-F2, `task-20260712-bfc3`). A future local adapter
/// made tmux-backed would fail **open** silently: the jail env never reaches
/// the worker, `exec_command` runs un-jailed, and no assertion trips.
///
/// This guard converts the coincidence into an enforced invariant. A strict
/// (egress-denied) posture on a tmux-backed adapter is a hard abort, emitted
/// *before any side effect*, naming the exact structural breach. It is
/// **byte-neutral** for every adapter shipping today — all strict-local
/// adapters are in-process, all tmux adapters are remote opt-ins — so it never
/// trips on the current registry; it only fires if someone introduces the
/// dangerous combination. Returns `None` when the invariant holds, or
/// `Some(message)` describing the breach.
///
/// The posture is resolved name-only ([`AutonomyPosture::for_adapter`]): the
/// strict-vs-remote decision is base-url-independent by design (only the audit
/// *endpoint* of a remote opt-in follows `base_url`), so no config is needed.
fn egress_invariant_breach(adapter: &str, is_tmux: bool) -> Option<String> {
    if AutonomyPosture::for_adapter(adapter).is_strict() && is_tmux {
        Some(format!(
            "cs tackle: adapter '{adapter}' resolves to a StrictLocal egress \
             posture (deny-external) but is tmux-backed. The strict jail is \
             injected through the COSMON_EGRESS_POLICY env var, which the \
             cosmon tmux server froze at startup and will NOT propagate to \
             this worker — the egress jail would silently fail OPEN, running \
             exec_command un-jailed. Refusing to dispatch. A strict-local \
             adapter MUST drive its agent loop in-process (see \
             docs/architectural-invariants.md; task-20260712-cefc)."
        ))
    } else {
        None
    }
}

/// Pure core of [`check`], with the PATH probe injected so tests can drive
/// every (present / absent) combination without touching the real machine.
fn collect_missing(
    adapter: &str,
    needs_tmux: bool,
    needs_git: bool,
    probe: impl Fn(&str) -> bool,
) -> Vec<MissingDep> {
    let mut missing = Vec::new();
    if needs_git && !probe("git") {
        missing.push(git_dep());
    }
    if needs_tmux {
        if !probe("tmux") {
            missing.push(tmux_dep());
        }
        if let Some(bin) = adapter_binary(adapter) {
            if !probe(bin) {
                missing.push(adapter_dep(adapter, bin));
            }
        }
    }
    missing
}

/// The CLI binary a tmux-backed adapter shells out to, if any.
///
/// Returns `None` for adapters whose name does not map to a single
/// PATH-resolved binary (in-process adapters, or future names) — the
/// preflight simply skips the binary probe for those rather than guessing.
fn adapter_binary(adapter: &str) -> Option<&'static str> {
    match adapter {
        "claude" => Some("claude"),
        "aider" => Some("aider"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        _ => None,
    }
}

fn git_dep() -> MissingDep {
    MissingDep {
        bin: "git".to_owned(),
        hint: "git not found — install it: `brew install git` (macOS), \
               `apt install git` (Debian/Ubuntu), or https://git-scm.com/downloads"
            .to_owned(),
    }
}

fn tmux_dep() -> MissingDep {
    MissingDep {
        bin: "tmux".to_owned(),
        hint: "tmux not found — install it: `brew install tmux` (macOS) or \
               `apt install tmux` (Debian/Ubuntu). cosmon runs each worker \
               in a tmux session."
            .to_owned(),
    }
}

fn adapter_dep(adapter: &str, bin: &str) -> MissingDep {
    let how = match adapter {
        "claude" => "install Claude Code: https://docs.claude.com/claude-code",
        "aider" => "install it: `pipx install aider-chat` (https://aider.chat)",
        "codex" => "install the Codex CLI from your provider",
        "opencode" => "install it: `npm install -g opencode-ai` (https://opencode.ai)",
        _ => "install the adapter CLI, or pass a different `--adapter`",
    };
    MissingDep {
        bin: bin.to_owned(),
        hint: format!(
            "{bin} (the `{adapter}` adapter CLI) not found — {how}, \
             or pass `--adapter <other>` / set `[adapters.default]` in \
             `.cosmon/config.toml`"
        ),
    }
}

/// Render the missing-dependency list into the actionable error body —
/// a header, one bullet per tool, and the no-auto-install footer.
fn render(missing: &[MissingDep]) -> String {
    let mut s = String::from(
        "cs tackle: missing runtime prerequisite(s) on this machine.\n\
         The first-run cycle needs these tools on your PATH:\n",
    );
    for d in missing {
        s.push_str("  • ");
        s.push_str(&d.hint);
        s.push('\n');
    }
    s.push_str(
        "Install the tool(s) above and re-run `cs tackle`. cosmon does not \
         auto-install dependencies — it reports, you install. \
         Run `cs doctor` for a fuller environment check.",
    );
    s
}

/// Return `true` iff an executable named `bin` is found on `PATH`.
///
/// A plain `PATH` scan (no subprocess fork): for each `PATH` entry, test
/// whether `<dir>/<bin>` is a regular file, and on Unix that it carries an
/// executable bit. This is the same answer `which <bin>` gives, computed
/// in-process so the hot dispatch path adds no fork.
fn on_path(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(bin)))
}

/// Whether `candidate` is a regular file that is executable (Unix) / present
/// (other platforms).
fn is_executable(candidate: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(candidate) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
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

    /// Build a probe that reports a fixed allowlist as present on PATH.
    fn present(allow: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |bin: &str| allow.contains(&bin)
    }

    #[test]
    fn all_present_yields_no_missing() {
        let missing = collect_missing("claude", true, true, present(&["git", "tmux", "claude"]));
        assert!(missing.is_empty());
    }

    #[test]
    fn egress_invariant_breach_fires_only_on_strict_local_plus_tmux() {
        // `local` is a strict-local adapter. Made tmux-backed (the dangerous
        // hypothetical) → hard breach, message names the frozen-env cause.
        let breach =
            egress_invariant_breach("local", true).expect("strict-local + tmux is a breach");
        assert!(breach.contains("StrictLocal"));
        assert!(breach.contains("COSMON_EGRESS_POLICY"));
        assert!(breach.contains("fail OPEN"));

        // The three quadrants that hold today are all clean.
        assert!(
            egress_invariant_breach("local", false).is_none(),
            "strict-local in-process (the shipping shape) is safe"
        );
        assert!(
            egress_invariant_breach("claude", true).is_none(),
            "remote opt-in on tmux carries no strict jail to lose"
        );
        assert!(
            egress_invariant_breach("claude", false).is_none(),
            "remote opt-in in-process is safe"
        );
    }

    #[test]
    fn missing_tmux_is_reported_with_install_hint() {
        let missing = collect_missing("claude", true, true, present(&["git", "claude"]));
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].bin, "tmux");
        assert!(missing[0].hint.contains("brew install tmux"));
        assert!(missing[0].hint.contains("apt install tmux"));
    }

    #[test]
    fn missing_adapter_binary_names_the_adapter_and_how_to_get_it() {
        let missing = collect_missing("claude", true, true, present(&["git", "tmux"]));
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].bin, "claude");
        assert!(missing[0].hint.contains("Claude Code"));
        assert!(missing[0].hint.contains("--adapter"));
    }

    #[test]
    fn missing_git_is_reported_when_worktree_needed() {
        let missing = collect_missing("claude", true, true, present(&["tmux", "claude"]));
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].bin, "git");
        assert!(missing[0].hint.contains("git-scm.com"));
    }

    #[test]
    fn git_skipped_when_no_worktree() {
        // needs_git=false (e.g. COSMON_ALLOW_NO_WORKTREE escape hatch) — a
        // machine without git is not flagged for the missing worktree step.
        let missing = collect_missing("claude", true, false, present(&["tmux", "claude"]));
        assert!(missing.is_empty());
    }

    #[test]
    fn inprocess_adapter_skips_tmux_and_binary_checks() {
        // openai is in-process (no tmux pane, no PATH binary): only git is
        // ever relevant, and here git is present, so nothing is missing.
        let missing = collect_missing("openai", false, true, present(&["git"]));
        assert!(missing.is_empty());
    }

    #[test]
    fn cargo_gate_detection_is_token_aware() {
        assert!(command_mentions_cargo("cargo test --workspace"));
        assert!(command_mentions_cargo("timeout 600 cargo test"));
        assert!(!command_mentions_cargo("echo cargo"));
        assert!(!command_mentions_cargo("cargo-fmt --check"));
    }

    #[test]
    fn multiple_missing_are_all_reported() {
        let missing = collect_missing("claude", true, true, present(&[]));
        let bins: Vec<&str> = missing.iter().map(|d| d.bin.as_str()).collect();
        assert!(bins.contains(&"git"));
        assert!(bins.contains(&"tmux"));
        assert!(bins.contains(&"claude"));
        assert_eq!(missing.len(), 3);
    }

    #[test]
    fn aider_binary_hint_is_adapter_specific() {
        let missing = collect_missing("aider", true, true, present(&["git", "tmux"]));
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].bin, "aider");
        assert!(missing[0].hint.contains("aider-chat"));
    }

    #[test]
    fn unknown_tmux_adapter_skips_binary_probe() {
        // A tmux-backed adapter we have no binary mapping for: tmux is still
        // checked, but no spurious binary line is emitted.
        let missing = collect_missing("future-pane-adapter", true, true, present(&["git", "tmux"]));
        assert!(missing.is_empty());
    }

    #[test]
    fn render_lists_every_dep_and_the_no_autoinstall_footer() {
        let body = render(&[git_dep(), tmux_dep()]);
        assert!(body.contains("missing runtime prerequisite"));
        assert!(body.contains("git not found"));
        assert!(body.contains("tmux not found"));
        assert!(body.contains("does not"));
        assert!(body.contains("auto-install"));
        assert!(body.contains("cs doctor"));
    }

    #[test]
    fn on_path_finds_a_known_unix_binary() {
        // `sh` is on PATH on every POSIX CI runner. Guard the assertion so a
        // pathological PATH-less environment does not red the suite.
        if std::env::var_os("PATH").is_some() {
            assert!(on_path("sh") || on_path("ls"));
        }
        assert!(!on_path("definitely-not-a-real-binary-xyzzy-42"));
    }
}
