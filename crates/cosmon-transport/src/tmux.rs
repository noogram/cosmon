// SPDX-License-Identifier: AGPL-3.0-only

//! Tmux-based transport backend.
//!
//! Spawns each worker agent in a dedicated tmux session on a configurable
//! socket (`tmux -L <socket>`). Session naming follows `{prefix}{worker-name}`.
//!
//! # Paste-buffer invariant
//!
//! Tmux paste buffers are scoped per server (socket) and named in a
//! server-global namespace. A shared literal like `"cosmon-input"` races
//! across parallel `send_input` calls. The invariant this module now
//! enforces is:
//!
//! > Every paste-buffer is minted by `TmuxBackend::load_buffer` with a
//! > name derived from `WorkerId` + PID + an atomic counter, and is handed
//! > back inside a `TmuxBuffer` RAII handle. The handle carries the
//! > target session name; `TmuxBackend::paste_buffer` pastes into that
//! > session and consumes the handle. Drop deletes any unconsumed buffer.
//!
//! Because `TmuxBuffer` is `pub(crate)` and only `TmuxBackend::load_buffer`
//! constructs one, cross-wiring a `(buffer, session)` pair is not just
//! unlikely — it does not type-check.

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cosmon_core::id::WorkerId;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};

/// Monotonic counter to disambiguate tmux paste-buffer names across concurrent
/// `send_input` calls. Combined with PID inside [`TmuxBackend::load_buffer`]
/// to yield a name unique across processes and threads on the same socket.
static BUF_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Substring Claude Code renders in its input box when a multi-line paste is
/// collapsed into a placeholder (e.g. `[Pasted text #1 +42 lines]`).
///
/// This marker is the load-bearing half of the "pasted-but-not-submitted"
/// detection. When a large prompt is pasted, the TUI
/// hides the literal text behind this placeholder, so the tail-match against
/// the prompt's last line can never see it — and the single Enter retry that
/// match guarded never fired. A worker then sat idle on `❯` with the paste
/// (and every later propulsion nudge) accumulating unsubmitted, occupying a
/// fleet slot while doing zero work. Matching the placeholder directly makes
/// the stall visible regardless of how the TUI rendered the paste.
const CLAUDE_PASTED_PLACEHOLDER: &str = "[Pasted text";

/// Codex's counterpart to [`CLAUDE_PASTED_PLACEHOLDER`]. Codex collapses a
/// multi-line composer paste as `[Pasted Content …]`. Treating only Claude's
/// spelling as pending made the shared submit loop return after its first
/// poll for Codex, leaving both tackle prompts and whispers at the composer.
const CODEX_PASTED_PLACEHOLDER: &str = "[Pasted Content";

/// Baseline number of `Enter`-resend cycles [`TmuxBackend::send_input`] will
/// spend flushing an unsubmitted paste before giving up — the budget for a
/// *small* prompt. Large multi-block pastes scale above this; see
/// [`submit_retry_budget`].
///
/// The first `Enter` is fire-and-forget: a busy TUI drops the keypress
/// silently. Rather than retry exactly once (the pre-fix behaviour, which
/// left intermittent idle-not-submitted workers), we poll the input zone and
/// re-send until it clears or this budget is spent. A later propulsion nudge
/// is the backstop if the budget is exhausted — it too funnels through
/// `send_input` and re-verifies submission.
const SUBMIT_RETRY_BUDGET_BASE: u32 = 5;

/// Hard ceiling on the auto-scaled submit-retry budget. Bounds the worst-case
/// wall-clock a single `send_input` can spend polling
/// (`SUBMIT_RETRY_BUDGET_MAX * SUBMIT_POLL_INTERVAL_MS` ≈ 12s) so a TUI that
/// never clears cannot wedge the caller indefinitely.
const SUBMIT_RETRY_BUDGET_MAX: u32 = 40;

/// Rough number of pasted lines Claude Code packs into one collapsed
/// `[Pasted text #N]` block. Used only to *scale* the retry budget so a
/// multi-block paste earns proportionally more re-`Enter` attempts; the exact
/// value is not load-bearing — under-estimating just costs a few extra polls.
const LINES_PER_PASTE_BLOCK: usize = 12;

/// Pause between submit-verification polls, in milliseconds.
const SUBMIT_POLL_INTERVAL_MS: u64 = 300;

/// The submit keystroke, spelled as a raw hex byte for `tmux send-keys -H`.
///
/// `0d` is ASCII carriage return — the byte a TUI reads as "submit".
///
/// # Why the byte and not the key name (task-20260724-c014)
///
/// `send-keys Enter` and `send-keys C-m` are *not* stable spellings of that
/// byte: tmux re-encodes named keys when the server option `extended-keys` is
/// on and the pane's application has negotiated an extended-key mode (Claude
/// Code v2.x asks for one at startup). Measured against tmux 3.5a with a pane
/// that requests both the kitty and `modifyOtherKeys` protocols:
///
/// | spelling         | `extended-keys off` | `extended-keys on`     |
/// |------------------|---------------------|------------------------|
/// | `Enter`          | `\r`                | `\r`                   |
/// | `C-m`            | `\r`                | `\x1b[27;5;109~`       |
/// | `C-j`            | `\n`                | `\x1b[27;5;106~`       |
/// | **`-H 0d`**      | `\r`                | `\r`                   |
///
/// So the folk remedy "send `C-m`, not `Enter`" is backwards: `C-m` is the
/// spelling that silently degrades into a modified-key escape sequence the
/// composer ignores, on exactly the hosts that enable extended keys. `-H 0d`
/// bypasses tmux's key table entirely and writes the byte, so the submit is
/// byte-identical on every host regardless of operator tmux configuration.
const SUBMIT_KEY_HEX: &str = "0d";

/// Auto-scale the submit-retry budget with the size of the pasted input.
///
/// The fixed-budget pre-fix (`SUBMIT_RETRY_BUDGET = 5`, ≈2s of polling)
/// flushed a single collapsed `[Pasted text]` block fine, but a *large* brief
/// pastes as several stacked blocks
/// (`[Pasted text #1 +13][Pasted text #2 +12]…[Pasted text #5 +24]`) and the
/// TUI stays busy long enough to swallow all five Enters before the budget is
/// spent — the worker then sits idle on `❯` with an unsubmitted paste, burning
/// a fleet slot for zero work (`galaxy-drain-playhouse-7ce2`, 2026-06-25, ~67% of
/// sampled drainage workers). Granting one extra re-`Enter` cycle per
/// estimated block — capped at [`SUBMIT_RETRY_BUDGET_MAX`] — keeps the loop
/// pressing Enter until the multi-block paste actually registers as submitted.
fn submit_retry_budget(input: &str) -> u32 {
    let lines = input.lines().count();
    // Saturating conversion: a count beyond u32 only means "very large", which
    // the `.min(MAX)` below clamps regardless.
    let extra_blocks = u32::try_from(lines / LINES_PER_PASTE_BLOCK).unwrap_or(u32::MAX);
    SUBMIT_RETRY_BUDGET_BASE
        .saturating_add(extra_blocks.saturating_mul(2))
        .min(SUBMIT_RETRY_BUDGET_MAX)
}

/// Pure submit-loop driver: keep "pressing Enter" until the input zone reads
/// clear or `budget` cycles are spent, returning `true` iff the input cleared.
///
/// Extracted from [`TmuxBackend::send_input`] so the multi-block convergence
/// is unit-testable without a live tmux server or Claude TUI: tests inject a
/// `poll_pending` that reports the paste pending for the first few polls (the
/// swallowed Enters) then clear, and assert the loop wins within the
/// auto-scaled budget. In production `poll_pending` sleeps then captures the
/// pane and `press_enter` re-sends the keypress — both best-effort, so a
/// capture failure ends the loop rather than escalating a delivered paste into
/// an error. A final poll after the last Enter lets the budget's last attempt
/// count.
fn drive_submit(
    budget: u32,
    mut poll_pending: impl FnMut() -> bool,
    mut press_enter: impl FnMut(),
) -> bool {
    for _ in 0..budget {
        if !poll_pending() {
            return true;
        }
        press_enter();
    }
    !poll_pending()
}

/// Pure half of [`TmuxBackend::input_still_pending`]: given a captured pane
/// and the input we tried to submit, decide whether the input is still
/// sitting unsubmitted in the TUI's bottom input zone.
///
/// Two independent signals, either of which means "not submitted":
///
/// 1. A collapsed-paste placeholder ([`CLAUDE_PASTED_PLACEHOLDER`] or
///    [`CODEX_PASTED_PLACEHOLDER`]) in the bottom few lines — the large-prompt
///    case, where the literal text is hidden.
///    A *large* brief stacks several such placeholders in the input box
///    (`[Pasted text #1 …][Pasted text #2 …]…`); matching any one of them
///    flags the whole multi-block paste as still pending.
/// 2. The last non-empty line of `input` still visible at the bottom — the
///    small-prompt case, where the TUI renders the text verbatim.
///
/// The composer is normally a closed `╭…╰` box; Codex may instead use a glyph
/// prompt with continuation lines.  Prefer the last *closed* box, never an
/// unmatched later `╭`: scrollback and transcript decorations can contain box
/// tops too.  If its top has scrolled away, retain a narrow bottom-window
/// placeholder floor — a collapsed paste is an unambiguous pending signal.
fn composer_indicates_pending(captured: &str, input: &str) -> bool {
    let lines: Vec<&str> = captured.lines().map(str::trim).collect();
    let Some(needle) = input
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
    else {
        return false;
    };
    let pending = |line: &str| {
        line.contains(CLAUDE_PASTED_PLACEHOLDER)
            || line.contains(CODEX_PASTED_PLACEHOLDER)
            || line.contains(needle)
    };

    // Pair the final closing border with the nearest preceding opening border.
    // An unmatched `╭` below a real composer must not hide that composer.
    if let Some(end) = lines.iter().rposition(|line| line.starts_with('╰')) {
        if let Some(start) = lines[..end].iter().rposition(|line| line.starts_with('╭')) {
            return lines[start..=end].iter().any(|line| pending(line));
        }
    }

    // A glyph composer may carry a short verbatim multi-line prompt. Include
    // all visible continuation lines, not merely the line bearing `›`/`❯`.
    if let Some(start) = lines
        .iter()
        .rposition(|line| matches!(line.chars().next(), Some('›' | '❯' | '>')))
    {
        if lines[start..].iter().any(|line| pending(line)) {
            return true;
        }
    }

    // The top border can scroll out of capture-pane's viewport for a tall
    // composer. A nearby collapsed-paste marker is then stronger evidence than
    // the absence of its border; confine it to the visible tail so transcript
    // history cannot revive an old submission.
    lines
        .iter()
        .rev()
        .filter(|line| !line.is_empty())
        .take(10)
        .any(|line| {
            line.contains(CLAUDE_PASTED_PLACEHOLDER) || line.contains(CODEX_PASTED_PLACEHOLDER)
        })
}

/// A tmux paste-buffer reserved for a single `send_input` call,
/// RAII-bound to its parent [`TmuxBackend`].
///
/// The buffer name is minted by [`TmuxBackend::load_buffer`] and stored
/// alongside the target session inside the handle. The only way to paste
/// is [`TmuxBackend::paste_buffer`], which reads both fields from the
/// handle — so a caller cannot hand in a mismatched `(buffer, session)`
/// pair. This is what makes the cross-wiring race
/// unrepresentable at the type level.
///
/// If the handle is dropped without being consumed (error path before
/// paste), [`Drop`] issues a best-effort `tmux delete-buffer` so an
/// orphan buffer cannot leak into a later paste on the same socket.
pub(crate) struct TmuxBuffer<'s> {
    backend: &'s TmuxBackend,
    session: String,
    name: String,
    consumed: bool,
}

impl Drop for TmuxBuffer<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            let _ = self.backend.tmux_cmd(&["delete-buffer", "-b", &self.name]);
        }
    }
}

/// Transport backend that manages agents via tmux sessions.
#[derive(Debug, Clone)]
pub struct TmuxBackend {
    socket: String,
}

impl TmuxBackend {
    /// Create a new `TmuxBackend` with the given socket name.
    #[must_use]
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Return the tmux socket name.
    #[must_use]
    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// Build session name from config prefix and worker name.
    fn session_name(config: &RuntimeConfig, worker_id: &WorkerId) -> String {
        format!("{}{}", config.session_prefix, worker_id.name())
    }

    /// Shell-quote a string for safe embedding in a shell command.
    fn shell_quote(s: &str) -> String {
        if s.is_empty() {
            return "''".to_owned();
        }
        // If the string is safe (no special chars), return as-is
        if s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
        {
            return s.to_owned();
        }
        // Wrap in single quotes, escaping embedded single quotes
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    /// Spawn an arbitrary command in a fresh tmux session with the
    /// supplied working directory.
    ///
    /// Unlike the generic [`TransportBackend::spawn`], this passes the
    /// command as a single string (not shell-quoted) and supports
    /// `-c workdir`. Despite the historical name (`spawn_claude`,
    /// renamed in ADR-097 / PR-4), this primitive is fully
    /// Adapter-agnostic: both [`crate::claude`] and [`crate::aider`]
    /// build their respective command strings and delegate here.
    ///
    /// # Env-propagation caveat
    ///
    /// The tmux **server** captures its environment once, when the very
    /// first `tmux -L <socket> …` command spawns it. Every subsequent
    /// `new-session` inherits the server's snapshot — *not* the client
    /// shell's current env. So any variable a caller exports
    /// immediately before `cs tackle` (e.g. `CLAUDE_CONFIG_DIR` for
    /// `claude-account` multi-forfait routing) is **silently dropped**
    /// unless it is folded into `cmd` itself as a `VAR=value …` prefix.
    /// Callers that need per-invocation env propagation must build that
    /// prefix; see
    /// [`cosmon_cli::tackle_env::build_claude_command`](../cosmon_cli/tackle_env/fn.build_claude_command.html)
    /// for the canonical pattern.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::SpawnFailed`] if the tmux session cannot be created.
    pub fn spawn_worker(
        &self,
        session_name: &str,
        work_dir: &str,
        cmd: &str,
    ) -> Result<(), TransportError> {
        self.tmux_cmd(&["new-session", "-d", "-s", session_name, "-c", work_dir, cmd])
            .map_err(|e| TransportError::SpawnFailed(format!("tmux new-session failed: {e}")))?;
        Ok(())
    }

    /// Mint a fresh paste-buffer bound to `worker` and load `payload` into it.
    ///
    /// The returned [`TmuxBuffer`] carries both the per-call unique buffer
    /// name and the worker's session name, so the only subsequent paste
    /// path ([`Self::paste_buffer`]) cannot target a different session
    /// than the one the buffer was minted for.
    ///
    /// On failure, any partial tmux buffer is deleted best-effort before
    /// returning — see the module-level invariant.
    fn load_buffer<'a>(
        &'a self,
        worker: &WorkerId,
        payload: &[u8],
    ) -> Result<TmuxBuffer<'a>, TransportError> {
        let sessions = self.list_sessions()?;
        let session = sessions
            .iter()
            .find(|s| s.worker_id == *worker)
            .ok_or_else(|| TransportError::NotFound(worker.clone()))?;

        let n = BUF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "cosmon-input-{}-{}-{}",
            worker.name(),
            std::process::id(),
            n,
        );
        let tmp = std::env::temp_dir().join(&name);
        std::fs::write(&tmp, payload)
            .map_err(|e| TransportError::Io(format!("failed to write temp file: {e}")))?;

        let tmp_path = tmp.to_string_lossy().into_owned();
        let load_result = self.tmux_cmd(&["load-buffer", "-b", &name, &tmp_path]);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = load_result {
            let _ = self.tmux_cmd(&["delete-buffer", "-b", &name]);
            return Err(TransportError::Io(format!("load-buffer failed: {e}")));
        }

        Ok(TmuxBuffer {
            backend: self,
            session: session.session_name.clone(),
            name,
            consumed: false,
        })
    }

    /// Consume the handle and paste into its bound session.
    ///
    /// The target session is read from the handle, not passed as an
    /// argument, so `(buffer, session)` pairs cannot be mismatched.
    /// The `-d` flag asks tmux to delete the buffer after pasting; on
    /// success we mark the handle consumed so [`Drop`] does not re-delete.
    fn paste_buffer(&self, mut buffer: TmuxBuffer<'_>) -> Result<(), TransportError> {
        self.tmux_cmd(&[
            "paste-buffer",
            "-d",
            // Have tmux delimit the entire buffer as one bracketed paste when
            // the TUI has enabled that terminal mode.  This makes the closing
            // marker precede the separately sent Enter in the pty byte stream.
            "-p",
            "-b",
            &buffer.name,
            "-t",
            &buffer.session,
        ])
        .map_err(|e| TransportError::Io(format!("paste-buffer failed: {e}")))?;
        buffer.consumed = true;
        Ok(())
    }

    /// Press "submit" in `session_name`'s composer.
    ///
    /// The one place the submit keystroke is spelled. Every submit — the bare
    /// nudge, the post-paste Enter, and each re-`Enter` of the retry loop —
    /// funnels through here so the encoding cannot drift between call sites
    /// (the asymmetry that let one path be fixed while another kept hanging).
    /// See [`SUBMIT_KEY_HEX`] for why the byte is sent rather than a key name.
    fn press_submit(&self, session_name: &str) -> Result<(), TransportError> {
        self.tmux_cmd(&["send-keys", "-t", session_name, "-H", SUBMIT_KEY_HEX])
            .map(|_| ())
            .map_err(|e| TransportError::Io(format!("send-keys submit failed: {e}")))
    }

    /// Best-effort check: is `input` still sitting unsubmitted in the
    /// worker's TUI input box?
    ///
    /// After a successful submit the pasted text scrolls up into the
    /// conversation and the input box at the bottom of the pane clears.
    /// If the Enter keypress was swallowed (the TUI was busy), the *tail*
    /// of the pasted text is still the last visible content. We capture
    /// the visible pane and look for the last non-empty line of `input`
    /// among the bottom few non-empty lines: present ⇒ not submitted.
    ///
    /// Returns `false` on any capture error — this check is advisory and
    /// must never escalate a delivered paste into a failure.
    fn input_still_pending(&self, session_name: &str, input: &str) -> bool {
        let Ok(raw) = self.tmux_cmd(&["capture-pane", "-t", session_name, "-p"]) else {
            return false;
        };
        composer_indicates_pending(&raw, input)
    }

    /// Spawn-time public check: is `input` still sitting pasted-but-unsubmitted
    /// in the worker's composer *right now*?
    ///
    /// A thin, session-resolving wrapper over the same composer scan
    /// (`input_still_pending`) that [`TransportBackend::send_input`]'s
    /// internal submit loop uses. It is exposed so the `cs tackle` spawn path
    /// can run a **longer, spawn-scale** submit-confirmation than a single
    /// `send_input` budget affords: a fresh Claude worker rendering a large
    /// briefing paste can stay busy past `send_input`'s ~6 s budget and swallow
    /// every re-`Enter`, leaving the worker idle on `❯ [Pasted text …]` — the
    /// exact never-started stall reported on the 2026-07-20 knowledge fleet.
    /// The caller polls this and re-nudges `Enter` until the worker leaves the
    /// composer (or a spawn deadline passes).
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::NotFound`] if no live session matches `id`.
    pub fn input_pending_for(&self, id: &WorkerId, input: &str) -> Result<bool, TransportError> {
        let sessions = self.list_sessions()?;
        let session = sessions
            .iter()
            .find(|s| s.worker_id == *id)
            .ok_or_else(|| TransportError::NotFound(id.clone()))?;
        Ok(self.input_still_pending(&session.session_name, input))
    }

    /// Install a `pane-died` hook on `session_name` that shells out to
    /// `command` when any pane in the session exits.
    ///
    /// The hook is the structural closure of the "worker-exit ⇒ someone
    /// listens" channel: the kernel sees the process die, tmux translates
    /// that into a `pane-died` notification, and this hook translates the
    /// notification into a shell command. Crucially the shell runs as a
    /// **sibling** of the worker — it inherits tmux's environment, not the
    /// worker's worktree cwd — which preserves the `cs done` =
    /// *not-the-worker* invariant.
    ///
    /// The hook is appended (`-a`) rather than replacing, so an earlier
    /// install (e.g. tackle-time) and a later one (e.g. `graceful_exit`
    /// signaling) both fire — tmux chains them in install order. Callers
    /// are responsible for quoting `command` safely; single quotes inside
    /// `command` will break the outer `run-shell '…'` wrapper.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Io`] if the underlying `tmux set-hook`
    /// call fails.
    pub fn install_pane_died_hook(
        &self,
        session_name: &str,
        command: &str,
    ) -> Result<(), TransportError> {
        // Outer wrapper uses double quotes — embedded single quotes in the
        // command (shell-escaped paths like `cd '/Users/you/…'`) then
        // pass through tmux's parser cleanly. Tmux still expands
        // `#{pane_dead_status}` inside double quotes.
        //
        // We only need to escape characters tmux treats specially inside
        // `"..."`: `"`, `$`, `\`, and backtick. The shell-escape of the
        // caller (single-quote wrapping) doesn't produce any of these in
        // normal paths / mol_ids, but the replacement is defensive.
        let escaped = command
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`");
        // The `pane-died` event only fires when the pane's window keeps the
        // dead pane around — i.e. `remain-on-exit on`. Without it, tmux
        // reaps the pane the instant the worker process exits and the hook
        // never runs (verified on tmux 3.5a, 2026-06-14). This was the
        // latent bug behind ADR-052 child #4 reading as "shipped" while
        // the kernel-level witness never actually fired on a hard death.
        // Setting it here, where the hook is armed, is the structural pair:
        // arming a `pane-died` hook without `remain-on-exit` is a no-op.
        //
        // Safe by construction: `list_sessions`/`parse_pane_listing` already
        // filter `pane_dead=1` carcasses (the function this module documents
        // for the `remain-on-exit` case), so a lingering dead pane never
        // reads as a live worker — `is_alive` stays honest, and `cs done`
        // kills the session on teardown.
        self.tmux_cmd(&["set-option", "-t", session_name, "remain-on-exit", "on"])
            .map_err(|e| TransportError::Io(format!("set-option remain-on-exit failed: {e}")))?;
        // `-a` appends to any existing hook, so a tackle-time harvest hook
        // and a graceful-exit signal hook can coexist.
        self.tmux_cmd(&[
            "set-hook",
            "-a",
            "-t",
            session_name,
            "pane-died",
            &format!("run-shell \"{escaped}\""),
        ])
        .map_err(|e| TransportError::Io(format!("set-hook pane-died failed: {e}")))?;
        Ok(())
    }

    /// Run a tmux command and return its stdout on success.
    fn tmux_cmd(&self, args: &[&str]) -> Result<String, TransportError> {
        let output = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .map_err(|e| TransportError::Io(format!("failed to run tmux: {e}")))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(TransportError::Io(format!("tmux error: {stderr}")))
        }
    }
}

impl TransportBackend for TmuxBackend {
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError> {
        let worker_id = WorkerId::new(agent.id.as_str())
            .map_err(|e| TransportError::SpawnFailed(e.to_string()))?;
        let session = Self::session_name(config, &worker_id);

        // Build the command string with proper shell quoting
        let mut full_cmd = Self::shell_quote(&agent.command);
        for arg in &agent.args {
            full_cmd.push(' ');
            full_cmd.push_str(&Self::shell_quote(arg));
        }

        self.tmux_cmd(&["new-session", "-d", "-s", &session, &full_cmd])
            .map_err(|e| TransportError::SpawnFailed(format!("tmux new-session failed: {e}")))?;

        Ok(SpawnHandle {
            id: worker_id,
            session_name: session,
        })
    }

    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
        // List sessions to find the one matching this worker
        let sessions = self.list_sessions()?;
        let session = sessions
            .iter()
            .find(|s| s.worker_id == *id)
            .ok_or_else(|| TransportError::NotFound(id.clone()))?;

        self.tmux_cmd(&["kill-session", "-t", &session.session_name])
            .map_err(|e| TransportError::Io(format!("kill-session failed: {e}")))?;

        Ok(())
    }

    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        let sessions = self.list_sessions()?;
        Ok(sessions.iter().any(|s| s.worker_id == *id))
    }

    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError> {
        if input.is_empty() {
            let sessions = self.list_sessions()?;
            let session = sessions
                .iter()
                .find(|s| s.worker_id == *id)
                .ok_or_else(|| TransportError::NotFound(id.clone()))?;
            self.press_submit(&session.session_name)?;
            return Ok(());
        }

        // load-buffer + paste-buffer handles arbitrarily long input that
        // `send-keys` would silently truncate. The RAII TmuxBuffer handle
        // ensures the buffer name is per-call unique and the paste target
        // is coupled to the minted buffer — see module header.
        let buf = self.load_buffer(id, input.as_bytes())?;
        let session_name = buf.session.clone();

        // Clear the worker's input line before pasting. If a previous
        // nudge is still sitting unsubmitted in the TUI input box — the
        // Enter was swallowed because the agent was busy, see
        // task-20260514-97f4 — `C-u` wipes it so the fresh paste does not
        // stack on top of stale text. This bounds the damage to one lost
        // message instead of an unbounded pile of un-submitted nudges.
        let _ = self.tmux_cmd(&["send-keys", "-t", &session_name, "C-u"]);

        self.paste_buffer(buf)?;

        // Brief pause to let the paste complete and the UI render.
        std::thread::sleep(std::time::Duration::from_millis(500));

        self.press_submit(&session_name)?;

        // The Enter above is fire-and-forget: when the TUI is busy the
        // keypress is dropped silently and the paste sits unsubmitted — and
        // because Claude collapses a large paste into a `[Pasted text #N]`
        // placeholder, the literal text never appears in the pane, so the
        // pre-fix single tail-match check missed the stall entirely. That was
        // the intermittent idle-not-submitted worker (`task-20260605-a307`):
        // a `running` molecule whose pane sat on `❯` with pastes piling up,
        // burning a fleet slot for zero tokens.
        //
        // Poll the input zone and re-send Enter until it clears or the retry
        // budget is spent. The budget auto-scales with prompt size: a large
        // brief pastes as several stacked `[Pasted text #N]` blocks and the
        // TUI stays busy long enough to swallow the fixed-budget five Enters,
        // leaving the worker idle on `❯` with an unsubmitted paste
        // (`galaxy-drain-playhouse-7ce2`). One extra re-Enter cycle per estimated
        // block keeps pressing until the multi-block paste registers.
        //
        // Verification is best-effort — a capture failure must never turn a
        // delivered paste into a hard error, so a poll that cannot read the
        // pane reads as "not pending" and ends the loop.
        //
        // `composer_indicates_pending` scopes the literal-tail check to the
        // visible composer, so Codex's submitted transcript echo no longer
        // masquerades as a pending paste.
        let budget = submit_retry_budget(input);
        let cleared = drive_submit(
            budget,
            || {
                std::thread::sleep(std::time::Duration::from_millis(SUBMIT_POLL_INTERVAL_MS));
                self.input_still_pending(&session_name, input)
            },
            || {
                let _ = self.press_submit(&session_name);
            },
        );

        // Observability (review task-20260711-a7a5, H2). Without this the
        // budget-exhaustion path was silent: a chronically-failing submit only
        // became visible once the patrol noticed the worker idling. Emit a
        // warning so a stuck submission is visible to patrol operators before
        // its next propulsion pass.
        if !cleared {
            tracing::warn!(
                session = %session_name,
                budget,
                "send_input submit budget exhausted; input may be unsubmitted, relying on patrol backstop"
            );
        }

        Ok(())
    }

    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        let sessions = self.list_sessions()?;
        let session = sessions
            .iter()
            .find(|s| s.worker_id == *id)
            .ok_or_else(|| TransportError::NotFound(id.clone()))?;

        // Capture the scrollback history. `-S -` = start of history,
        // then we trim to the requested number of lines.
        let raw = self.tmux_cmd(&[
            "capture-pane",
            "-t",
            &session.session_name,
            "-p", // print to stdout
            "-S", // start line
            "-",  // beginning of history
        ])?;

        // Trim trailing empty lines, then take the last `lines` lines.
        let all: Vec<&str> = raw.lines().collect();
        let trimmed: Vec<&str> = {
            let end = all.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
            all[..end].to_vec()
        };
        let start = trimmed.len().saturating_sub(lines);
        Ok(trimmed[start..].join("\n"))
    }

    fn graceful_exit(
        &self,
        id: &WorkerId,
        timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        let sessions = self.list_sessions()?;
        let session = sessions
            .iter()
            .find(|s| s.worker_id == *id)
            .ok_or_else(|| TransportError::NotFound(id.clone()))?;
        let session_name = session.session_name.clone();

        // Install a pane-died hook that signals a wait-for channel.
        // This gives us event-driven exit detection (no polling).
        let channel = format!("exit-{session_name}");
        let signal_cmd = format!(
            "tmux -L {} wait-for -S {}",
            Self::shell_quote(&self.socket),
            Self::shell_quote(&channel)
        );
        let _ = self.tmux_cmd(&[
            "set-hook",
            "-t",
            &session_name,
            "pane-died",
            &format!("run-shell '{signal_cmd}'"),
        ]);

        // Send /exit to Claude Code's interactive prompt.
        self.send_input(id, "/exit")?;

        // Block on wait-for with a timeout via a spawned process.
        let socket = self.socket.clone();
        let chan = channel.clone();
        let waiter = std::thread::spawn(move || {
            Command::new("tmux")
                .args(["-L", &socket, "wait-for", &chan])
                .output()
        });

        // Wait for the thread to complete, or timeout.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if waiter.is_finished() {
                // Process exited cleanly.
                let _ = waiter.join();
                return Ok(true);
            }
            if std::time::Instant::now() >= deadline {
                // Timeout — signal the channel to unblock the waiter thread,
                // then force-kill.
                let _ = self.tmux_cmd(&["wait-for", "-S", &channel]);
                let _ = waiter.join();
                // Force-kill: terminate may fail on attached sessions,
                // so also try direct kill-session by name as fallback.
                let _ = self.terminate(id);
                if self.is_alive(id).unwrap_or(false) {
                    let _ = self.tmux_cmd(&["kill-session", "-t", &session_name]);
                }
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        // We query at pane granularity because `pane_dead` is a per-pane
        // attribute. When a tmux config carries `set -g remain-on-exit on`
        // (common in operator shells) a session whose wrapped process has
        // exited still appears in `list-sessions`, even though no live
        // child is attached. Treating that carcass as alive is exactly the
        // surface-lie bug behind task-4046: `is_alive` returned `true`,
        // `cs tackle` wrote `Running`, and the operator saw a healthy row
        // while claude was long gone. Filtering on `pane_dead` here is the
        // single-place fix that repairs every downstream check.
        let output = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(["list-panes", "-a", "-F", "#{session_name}|#{pane_dead}"])
            .output()
            .map_err(|e| TransportError::Io(format!("failed to run tmux: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Various tmux messages that mean "no sessions exist" — not an error
            if stderr.contains("no server running")
                || stderr.contains("no sessions")
                || stderr.contains("error connecting")
            {
                return Ok(Vec::new());
            }
            return Err(TransportError::Io(format!(
                "tmux list-panes failed: {stderr}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_pane_listing(&stdout))
    }
}

/// Parse `tmux list-panes -aF '#{session_name}|#{pane_dead}'` output into
/// the list of sessions that still have at least one live pane.
///
/// A session appears multiple times (once per pane). We treat a session as
/// alive when *any* of its panes has `pane_dead=0`. Sessions whose only
/// pane has died (`pane_dead=1`, the `[exited]` carcass under
/// `remain-on-exit`) are excluded. Names that do not parse as a `WorkerId`
/// are silently skipped — those belong to other tools sharing the socket.
fn parse_pane_listing(stdout: &str) -> Vec<SessionInfo> {
    use std::collections::BTreeMap;
    let mut alive: BTreeMap<String, bool> = BTreeMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (name, dead_flag) = match line.split_once('|') {
            Some((n, d)) => (n.trim(), d.trim()),
            None => (line, "0"),
        };
        if name.is_empty() {
            continue;
        }
        let pane_alive = dead_flag != "1";
        // Any live pane promotes the session to alive and stays that way.
        let entry = alive.entry(name.to_owned()).or_insert(false);
        if pane_alive {
            *entry = true;
        }
    }

    let mut sessions = Vec::new();
    for (name, is_alive) in alive {
        if !is_alive {
            continue;
        }
        // Sessions we didn't create (foreign workers on the socket) have
        // names that don't parse as `WorkerId`. Skip silently.
        if let Ok(worker_id) = WorkerId::new(&name) {
            sessions.push(SessionInfo {
                worker_id,
                session_name: name,
            });
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::agent::AgentRole;
    use cosmon_core::id::AgentId;

    #[test]
    fn parse_pane_listing_drops_dead_only_session() {
        // Canned output from `tmux list-panes -aF '#{session_name}|#{pane_dead}'`
        // under a config with `remain-on-exit on`. `carcass` is the
        // task-4046 failure mode: the wrapped process exited, but the
        // session persists. `alive` has a live pane and must survive.
        let canned = "alive-session|0\ncarcass-session|1\n";
        let out = parse_pane_listing(canned);
        let names: Vec<&str> = out.iter().map(|s| s.session_name.as_str()).collect();
        assert_eq!(names, vec!["alive-session"]);
    }

    #[test]
    fn parse_pane_listing_promotes_on_any_live_pane() {
        // A session with multiple panes where only one is dead stays alive.
        let canned = "mixed|1\nmixed|0\n";
        let out = parse_pane_listing(canned);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_name, "mixed");
    }

    #[test]
    fn parse_pane_listing_skips_non_worker_names() {
        // Names that don't parse as WorkerId (e.g. shared dev sockets)
        // are silently dropped — they belong to other tools.
        let canned = "alive-ok|0\nfoo bar|0\n";
        let out = parse_pane_listing(canned);
        let names: Vec<&str> = out.iter().map(|s| s.session_name.as_str()).collect();
        assert_eq!(names, vec!["alive-ok"]);
    }

    #[test]
    fn parse_pane_listing_empty_input() {
        assert!(parse_pane_listing("").is_empty());
    }

    #[test]
    fn pending_detects_collapsed_paste_placeholder() {
        // The task-20260605-a307 failure mode: a large prompt was pasted, the
        // TUI collapsed it to a placeholder, and the literal text is nowhere
        // in the pane. The tail-only check used to miss this and never retry
        // the dropped Enter. Detecting the placeholder makes the stall visible.
        let pane = "\
 some earlier output
╭──────────────────────────────────────╮
│ > [Pasted text #1 +312 lines]        │
╰──────────────────────────────────────╯
 ❯";
        let original_prompt = "line one of a very long brief\nfinal line of the brief";
        assert!(composer_indicates_pending(pane, original_prompt));
    }

    #[test]
    fn pending_detects_codex_collapsed_paste_placeholder() {
        // Codex uses a different placeholder from Claude. Missing this token
        // made the shared verifier report "cleared" while the prompt was
        // visibly still waiting in Codex's composer.
        let pane = "\
 OpenAI Codex
╭──────────────────────────────────╮
│ > [Pasted Content 3578 chars]      │
╰──────────────────────────────────╯";
        assert!(composer_indicates_pending(
            pane,
            "first line of the bootstrap\nlast line of the bootstrap"
        ));
    }

    #[test]
    fn pending_detects_verbatim_small_prompt() {
        // Small prompts the TUI renders verbatim (no placeholder) must still
        // be caught by the last-line tail match.
        let pane = "\
 welcome banner
 ❯ continue the work now";
        assert!(composer_indicates_pending(pane, "continue the work now"));
    }

    #[test]
    fn pending_detects_multiline_verbatim_codex_composer() {
        // The glyph composer can render a short Codex paste verbatim over
        // several lines. Looking only at the glyph line loses the input tail
        // and turns a real pending paste into a false "cleared".
        let pane = "\
 OpenAI Codex
 › first line of the brief
   final line of the brief";
        assert!(composer_indicates_pending(
            pane,
            "first line of the brief\nfinal line of the brief"
        ));
    }

    #[test]
    fn pending_detects_placeholder_when_box_top_is_scrolled_or_stray() {
        // A tall composer may have its `╭` above the capture viewport; a
        // transcript decoration can also leave an unmatched later `╭`. The
        // visible collapsed-paste interior must remain a pending signal.
        let pane = "\
 │ > [Pasted Content 3578 chars] │
 │   [Pasted Content 181 chars]  │
 ╭ transcript decoration without a closing border
 ❯";
        assert!(composer_indicates_pending(
            pane,
            "first line of the bootstrap\nlast line of the bootstrap"
        ));
    }

    #[test]
    fn pending_false_when_input_zone_cleared() {
        // After a successful submit the bottom is the empty chevron / working
        // spinner — neither the placeholder nor the prompt tail is present.
        let pane = "\
 ⏺ Reading files...
 ❯";
        assert!(!composer_indicates_pending(pane, "continue the work now"));
    }

    #[test]
    fn pending_false_after_codex_submit_echoes_prompt_in_transcript() {
        // Codex prints a submitted multi-line prompt verbatim immediately
        // above its now-empty composer. The verifier must inspect only that
        // composer, not mistake transcript text for an unsubmitted paste.
        let pane = "\
 brief line 59 of the bootstrap prompt
 • Paste rendering test received: 60 lines
 › Explain this codebase";
        assert!(!composer_indicates_pending(
            pane,
            "brief line 0 of the bootstrap prompt\nbrief line 59 of the bootstrap prompt"
        ));
    }

    #[test]
    fn pending_ignores_placeholder_scrolled_into_transcript() {
        // A placeholder that has scrolled up past the bottom 8 non-empty
        // lines belongs to an already-submitted message and must not read as
        // a fresh pending paste.
        let mut pane = String::from(" > [Pasted text #1 +9 lines]\n");
        for i in 0..12 {
            use std::fmt::Write as _;
            let _ = writeln!(pane, " output line {i}");
        }
        pane.push_str(" ❯");
        assert!(!composer_indicates_pending(&pane, "some prompt tail"));
    }

    #[test]
    fn pending_false_on_empty_input() {
        // A bare-Enter nudge (empty input) has no tail to match and no
        // placeholder of its own — never reads as pending.
        assert!(!composer_indicates_pending(" ❯", ""));
    }

    /// A realistic input box for a large brief that Claude Code collapsed into
    /// five stacked `[Pasted text #N]` blocks — the `galaxy-drain-playhouse-7ce2`
    /// stall shape.
    const FIVE_BLOCK_PANE: &str = "\
 ⏺ ready
╭───────────────────────────────────────────╮
│ > [Pasted text #1 +13 lines]              │
│   [Pasted text #2 +12 lines]              │
│   [Pasted text #3 +20 lines]              │
│   [Pasted text #4 +15 lines]              │
│   [Pasted text #5 +24 lines]              │
╰───────────────────────────────────────────╯
  ? for shortcuts";

    #[test]
    fn pending_detects_stacked_multiblock_paste() {
        // The LARGE-prompt regression: a brief big enough to collapse into
        // five `[Pasted text]` blocks must still read as pending even with the
        // box border and a hint line below it pushing the placeholders up.
        let big_prompt = "brief line\n".repeat(84) + "final line of the brief";
        assert!(
            composer_indicates_pending(FIVE_BLOCK_PANE, &big_prompt),
            "a 5-block stacked paste must read as pending"
        );
    }

    #[test]
    fn submit_retry_budget_baseline_for_small_prompt() {
        // Small prompts keep the baseline budget — no needless extra polling.
        assert_eq!(submit_retry_budget("continue the work now"), 5);
        assert_eq!(submit_retry_budget(""), 5);
    }

    #[test]
    fn submit_retry_budget_scales_with_large_prompt() {
        // A ~85-line brief (≈7 estimated blocks) earns well above the baseline
        // five cycles so the multi-block paste has time to register.
        let big_prompt = "brief line\n".repeat(84) + "final line of the brief";
        let budget = submit_retry_budget(&big_prompt);
        assert!(
            budget > SUBMIT_RETRY_BUDGET_BASE,
            "large multi-block prompt must scale the budget above baseline, got {budget}"
        );
        assert!(
            budget <= SUBMIT_RETRY_BUDGET_MAX,
            "budget must stay capped at the ceiling, got {budget}"
        );
    }

    #[test]
    fn submit_retry_budget_caps_at_ceiling() {
        // A pathologically huge prompt must not grant an unbounded budget —
        // the worst-case polling wall-clock stays bounded.
        let huge = "x\n".repeat(10_000);
        assert_eq!(submit_retry_budget(&huge), SUBMIT_RETRY_BUDGET_MAX);
    }

    #[test]
    fn drive_submit_converges_on_multiblock_after_swallowed_enters() {
        // The core fix, simulated end-to-end without a live TUI: a 5-block
        // large paste whose first four Enters are swallowed while the TUI
        // settles, then the input zone clears. With the auto-scaled budget the
        // loop keeps re-Entering long enough to win.
        let big_prompt = "brief line\n".repeat(84) + "final line of the brief";
        let budget = submit_retry_budget(&big_prompt);

        let cleared_pane = " ⏺ Working...\n ❯";
        let mut polls = 0u32;
        let mut enters = 0u32;
        let cleared = drive_submit(
            budget,
            || {
                polls += 1;
                // Pending for the first four polls (Enters swallowed), then
                // the pane clears.
                let pane = if polls <= 4 {
                    FIVE_BLOCK_PANE
                } else {
                    cleared_pane
                };
                composer_indicates_pending(pane, &big_prompt)
            },
            || enters += 1,
        );

        assert!(
            cleared,
            "input zone must clear within the auto-scaled budget"
        );
        assert_eq!(
            enters, 4,
            "exactly the four swallowed Enters get re-sent before clearance"
        );
    }

    #[test]
    fn drive_submit_gives_up_when_input_never_clears() {
        // If the paste never registers, the loop must terminate after the
        // budget is spent rather than hang — the propulsion nudge is the
        // backstop. It re-Enters once per budgeted cycle.
        let big_prompt = "x\n".repeat(60);
        let budget = submit_retry_budget(&big_prompt);
        let mut enters = 0u32;
        let cleared = drive_submit(
            budget,
            || composer_indicates_pending(FIVE_BLOCK_PANE, &big_prompt),
            || enters += 1,
        );
        assert!(!cleared, "a never-clearing input must report not-cleared");
        assert_eq!(enters, budget, "every budgeted cycle re-sends Enter");
    }

    fn test_config(socket: &str) -> RuntimeConfig {
        RuntimeConfig {
            socket_name: socket.to_owned(),
            session_prefix: String::new(),
        }
    }

    fn sleep_agent(name: &str) -> AgentDefinition {
        AgentDefinition {
            id: AgentId::new(name).unwrap(),
            role: AgentRole::Implementation,
            command: "sleep".to_owned(),
            args: vec!["300".to_owned()],
        }
    }

    /// Clean up any leftover test sessions on a given socket.
    fn cleanup(socket: &str) {
        let _ = Command::new("tmux")
            .args(["-L", socket, "kill-server"])
            .output();
    }

    #[test]
    fn test_spawn_and_alive() {
        let sock = "cosmon-test-spawn";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);
        let agent = sleep_agent("test-agent");

        let worker = backend.spawn(&agent, &config).expect("spawn failed");
        assert_eq!(worker.id.name(), "test-agent");

        let alive = backend.is_alive(&worker.id).expect("is_alive failed");
        assert!(alive, "worker should be alive after spawn");

        backend.terminate(&worker.id).expect("terminate failed");

        let alive_after = backend.is_alive(&worker.id).expect("is_alive failed");
        assert!(!alive_after, "worker should be dead after terminate");

        cleanup(sock);
    }

    #[test]
    fn test_capture_output() {
        let sock = "cosmon-test-capture";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);

        // Use sh -c so the session stays alive after echo
        let agent = AgentDefinition {
            id: AgentId::new("echo-agent").unwrap(),
            role: AgentRole::Implementation,
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), "echo hello-cosmon && sleep 300".to_owned()],
        };

        let worker = backend.spawn(&agent, &config).expect("spawn failed");

        // Give tmux time to start the session and run the echo command
        std::thread::sleep(std::time::Duration::from_secs(1));

        let output = backend
            .capture_output(&worker.id, 10)
            .expect("capture failed");
        assert!(
            output.contains("hello-cosmon"),
            "output should contain 'hello-cosmon', got: {output}"
        );

        let _ = backend.terminate(&worker.id);
        cleanup(sock);
    }

    #[test]
    fn test_list_sessions() {
        let sock = "cosmon-test-list";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);

        // Should be empty initially (no server running)
        let sessions = backend.list_sessions().expect("list failed");
        assert!(sessions.is_empty());

        let agent = sleep_agent("list-agent");
        let worker = backend.spawn(&agent, &config).expect("spawn failed");

        let sessions = backend.list_sessions().expect("list failed");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].worker_id, worker.id);

        backend.terminate(&worker.id).expect("terminate failed");
        cleanup(sock);
    }

    #[test]
    fn test_send_input_long_text() {
        let sock = "cosmon-test-sendinput";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);

        // Use cat so the session reads stdin and stays alive.
        let agent = AgentDefinition {
            id: AgentId::new("cat-agent").unwrap(),
            role: AgentRole::Implementation,
            command: "cat".to_owned(),
            args: vec![],
        };

        let worker = backend.spawn(&agent, &config).expect("spawn failed");
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Build a long string that would fail with plain send-keys.
        let long_text = "A".repeat(2000);
        backend
            .send_input(&worker.id, &long_text)
            .expect("send_input failed for long text");

        std::thread::sleep(std::time::Duration::from_millis(500));

        let output = backend
            .capture_output(&worker.id, 100)
            .expect("capture failed");
        // tmux scrollback is limited, so we can't capture all 2000 chars.
        // Verify that a substantial amount was pasted (send-keys would truncate
        // much more aggressively or fail entirely with 2000 chars).
        let joined: String = output.lines().collect();
        let a_count = joined.chars().filter(|&c| c == 'A').count();
        assert!(
            a_count >= 500,
            "expected at least 500 'A' chars in output, got {a_count}",
        );

        let _ = backend.terminate(&worker.id);
        cleanup(sock);
    }

    #[test]
    fn submit_is_a_carriage_return_even_under_extended_keys() {
        // task-20260724-c014 regression, at the byte seam.
        //
        // Claude Code v2.x negotiates an extended-key protocol at startup. On a
        // host whose tmux server has `extended-keys on`, tmux answers a *named*
        // key by re-encoding it: `send-keys C-m` becomes `\x1b[27;5;109~`, which
        // no composer reads as "submit" — the worker then sits forever on
        // `❯ [Pasted text …]`. This probe is a tiny TUI that requests both the
        // `modifyOtherKeys` and kitty protocols and records the raw pty byte of
        // one submit. The oracle is the application's own capture, so it fails
        // the moment the submit stops being a bare CR.
        let sock = format!("cosmon-test-extkeys-{}", std::process::id());
        cleanup(&sock);
        let backend = TmuxBackend::new(&sock);
        let config = test_config(&sock);
        let capture = tempfile::NamedTempFile::new().expect("create pty capture");
        let capture_path = capture.path().to_string_lossy();
        let script = format!(
            "stty -icanon -echo -icrnl -inlcr -igncr min 1 time 0; \
             printf '\\033[>4;2m\\033[>1u'; dd bs=1 count=1 of={} 2>/dev/null",
            TmuxBackend::shell_quote(&capture_path),
        );
        let agent = AgentDefinition {
            id: AgentId::new("extkeys-agent").unwrap(),
            role: AgentRole::Implementation,
            command: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script],
        };
        let worker = backend.spawn(&agent, &config).expect("spawn probe TUI");
        // Turn the hostile server option on *for this socket only*, so the test
        // reproduces the operator configuration that breaks a named key.
        backend
            .tmux_cmd(&["set", "-s", "extended-keys", "on"])
            .expect("enable extended-keys");
        std::thread::sleep(std::time::Duration::from_millis(300));

        backend
            .send_input(&worker.id, "")
            .expect("bare submit failed");
        std::thread::sleep(std::time::Duration::from_millis(300));

        let captured = std::fs::read(capture.path()).expect("read pty capture");
        assert_eq!(
            captured, b"\r",
            "submit must reach the pty as a bare CR under `extended-keys on`; \
             a named key would arrive re-encoded and never submit"
        );

        let _ = backend.terminate(&worker.id);
        cleanup(&sock);
    }

    #[test]
    fn send_input_emits_bracketed_paste_close_before_enter_on_pty() {
        // Live tmux seam probe: this tiny TUI first consumes Cosmon's C-u,
        // then explicitly requests bracketed-paste mode and records the raw
        // pty bytes of one `send_input`. The oracle is the application's byte
        // capture, independent of the implementation under test.
        let sock = format!("cosmon-test-bracketed-{}", std::process::id());
        cleanup(&sock);
        let backend = TmuxBackend::new(&sock);
        let config = test_config(&sock);
        let capture = tempfile::NamedTempFile::new().expect("create pty capture");
        let capture_path = capture.path().to_string_lossy();
        let input = "first\nsecond";
        let byte_count = input.len() + b"\x1b[200~".len() + b"\x1b[201~".len() + 1;
        let script = format!(
            "stty -icanon -echo -icrnl -inlcr -igncr min 1 time 0; dd bs=1 count=1 of=/dev/null 2>/dev/null; printf '\\033[?2004h'; dd bs=1 count={byte_count} of={} 2>/dev/null",
            TmuxBackend::shell_quote(&capture_path),
        );
        let agent = AgentDefinition {
            id: AgentId::new("bracketed-agent").unwrap(),
            role: AgentRole::Implementation,
            command: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script],
        };
        let worker = backend.spawn(&agent, &config).expect("spawn probe TUI");
        std::thread::sleep(std::time::Duration::from_millis(200));

        backend
            .send_input(&worker.id, input)
            .expect("send bracketed input");

        let captured = std::fs::read(capture.path()).expect("read pty capture");
        // tmux's normal paste path maps buffer line feeds to carriage returns;
        // assert that real pty representation, rather than deriving an oracle
        // from `paste_buffer`'s implementation.
        let expected = [
            b"\x1b[200~".as_slice(),
            b"first\rsecond".as_slice(),
            b"\x1b[201~\r".as_slice(),
        ]
        .concat();
        assert_eq!(
            captured, expected,
            "pty bytes must close the paste before Enter"
        );

        let _ = backend.terminate(&worker.id);
        cleanup(&sock);
    }

    #[test]
    fn install_pane_died_hook_sets_remain_on_exit() {
        // C2 regression: `pane-died` only fires when the window keeps the
        // dead pane (`remain-on-exit on`). Arming the hook MUST set it, or
        // the kernel-level witness silently never fires.
        let sock = "cosmon-test-remain-on-exit";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);
        let agent = sleep_agent("roe-agent");
        let worker = backend.spawn(&agent, &config).expect("spawn failed");

        backend
            .install_pane_died_hook("roe-agent", "true")
            .expect("install hook failed");

        let opt = backend
            .tmux_cmd(&["show-options", "-t", "roe-agent", "remain-on-exit"])
            .expect("show-options failed");
        assert!(
            opt.contains("on"),
            "remain-on-exit must be 'on' after arming the pane-died hook, got: {opt:?}"
        );

        let _ = backend.terminate(&worker.id);
        cleanup(sock);
    }

    #[test]
    fn input_pending_for_detects_unsubmitted_paste() {
        // BUG #6 seam: the spawn-time submit-confirmation reads pending state
        // through this public wrapper. A worker whose composer shows a
        // collapsed-paste placeholder must read as pending so the tackle
        // backstop keeps nudging Enter.
        let sock = "cosmon-test-pending-for";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);

        // A tiny TUI that prints a composer with the collapsed-paste
        // placeholder and then blocks, so the pane keeps showing it.
        let agent = AgentDefinition {
            id: AgentId::new("pending-agent").unwrap(),
            role: AgentRole::Implementation,
            command: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf '\\342\\235\\257 [Pasted text #1 +86 lines]\\n'; sleep 300".to_owned(),
            ],
        };
        let worker = backend.spawn(&agent, &config).expect("spawn failed");
        std::thread::sleep(std::time::Duration::from_millis(500));

        let pending = backend
            .input_pending_for(&worker.id, "line one\nfinal line of the brief")
            .expect("input_pending_for failed");
        assert!(
            pending,
            "a composer showing `[Pasted text …]` must read as pending"
        );

        // A worker that never existed is NotFound, not a silent false.
        let ghost = WorkerId::new("ghost-pending").unwrap();
        assert!(backend.input_pending_for(&ghost, "x").is_err());

        let _ = backend.terminate(&worker.id);
        cleanup(sock);
    }

    #[test]
    fn test_send_input_empty_sends_enter() {
        let sock = "cosmon-test-empty-input";
        cleanup(sock);

        let backend = TmuxBackend::new(sock);
        let config = test_config(sock);

        let agent = AgentDefinition {
            id: AgentId::new("sh-agent").unwrap(),
            role: AgentRole::Implementation,
            command: "sh".to_owned(),
            args: vec![],
        };

        let worker = backend.spawn(&agent, &config).expect("spawn failed");
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Empty send_input should just press Enter (no error).
        backend
            .send_input(&worker.id, "")
            .expect("send_input empty failed");

        let _ = backend.terminate(&worker.id);
        cleanup(sock);
    }
}
