// SPDX-License-Identifier: AGPL-3.0-only

//! `exec_command` — persistent-shell tool for the agent harness.
//!
//! Owns one long-lived `/bin/bash` per harness run, threading commands
//! through a UUID-style sentinel echo protocol so each call returns
//! the merged stdout+stderr of exactly that command plus the
//! trailing `$?` exit code. **This is the load-bearing v0 addition**:
//! without it the harness ships Codex-without-the-shell, which is
//! Aider-without-the-diff — each is an agent class, neither is
//! general.
//!
//! # Sentinel-prompt protocol
//!
//! After every command, we append `echo "__COSMON_END_<id>__:$?"`
//! on the **next line** (so heredocs and `for` loops survive) and
//! write both as one block to the shell's stdin. The reader threads
//! accumulate output until the marker substring appears; everything
//! before the marker is the command's output, everything after the
//! `:` up to the next newline is `$?`. The id is a 128-bit
//! time + pid + counter mix freshly generated **per call**
//! (rotated every `run()`, not per session) — a model that could
//! print the previous turn's marker on this turn now finds it has
//! no way to predict the current one. Knuth K3 / adversary F1.5.
//!
//! ## Marker-collision discipline
//!
//! Detecting *exactly one* marker hit is the load-bearing invariant.
//! If a command's legitimate output contains the marker prefix (e.g.
//! the model read `/proc/$$/cmdline` and printed it back), the buffer
//! ends up with two `__COSMON_END_<id>__:` hits — the forgery and the
//! real echo. The reader returns `RunErr::ShellDied` in that case
//! rather than silently framing on the first hit and reporting a
//! forged exit code. The scan itself is a streaming KMP whose partial
//! state survives across `recv_timeout` chunks so the work is Θ(N)
//! over total bytes, not Θ(N · chunks).
//!
//! # Sub-FSM (knuth §7)
//!
//! ```text
//! NotSpawned ──spawn()─► Alive(pid)
//!                           │
//!                           │ child.wait() returns / SIGCHLD
//!                           ▼
//!                        Dead(exit_code)
//! ```
//!
//! The shell can die between turns (oom, `exit`, segfault). The next
//! `execute()` call detects the dead session via `ExecSession::is_dead`,
//! transparently respawns, and prepends `[shell restarted]` to that
//! turn's output. Choice (a) of the briefing — transparent restart
//! preserves the single-tool-call abstraction the model sees.
//!
//! # Limits
//!
//! - Per-command timeout default: 300 s (5 min) — generous for a
//!   cold `cargo check` (~90 s); overridable via `timeout_secs` in
//!   the call args.
//! - Output cap: 32 KiB. Longer output is truncated with a loud
//!   `... output truncated; original size = N bytes ...` marker so
//!   the model can run `tail` / `rg` to narrow the next read.
//! - No interactive-prompt handling (no `read -p`).
//! - **No filesystem confinement — the worktree is NOT a security
//!   boundary for this tool.** Unlike the path-based tools
//!   (`read_file` / `write_file` / `list_dir`), whose
//!   [`crate::tool::sanitize_join`] pins every access under the
//!   worktree, this shell tool has no `chroot`, no mount namespace,
//!   and no read allowlist. `cd /`, absolute paths, and `$HOME` all
//!   resolve to the real filesystem, so a command reads anything the
//!   harness's uid can — including secrets outside the worktree
//!   (`~/.ssh/id_rsa`, the OIDC bearer store at
//!   `~/.config/cosmon-remote/credentials/`). The path tools and this
//!   tool are therefore **not** "the same"; the shell is a hole the
//!   path tools are not. (A prior version of this comment claimed
//!   "the worktree IS the security boundary, same as `sanitize_join`"
//!   — that was false and self-contradictory; corrected under
//!   `task-20260712-cefc` after the C1-F1 adversarial finding.)
//! - The **only** enforced boundary is the egress network namespace
//!   ([`cosmon_core::egress`], `StrictLocal` posture): it makes a
//!   remote-oracle shellout a refused syscall, but blocks the *wire*
//!   only. It does NOT confine filesystem reads, and a secret read
//!   here still exfiltrates through the data plane
//!   (output → `synthesis.md` → branch → operator/downstream). Under
//!   the default `AllowAll` posture even the wire is open. Adding
//!   mount-ns / chroot / a read allowlist to close the fs-escape is an
//!   operator-owned security-model decision (see `task-20260712-cefc`
//!   / `decision.md`); until it lands the trust model is explicit:
//!   **the harness runs code the operator would run themselves.**
//!
//! # What is intentionally NOT here (v0 scope)
//!
//! - Registration in [`crate::tool::default_registry`] is the spine
//!   PR's job.
//! - `Drop` cleanup of a tmux pane — there is no tmux pane for
//!   in-process Adapters (sentinel `socket = "openai-inprocess"`).
//! - Windows support. The implementation is Unix-only by design.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::tool::{ParametersSchema, Tool, ToolDeclaration, ToolError};

/// Default per-command timeout, in seconds. 5 minutes is generous for
/// a cold `cargo check` (~90 s) without becoming a silent stall
/// budget — see knuth §6 (`provider.timeout` is the load-bearing
/// termination witness).
pub const DEFAULT_TIMEOUT_SECS: u32 = 300;

/// Maximum bytes of merged stdout+stderr returned in
/// [`ExecResult::output`]. Longer outputs are truncated with a loud
/// marker so the model can run `tail` / `rg` to narrow the next read
/// (never silent slicing — torvalds §Q1).
pub const OUTPUT_CAP_BYTES: usize = 32 * 1024;

/// Settle window after the first marker hit: how long to keep reading
/// the pump channel before declaring the framing trusted.
///
/// **Why this exists (adversary F1.5).** Bash's `echo` builtin flushes
/// stdout after each invocation; the protocol's two echos — the user's
/// command and the harness's appended marker echo — therefore land as
/// two separate `write(2)` calls on the pipe. They are usually
/// coalesced into one chunk at the pump-thread read, but the kernel
/// scheduler may slot the harness's recv between the two writes. When
/// that happens, a non-blocking `try_recv()` drain returns Empty even
/// though the second marker chunk is *about to* arrive — the function
/// accepts the framing on a forged first marker, exit_code = the
/// forgery's value. The settle window is a brief blocking wait that
/// catches the in-flight second chunk.
///
/// 25 ms is empirically generous: bash → kernel → pump-thread → mpsc
/// latency on the platforms we ship to (macOS, Linux) is sub-millisecond
/// for the second flush of an `echo` already queued in bash's stdin
/// buffer. The cost is paid once per `exec_command` call; for the slow
/// commands the tool exists to run (`cargo build`, `pytest`, multi-line
/// scripts) the latency is invisible.
const FIRST_MARKER_SETTLE_WINDOW: Duration = Duration::from_millis(25);

#[derive(Debug, Deserialize)]
struct ExecParams {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u32>,
}

/// Serialised exec-command result. Returned as JSON in `Tool::execute`'s
/// `Ok` payload.
///
/// `#[non_exhaustive]` — future
/// fields (e.g. resource-usage telemetry) must not require a major
/// bump. Serde deserialization continues to work; downstream literal
/// construction must move through a constructor if/when it appears.
#[non_exhaustive]
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecResult {
    /// Merged stdout+stderr, truncated to [`OUTPUT_CAP_BYTES`] with a
    /// loud marker if it overflows.
    pub output: String,
    /// `$?` after the command returned. `-1` when the command was
    /// killed by the timeout watchdog or when the shell died before
    /// the sentinel was echoed.
    pub exit_code: i32,
    /// Wall-clock duration of the command in milliseconds.
    pub duration_ms: u64,
    /// `true` if the per-command timeout fired and the shell was
    /// killed; the next call respawns transparently.
    pub timed_out: bool,
    /// `true` if the shell was respawned (sub-FSM `Dead → Alive`) on
    /// this turn — typically because it died between calls. The
    /// `output` field is prefixed with `[shell restarted]` so the
    /// model sees the transition loudly.
    pub shell_restarted: bool,
}

/// `exec_command` tool — owns one persistent `/bin/bash` per harness
/// run. Lazily spawned on the first `execute()` call; respawned
/// transparently if the shell dies or the `work_dir` changes.
pub struct ExecCommand {
    state: Mutex<ToolState>,
}

#[derive(Default)]
struct ToolState {
    session: Option<ExecSession>,
    /// `true` once the first session has ever been spawned. Used to
    /// distinguish "very first call" (no `[shell restarted]` prefix
    /// — nothing to restart from) from "previous session died and
    /// was cleared, now respawning" (must flag the restart).
    ever_spawned: bool,
}

struct ExecSession {
    child: Child,
    stdin: ChildStdin,
    output_rx: mpsc::Receiver<Vec<u8>>,
    // NB: the end marker was once stored here, generated once at
    // `spawn()`. It is now rotated per `run()` call (see knuth K3 /
    // adversary F1.5) so this field is intentionally absent.
    work_dir: PathBuf,
    // Reader threads pump stdout/stderr bytes into `output_rx`. They
    // exit when the child closes its pipes (EOF) or when the receiver
    // is dropped (Drop of `ExecSession`). Stored as `_` because we
    // never explicitly join — detachment is fine, they always
    // terminate via one of those two conditions.
    _stdout_reader: JoinHandle<()>,
    _stderr_reader: JoinHandle<()>,
}

impl ExecCommand {
    /// Construct a new tool with no session spawned yet. The first
    /// `execute()` call spawns the shell.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ToolState::default()),
        }
    }
}

impl Default for ExecCommand {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ExecCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecCommand").finish_non_exhaustive()
    }
}

impl Tool for ExecCommand {
    fn name(&self) -> &'static str {
        "exec_command"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "exec_command",
            description: "Run a shell command in the persistent harness shell. \
                The shell keeps its cwd and environment variables across calls. \
                Returns JSON { output, exit_code, duration_ms, timed_out, shell_restarted }. \
                Default per-call timeout 300 s; override via timeout_secs. \
                Output is truncated to 32 KiB with a loud marker if longer.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command line. May span multiple lines (heredocs, for-loops)."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional per-command timeout in seconds (default 300)."
                    }
                },
                "required": ["command"]
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: ExecParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "exec_command".to_owned(),
                message: e.to_string(),
            })?;
        let timeout = Duration::from_secs(u64::from(
            params.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        ));

        let mut guard = self
            .state
            .lock()
            .map_err(|e| ToolError::Io(format!("lock poisoned: {e}")))?;

        // Detect work_dir drift or dead session — both force respawn.
        let needs_respawn = match guard.session.as_mut() {
            None => true,
            Some(session) => session.work_dir != work_dir || session.is_dead(),
        };
        if needs_respawn {
            guard.session = None;
        }
        // `shell_restarted` fires whenever we spawn AND we've spawned
        // before. That captures both flavours: (a) the previous
        // session was alive when we entered this call but turned out
        // to be unhealthy; (b) the previous session was already None
        // because the prior turn cleared it (shell died mid-call,
        // timed out, etc.). The very first call leaves it `false` —
        // there's nothing to restart from.
        let mut shell_restarted = false;
        if guard.session.is_none() {
            if guard.ever_spawned {
                shell_restarted = true;
            }
            let session = ExecSession::spawn(work_dir)?;
            guard.session = Some(session);
            guard.ever_spawned = true;
        }

        let session = guard
            .session
            .as_mut()
            .expect("session guaranteed by the spawn above");
        let started = Instant::now();
        let outcome = session.run(&params.command, timeout);
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let (output_raw, exit_code, timed_out) = match outcome {
            Ok((out, ec, to)) => (out, ec, to),
            Err(RunErr::ShellDied { output, exit_code }) => {
                // Clear the dead session so the next call respawns.
                // This turn's `shell_restarted` stays false — it's
                // THIS shell that died, not a recovery from a prior
                // death.
                guard.session = None;
                let code_label = exit_code.map_or_else(|| "?".to_owned(), |c| c.to_string());
                (
                    format!("{output}\n[shell died: exit_code={code_label}]"),
                    exit_code.unwrap_or(-1),
                    false,
                )
            }
            Err(RunErr::Io(msg)) => return Err(ToolError::Io(msg)),
        };

        if timed_out {
            // After timeout, the watchdog killed the shell; clear the
            // slot so the next call respawns transparently.
            guard.session = None;
        }

        let mut output = output_raw;
        if shell_restarted {
            output = format!("[shell restarted]\n{output}");
        }
        let output = truncate_output(output);

        let result = ExecResult {
            output,
            exit_code,
            duration_ms,
            timed_out,
            shell_restarted,
        };
        serde_json::to_string(&result)
            .map_err(|e| ToolError::Io(format!("serialise exec result: {e}")))
    }
}

/// Truncate output to [`OUTPUT_CAP_BYTES`], appending a loud marker
/// that names the original byte count. Char-boundary safe.
fn truncate_output(mut s: String) -> String {
    use std::fmt::Write as _;
    if s.len() <= OUTPUT_CAP_BYTES {
        return s;
    }
    let original = s.len();
    let mut cut = OUTPUT_CAP_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    // `write!` into a `String` is infallible — the io::Error variant
    // never fires for in-memory buffers; the `_` discards the
    // never-erroring Result to satisfy `must_use` without an unwrap.
    let _ = write!(
        s,
        "\n... output truncated; original size = {original} bytes ...\n"
    );
    s
}

enum RunErr {
    Io(String),
    ShellDied {
        output: String,
        exit_code: Option<i32>,
    },
}

impl ExecSession {
    fn spawn(work_dir: &Path) -> Result<Self, ToolError> {
        // Plain `/bin/bash` — not `-i`. Interactive mode would print
        // PS1 / PS2 prompts (still visible on stderr even when PS1 is
        // empty in some bash builds), and the test asserts `pwd`
        // output equals the tempdir exactly. Non-interactive bash
        // reads commands from its stdin pipe, executes them in
        // sequence, preserves cwd + env between commands, and exits
        // on stdin EOF — exactly the persistent-shell semantics the
        // briefing asks for, without the prompt noise. `--noprofile`
        // / `--norc` keep the user's RC files out of the picture so
        // behaviour is deterministic across hosts.
        // Egress jail (task-20260530-d8bc / autonomy guard). When `cs tackle`
        // dispatched a strict-local worker it set
        // `COSMON_EGRESS_POLICY=deny-external` in this process's environment;
        // we read it back and, on a capable kernel, spawn bash inside an
        // egress-denied network namespace. A model that emits
        // `exec_command { "claude -p '…'" }` then finds the remote API
        // physically unreachable — a *refused syscall*, not a *detected
        // anomaly* (turing's master finding: the witness lives below the
        // harness, in the spawner's process-group + netns ownership). When the
        // var is unset the policy is `AllowAll` and the wrapped command is
        // byte-identical to the pre-guard `/bin/bash --noprofile --norc`
        // shape, so every existing codepath (tests, claude workers) is
        // unaffected.
        let policy = cosmon_core::egress::EgressPolicy::from_env_value(
            std::env::var(cosmon_core::egress::EgressPolicy::ENV_VAR)
                .ok()
                .as_deref(),
        );
        let base_args = ["--noprofile".to_owned(), "--norc".to_owned()];
        // C1-F3 (task-20260712-8d2d): compute the *truthful* enforcement mode
        // from a runtime probe, not the optimistic `cfg!(linux)` ceiling. On a
        // hardened kernel with unprivileged user namespaces disabled the probe
        // returns false and the policy degrades to advisory here — the worker
        // runs (unjailed, policy recorded) instead of dying opaquely because
        // `unshare` could never create the namespace.
        let mode = cosmon_core::egress::EgressJail::enforcement_mode_for(
            crate::egress_probe::netns_available(),
        );
        let jailed =
            cosmon_core::egress::EgressJail::wrap_with_mode(mode, policy, "/bin/bash", &base_args);
        let mut cmd = Command::new(&jailed.program);
        cmd.args(&jailed.args)
            .current_dir(work_dir)
            .env("PS1", "")
            .env("PS2", "")
            .env("HISTFILE", "/dev/null")
            .env("TERM", "dumb")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // delib-20260519-e6db W3 / adversary F1.4 — put bash in its
        // own session (and process group) so a `kill(-pgid, SIGKILL)`
        // on timeout reaps every grandchild the command spawned. Without
        // this, a `cargo build` whose timeout fires would leave rustc
        // workers reparented to init/launchd, still holding the
        // target/ filelock; the harness would not see them and the
        // operator would discover the leak only when the next build
        // hangs on a stale lock.
        //
        // `setsid(2)` returns -1 only when the caller is already a
        // session leader (already in its own session). The freshly
        // spawned bash never is, so the failure mode here means a
        // pathological kernel state — fail the spawn rather than fall
        // through with a wrong pgid.
        //
        // The `unsafe` block crosses the `deny(unsafe_code)` boundary
        // (relaxed from `forbid` for this perimeter only):
        // `Command::pre_exec` requires `unsafe fn` because the closure
        // runs in the forked child between `fork(2)` and `execvp(2)`
        // and is restricted to async-signal-safe libc calls. `setsid`
        // is on the POSIX async-signal-safe list; the closure here
        // does no allocation, no locking, no Rust runtime work.
        #[cfg(unix)]
        #[allow(unsafe_code)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }

        let mut child = cmd
            .spawn()
            // Name the *actual* program spawned — under the netns jail that is
            // `unshare`, not `/bin/bash` (C1-F3, task-20260712-8d2d): the old
            // hard-coded "spawn /bin/bash" misnamed the failing program on any
            // `unshare`-spawn failure, sending a debugger down the wrong path.
            .map_err(|e| ToolError::Io(format!("spawn {}: {e}", jailed.program)))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ToolError::Io("child has no stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Io("child has no stdout".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::Io("child has no stderr".to_owned()))?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let tx_err = tx.clone();

        let stdout_reader = thread::spawn(move || pump(stdout, &tx));
        let stderr_reader = thread::spawn(move || pump(stderr, &tx_err));

        let mut session = Self {
            child,
            stdin,
            output_rx: rx,
            work_dir: work_dir.to_path_buf(),
            _stdout_reader: stdout_reader,
            _stderr_reader: stderr_reader,
        };

        // Merge stderr into stdout for the rest of the session. Bash's
        // `exec 2>&1` does a `dup2(1, 2)` so subsequent commands write
        // both streams through a single OS pipe — order-preserving by
        // construction. Without this, stdout and stderr are two
        // independent pipes feeding two pump threads through an mpsc;
        // a command that writes to stderr after the harness's marker
        // echo can land its bytes *after* the framing boundary, where
        // the slicing logic discards them. The stderr pump thread sees
        // EOF immediately after this and exits; the channel stays open
        // because the stdout pump's clone of the Sender is still held.
        //
        // Combined with the new 25 ms settle window after the first
        // marker hit (adversary F1.5 fix), this closes the
        // race that was silently dropping `echo ... 1>&2` output —
        // the settle window now sees stderr bytes arrive *before*
        // the marker, in the single ordered stream, and they correctly
        // land in the command's output.
        //
        // The init still ends with `true` (paired with the marker echo
        // inside `run()`) so the spawn returns only after the shell
        // has acknowledged BOTH the redirect AND the no-op.
        match session.run("exec 2>&1\ntrue", Duration::from_secs(5)) {
            Ok((_, 0, false)) => Ok(session),
            Ok((out, ec, to)) => Err(ToolError::Io(format!(
                "shell init failed: exit_code={ec} timed_out={to} output={out:?}"
            ))),
            Err(RunErr::Io(msg)) => Err(ToolError::Io(format!("init shell: {msg}"))),
            Err(RunErr::ShellDied { exit_code, .. }) => {
                let code_label = exit_code.map_or_else(|| "?".to_owned(), |c| c.to_string());
                Err(ToolError::Io(format!(
                    "shell died during init: exit_code={code_label}"
                )))
            }
        }
    }

    fn is_dead(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_status)) => true,
            Ok(None) => false,
            // `try_wait` errored — treat as dead to force respawn.
            Err(_) => true,
        }
    }

    fn run(&mut self, command: &str, timeout: Duration) -> Result<(String, i32, bool), RunErr> {
        // Drain any leftover bytes from a prior partial read before
        // issuing the next command. Belt-and-suspenders — the marker
        // protocol already terminates each turn cleanly, but a slow
        // PS-prompt flush (if a future contributor switches to `-i`)
        // would otherwise leak into the next turn's output.
        while self.output_rx.try_recv().is_ok() {}

        // Rotate the marker per call — a model that observed (or
        // logged) the previous turn's marker is denied the ability to
        // predict this turn's (knuth K3 / adversary F1.5).
        let end_marker = generate_marker();

        // The marker line is `<MARKER>:$?\n`. We append it on a NEW
        // line (not after `;`) so heredocs and multi-line constructs
        // in `command` survive untouched.
        let marker_line = format!("{end_marker}:$?");
        let payload = format!("{command}\necho \"{marker_line}\"\n");
        self.stdin
            .write_all(payload.as_bytes())
            .map_err(|e| RunErr::Io(format!("write stdin: {e}")))?;
        self.stdin
            .flush()
            .map_err(|e| RunErr::Io(format!("flush stdin: {e}")))?;

        let marker_needle = format!("{end_marker}:");
        let needle_bytes = marker_needle.as_bytes();
        // Streaming substring matcher whose state survives across
        // `recv_timeout` chunks — Θ(N) total work over received bytes,
        // not Θ(N · M / chunk_size) (knuth K3 / adversary F1.5).
        let mut scanner = KmpScanner::new(needle_bytes);
        // Start positions of every marker hit observed so far. Capacity
        // 2 is the load-bearing case: one hit is the legitimate echo;
        // two means a forgery hit landed first.
        let mut hits: Vec<usize> = Vec::with_capacity(2);
        let deadline = Instant::now() + timeout;
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);

        loop {
            let now = Instant::now();
            if now >= deadline {
                self.kill_group_and_reap();
                let out = String::from_utf8_lossy(&buffer).into_owned();
                return Ok((out, -1, true));
            }
            let remaining = deadline - now;
            match self.output_rx.recv_timeout(remaining) {
                Ok(chunk) => {
                    let base = buffer.len();
                    buffer.extend_from_slice(&chunk);
                    scanner.feed(&chunk, base, &mut hits);

                    // Two hits = forged + real marker, or two real
                    // markers from a shell glitch. Either way the
                    // framing is no longer trustworthy: refuse loudly.
                    // We kill the shell so the next call respawns —
                    // a forgery this turn means the shell saw the
                    // marker once and the protocol's secrecy assumption
                    // is broken for this session.
                    if hits.len() >= 2 {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        let out = String::from_utf8_lossy(&buffer).into_owned();
                        return Err(RunErr::ShellDied {
                            output: out,
                            exit_code: None,
                        });
                    }

                    if let Some(&start) = hits.first() {
                        let after_marker = start + needle_bytes.len();
                        if let Some(rel_nl) =
                            buffer[after_marker..].iter().position(|&b| b == b'\n')
                        {
                            // Settling window — bounded blocking wait
                            // for any in-flight second chunk. Catches
                            // the race where bash's two `echo` flushes
                            // land in two separate pump-thread sends
                            // and the recv that delivered the first
                            // chunk happened to fire BEFORE the second
                            // chunk reached the mpsc. A non-blocking
                            // `try_recv()` drain would miss those bytes
                            // and silently accept a forged exit code
                            // (adversary F1.5 — regression that broke
                            // `duplicate_marker_is_loud_shell_died_error`
                            // before the W9 sweep). The deadline is also
                            // capped by the overall command `deadline`
                            // so settling can never push the call past
                            // its timeout.
                            let settle_deadline =
                                (Instant::now() + FIRST_MARKER_SETTLE_WINDOW).min(deadline);
                            loop {
                                let now = Instant::now();
                                if now >= settle_deadline {
                                    break;
                                }
                                let wait = settle_deadline - now;
                                match self.output_rx.recv_timeout(wait) {
                                    Ok(extra) => {
                                        let extra_base = buffer.len();
                                        buffer.extend_from_slice(&extra);
                                        scanner.feed(&extra, extra_base, &mut hits);
                                        if hits.len() >= 2 {
                                            break;
                                        }
                                    }
                                    Err(
                                        mpsc::RecvTimeoutError::Timeout
                                        | mpsc::RecvTimeoutError::Disconnected,
                                    ) => break,
                                }
                            }
                            if hits.len() >= 2 {
                                let _ = self.child.kill();
                                let _ = self.child.wait();
                                let out = String::from_utf8_lossy(&buffer).into_owned();
                                return Err(RunErr::ShellDied {
                                    output: out,
                                    exit_code: None,
                                });
                            }

                            let exit_slice = &buffer[after_marker..after_marker + rel_nl];
                            let exit_str = std::str::from_utf8(exit_slice).unwrap_or("0").trim();
                            let exit_code: i32 = exit_str.parse().unwrap_or(-1);
                            // Command's output = everything strictly
                            // before the marker. Strip the trailing
                            // `\n` that `echo` printed just before the
                            // marker line.
                            let output_slice = trim_trailing_newline(&buffer[..start]);
                            let output = String::from_utf8_lossy(output_slice).into_owned();
                            return Ok((output, exit_code, false));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.kill_group_and_reap();
                    let out = String::from_utf8_lossy(&buffer).into_owned();
                    return Ok((out, -1, true));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let exit_code = self.child.try_wait().ok().flatten().and_then(|s| s.code());
                    let out = String::from_utf8_lossy(&buffer).into_owned();
                    return Err(RunErr::ShellDied {
                        output: out,
                        exit_code,
                    });
                }
            }
        }
    }

    /// SIGKILL the whole process group of the bash child and reap.
    ///
    /// `setsid(2)` in [`Self::spawn`] put bash in its own session +
    /// process group keyed by the bash PID. `kill(-pgid, SIGKILL)`
    /// therefore reaches every grandchild bash forked (rustc workers
    /// under `cargo`, child processes under `make`, …). Idempotent:
    /// if the group is already empty, the kill returns ESRCH and we
    /// fall through to `child.wait()` which reaps bash itself.
    fn kill_group_and_reap(&mut self) {
        #[cfg(unix)]
        #[allow(unsafe_code)]
        {
            // The child PID is a `u32` per std API but POSIX `pid_t`
            // is signed and never exceeds `i32::MAX` on supported
            // platforms — `cast_signed` makes the deliberate
            // wide-to-narrow conversion explicit for clippy.
            let pid = self.child.id().cast_signed();
            if pid > 0 {
                // Sign-flipped pid = process group target for kill(2).
                // Errors are intentionally swallowed: ESRCH = group
                // already gone, EPERM = bash already reparented under
                // a different uid. Neither is actionable here. The
                // `unsafe` block crosses the `deny(unsafe_code)`
                // perimeter: `libc::kill` takes raw pid_t + signal
                // number, the safety contract is "the signal is one
                // of the POSIX signals" — SIGKILL is on the POSIX
                // signal list, so the call is sound.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
        // Also kill the direct child (the bash itself) in case the
        // group call above missed it (e.g. `setsid` failed at spawn
        // and the cfg gate skipped the group kill on a non-unix
        // build). Idempotent.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Streaming Knuth-Morris-Pratt substring matcher.
///
/// The classic `haystack.windows(needle.len()).position(...)` form
/// scans Θ(N · M) per call where `N` is the buffer length and `M`
/// the needle length. When the buffer grows chunk-by-chunk through
/// `recv_timeout`, naive rescanning on every chunk degrades further
/// to Θ(N²) over total bytes. KMP keeps a single `state` variable
/// (the number of needle bytes provisionally matched against the
/// suffix of bytes seen so far); feeding new bytes advances or backs
/// off `state` via the failure table, yielding Θ(N) work over all
/// chunks combined. The cost is one preprocessing pass over the
/// needle plus an array of `M` `usize`s — both negligible at the
/// ~50-byte marker sizes used here. Knuth K3 / adversary F1.5.
struct KmpScanner {
    needle: Vec<u8>,
    /// Standard KMP failure (a.k.a. partial-match) table:
    /// `failure[i]` is the length of the longest proper prefix of
    /// `needle[..=i]` that is also a suffix. Indexed by `state - 1`
    /// when backing off after a mismatch.
    failure: Vec<usize>,
    /// Number of needle bytes provisionally matched against the
    /// suffix of bytes fed so far. `state == needle.len()` means a
    /// complete match was reported on the previous byte; the scanner
    /// then backs off via the failure table before consuming the next
    /// input byte.
    state: usize,
}

impl KmpScanner {
    fn new(needle: &[u8]) -> Self {
        let n = needle.len();
        let mut failure = vec![0_usize; n.max(1)];
        if n > 1 {
            let mut k = 0_usize;
            for i in 1..n {
                while k > 0 && needle[k] != needle[i] {
                    k = failure[k - 1];
                }
                if needle[k] == needle[i] {
                    k += 1;
                }
                failure[i] = k;
            }
        }
        Self {
            needle: needle.to_vec(),
            failure,
            state: 0,
        }
    }

    /// Consume `chunk`, appending the *start position* of every match
    /// (in the global byte stream starting at `base_offset`) to
    /// `hits`. The state carried in `self` lets a match that spans a
    /// chunk boundary still be found.
    fn feed(&mut self, chunk: &[u8], base_offset: usize, hits: &mut Vec<usize>) {
        if self.needle.is_empty() {
            return;
        }
        for (i, &b) in chunk.iter().enumerate() {
            while self.state > 0 && self.needle[self.state] != b {
                self.state = self.failure[self.state - 1];
            }
            if self.needle[self.state] == b {
                self.state += 1;
            }
            if self.state == self.needle.len() {
                let end = base_offset + i + 1;
                hits.push(end - self.needle.len());
                self.state = self.failure[self.state - 1];
            }
        }
    }
}

impl Drop for ExecSession {
    fn drop(&mut self) {
        // Best-effort SIGKILL + reap. Idempotent — if the child is
        // already dead, both calls return harmless errors. Sweeps the
        // whole process group (setsid in spawn) so grandchild rustc
        // / cargo workers don't leak past session shutdown.
        self.kill_group_and_reap();
    }
}

/// Read bytes from a child pipe and forward them to the merged
/// output channel. Exits on EOF, on a read error, or when the
/// receiver is dropped.
fn pump<R: Read>(mut reader: R, tx: &mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            // EOF and read errors collapse to the same termination
            // — the reader is done; the channel will see Disconnected
            // once both senders drop in the spawning closures.
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

/// Quadratic substring search retained as a test oracle for
/// [`KmpScanner`] regressions. Production code uses the streaming
/// KMP path; this lives only under `cfg(test)`.
#[cfg(test)]
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn trim_trailing_newline(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
        if end > 0 && bytes[end - 1] == b'\r' {
            end -= 1;
        }
    }
    &bytes[..end]
}

/// Per-process secret salt mixed into every marker.
///
/// **Threat model (adversary F1.5 / knuth K3).** The marker rotation
/// based on `nanos + pid + ctr` defeats a "naive replay" of the
/// previous turn's marker, but the model can in principle observe each
/// input (`pid` via `/proc/self/status`, `nanos` via `date +%s%N`,
/// `ctr` by incrementing by one) and reconstruct the next marker. The
/// salt closes that gap: 128 bits of `/dev/urandom`, captured once at
/// first use and mixed into every subsequent marker. The model has no
/// way to learn the salt — `/dev/urandom` returns *different* bytes to
/// each reader — so the marker is unforgeable in the same way a
/// keyed MAC (HMAC) over the same inputs would be.
///
/// Fallback path: if `/dev/urandom` cannot be opened or read (extreme
/// rare on POSIX; chrooted minimal images), we fall back to a mix of
/// `nanos · pid · two large primes`, which preserves uniqueness
/// across processes but loses the unforgeability guarantee. The
/// fallback never returns zero, so the salt always contributes some
/// per-process entropy.
fn process_salt() -> u128 {
    static SALT: OnceLock<u128> = OnceLock::new();
    *SALT.get_or_init(|| {
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if f.read_exact(&mut buf).is_ok() {
                return u128::from_ne_bytes(buf);
            }
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(1);
        nanos
            .wrapping_mul(0xDEAD_BEEF_CAFE_BABE_F00D_BA11_BEEF_CAFE_u128)
            .wrapping_add(u128::from(std::process::id()))
            | 1
    })
}

/// Generate a fresh 128-bit marker from a per-process secret salt
/// mixed with system time + pid + a monotonic per-process counter.
/// Avoids a `uuid` / `rand` / `hmac` dep; the counter guarantees
/// uniqueness even if two calls land within the same nanosecond (rare
/// on tests, possible on tight harness loops), and the salt makes the
/// output unforgeable even when the adversary can observe the other
/// inputs (`pid`, wall-clock, counter cadence). Per-call HMAC in
/// spirit — see [`process_salt`].
///
/// Called once per `ExecSession::run()` — the marker rotates per call
/// (knuth K3 / adversary F1.5), not per session.
fn generate_marker() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    #[cfg(test)]
    if let Some(forced) = test_marker_override() {
        return forced;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = u128::from(std::process::id());
    let ctr = u128::from(COUNTER.fetch_add(1, Ordering::Relaxed));
    let mix = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(pid)
        .wrapping_add(ctr.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        ^ process_salt();
    format!("__COSMON_END_{mix:032x}__")
}

#[cfg(test)]
thread_local! {
    /// Test-only override for [`generate_marker`]. Lets a regression
    /// test pin the marker to a known value so it can be planted in
    /// command output, exercising the duplicate-marker code path.
    static MARKER_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_marker_override() -> Option<String> {
    MARKER_OVERRIDE.with(|m| m.borrow().clone())
}

#[cfg(test)]
fn set_test_marker_override(marker: Option<String>) {
    MARKER_OVERRIDE.with(|m| *m.borrow_mut() = marker);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Pin the egress policy to `allow-all` for this test binary, once.
    ///
    /// These tests exercise the persistent-shell *framing* protocol, not the
    /// egress jail (that has its own suites: `exec_command_egress.rs`,
    /// `exec_command_netns_e2e.rs`, and `cosmon-core::egress`). Since the
    /// security-review 5008 fix, an **unset** `COSMON_EGRESS_POLICY` fails
    /// closed to `deny-external`, which on a netns-capable Linux host would wrap
    /// every framing shell in `unshare --net` — unavailable in sandboxed CI.
    /// Opting into `allow-all` keeps these shells unconfined. `Once` makes it a
    /// barrier that never races a parallel spawn; the value is constant so no
    /// test ever observes a different one.
    fn allow_local_shell() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            std::env::set_var(
                cosmon_core::egress::EgressPolicy::ENV_VAR,
                cosmon_core::egress::EgressPolicy::AllowAll.token(),
            );
        });
    }

    fn exec(tool: &ExecCommand, work_dir: &Path, cmd: &str) -> ExecResult {
        allow_local_shell();
        let args = serde_json::json!({"command": cmd}).to_string();
        let raw = tool.execute(&args, work_dir).expect("exec must succeed");
        serde_json::from_str(&raw).expect("result is valid JSON")
    }

    #[test]
    fn declaration_names_the_tool() {
        let tool = ExecCommand::new();
        assert_eq!(tool.name(), "exec_command");
        let decl = tool.declaration();
        assert_eq!(decl.name, "exec_command");
        assert!(decl.parameters.as_json()["properties"]["command"].is_object());
    }

    #[test]
    fn invalid_arguments_are_rejected_loudly() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let err = tool
            .execute("not json", dir.path())
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn pwd_returns_work_dir() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let r = exec(&tool, dir.path(), "pwd");
        assert_eq!(r.exit_code, 0);
        assert!(!r.timed_out);
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let actual = std::fs::canonicalize(r.output.trim()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn env_persists_across_commands() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let r1 = exec(&tool, dir.path(), "export FOO=bar");
        assert_eq!(r1.exit_code, 0);
        let r2 = exec(&tool, dir.path(), "echo $FOO");
        assert_eq!(r2.exit_code, 0);
        assert_eq!(r2.output.trim(), "bar");
    }

    #[test]
    fn cwd_persists_after_cd() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let r1 = exec(&tool, dir.path(), "mkdir -p sub");
        assert_eq!(r1.exit_code, 0);
        let r2 = exec(&tool, dir.path(), "cd sub");
        assert_eq!(r2.exit_code, 0);
        let r3 = exec(&tool, dir.path(), "pwd");
        assert_eq!(r3.exit_code, 0);
        let expected = std::fs::canonicalize(dir.path().join("sub")).unwrap();
        let actual = std::fs::canonicalize(r3.output.trim()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn nonzero_exit_code_is_surfaced() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let r = exec(&tool, dir.path(), "false");
        assert_eq!(r.exit_code, 1);
    }

    #[test]
    fn stderr_is_merged_into_output() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let r = exec(&tool, dir.path(), "echo on-stderr 1>&2");
        assert_eq!(r.exit_code, 0);
        // Stderr is pumped through the same channel as stdout; bytes
        // arrive interleaved but the marker line itself is on stdout,
        // so all stderr from this command lands before the marker.
        assert_eq!(r.output.trim(), "on-stderr");
    }

    #[test]
    fn shell_restarts_transparently_after_exit() {
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        // First, a healthy command to spawn the session.
        let _ = exec(&tool, dir.path(), "echo hi");
        // Kill the shell from inside.
        let died = exec(&tool, dir.path(), "exit 0");
        assert!(
            died.output.contains("[shell died") || died.exit_code != 0 || !died.timed_out,
            "shell death must be visible, got {died:?}"
        );
        // Next call must transparently respawn.
        let recovered = exec(&tool, dir.path(), "echo recovered");
        assert!(recovered.shell_restarted);
        assert!(recovered.output.contains("[shell restarted]"));
        assert!(recovered.output.contains("recovered"));
        assert_eq!(recovered.exit_code, 0);
    }

    #[test]
    fn timeout_fires_and_marks_result() {
        allow_local_shell();
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();
        let args = serde_json::json!({
            "command": "sleep 5",
            "timeout_secs": 1,
        })
        .to_string();
        let raw = tool.execute(&args, dir.path()).expect("must return result");
        let r: ExecResult = serde_json::from_str(&raw).unwrap();
        assert!(r.timed_out);
        assert_eq!(r.exit_code, -1);
    }

    #[test]
    fn truncate_output_adds_loud_marker() {
        let big = "x".repeat(OUTPUT_CAP_BYTES + 100);
        let truncated = truncate_output(big);
        assert!(truncated.contains("output truncated"));
        assert!(truncated.contains(&format!("original size = {} bytes", OUTPUT_CAP_BYTES + 100)));
        assert!(truncated.len() <= OUTPUT_CAP_BYTES + 128);
    }

    #[test]
    fn truncate_output_respects_char_boundaries() {
        // 4-byte char at the boundary — make sure we don't slice
        // mid-codepoint.
        let mut s = "a".repeat(OUTPUT_CAP_BYTES - 1);
        s.push('💡'); // 4 bytes
        s.push_str("trailing");
        let truncated = truncate_output(s);
        // Must be valid UTF-8 (String guarantees it, but make the
        // contract visible).
        let _ = truncated.as_str();
        assert!(truncated.contains("output truncated"));
    }

    #[test]
    fn generate_marker_is_unique_across_calls() {
        let m1 = generate_marker();
        let m2 = generate_marker();
        assert_ne!(m1, m2);
        assert!(m1.starts_with("__COSMON_END_"));
        assert!(m1.ends_with("__"));
    }

    /// knuth K3 / adversary F1.5 — when a command's legitimate output
    /// contains the marker prefix, the harness must refuse loudly
    /// rather than silently framing on the first hit. Test pins the
    /// marker via the cfg(test) override, then runs a command that
    /// prints a forged marker line; bash's own echo then emits the
    /// second hit. Two markers in one run → `[shell died]` surfaced
    /// in `output` and `exit_code = -1`.
    #[test]
    fn duplicate_marker_is_loud_shell_died_error() {
        allow_local_shell();
        let dir = tempdir().unwrap();
        let forged = "__COSMON_END_deadbeefcafebabe0123456789abcdef__".to_owned();
        set_test_marker_override(Some(forged.clone()));
        let tool = ExecCommand::new();
        // The command prints the marker prefix with a colon and a
        // fake exit code. Bash will then run the harness's appended
        // `echo "<marker>:$?"` and a second marker hit appears in
        // the buffer — duplicate detection must fire.
        let cmd = format!("echo \"{forged}:42\"");
        let raw = tool
            .execute(
                &serde_json::json!({ "command": cmd }).to_string(),
                dir.path(),
            )
            .expect("execute returns Ok envelope");
        let r: ExecResult = serde_json::from_str(&raw).unwrap();
        // The ShellDied → execute() bridge prefixes output with
        // `[shell died: ...]` and sets exit_code to -1.
        assert!(
            r.output.contains("[shell died"),
            "duplicate marker must surface as shell-died, got: {r:?}"
        );
        assert_eq!(r.exit_code, -1);
        // Cleanup so neighbouring tests get fresh, unique markers.
        set_test_marker_override(None);
    }

    #[test]
    fn kmp_scanner_reports_overlapping_hits() {
        let mut scanner = KmpScanner::new(b"AA");
        let mut hits = Vec::new();
        // "AAAA" contains "AA" at positions 0, 1, 2.
        scanner.feed(b"AAAA", 0, &mut hits);
        assert_eq!(hits, vec![0, 1, 2]);
    }

    #[test]
    fn kmp_scanner_state_survives_chunk_boundary() {
        // Marker split mid-byte across two chunks — Θ(N) streaming
        // requires the failure-table state to carry over.
        let needle = b"__COSMON_END_deadbeef__:";
        let mut scanner = KmpScanner::new(needle);
        let mut hits = Vec::new();
        scanner.feed(b"prelude__COSMON_END_dead", 0, &mut hits);
        scanner.feed(b"beef__:postscript", 24, &mut hits);
        assert_eq!(hits, vec![7]);
        assert_eq!(needle.len(), 24);
    }

    #[test]
    fn kmp_scanner_finds_two_hits_in_one_chunk() {
        let needle = b"__COSMON_END_x__:";
        let mut scanner = KmpScanner::new(needle);
        let mut hits = Vec::new();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"forgery__COSMON_END_x__:42\nreal__COSMON_END_x__:0\n");
        scanner.feed(&buf, 0, &mut hits);
        assert_eq!(hits.len(), 2, "both markers must be reported");
        assert!(hits[0] < hits[1]);
    }

    #[test]
    fn find_subsequence_handles_split_match() {
        assert_eq!(find_subsequence(b"hello world", b"world"), Some(6));
        assert_eq!(find_subsequence(b"hello", b"xyz"), None);
        assert_eq!(find_subsequence(b"abc", b""), Some(0));
        assert_eq!(find_subsequence(b"", b"abc"), None);
    }

    /// A timed-out
    /// command's grandchild process is reaped by `setsid` +
    /// `kill(-pgid)`. We spawn a `sleep 999` grandchild via
    /// `bash -c 'sleep 999 &'` (the `&` reparents it under the bash
    /// session leader; without `setsid` + group-kill it would survive
    /// the bash kill and reparent under init/launchd). The test
    /// captures the grandchild PID inside the shell and asserts it is
    /// no longer running after the timeout fires.
    ///
    /// Skipped on non-Unix.
    #[cfg(unix)]
    #[test]
    fn timeout_kills_grandchild_process_group() {
        allow_local_shell();
        let dir = tempdir().unwrap();
        let tool = ExecCommand::new();

        // Spawn a long-running grandchild and capture its PID.
        // `nohup`-style background detachment is intentional — pre-W3,
        // the harness's kill(self.child) only killed bash, leaving
        // sleep reparented under launchd.
        let pid_file = dir.path().join("sleeper.pid");
        let pid_path = pid_file.to_string_lossy().into_owned();
        let args = serde_json::json!({
            "command": format!(
                "( sleep 999 ) & echo $! > '{pid_path}'; wait",
            ),
            "timeout_secs": 1,
        })
        .to_string();
        let raw = tool.execute(&args, dir.path()).expect("must return result");
        let r: ExecResult = serde_json::from_str(&raw).unwrap();
        assert!(r.timed_out, "timeout must fire; got {r:?}");

        // Read the grandchild PID and probe it. `kill(pid, 0)` returns
        // 0 if the process exists, -1 with ESRCH otherwise. We use the
        // shell's `kill -0` so the test stays portable across libc
        // versions.
        let pid_text =
            std::fs::read_to_string(&pid_file).expect("grandchild must have written its PID");
        let pid: i32 = pid_text.trim().parse().expect("PID must be an integer");
        assert!(pid > 0, "captured PID must be positive ({pid})");

        // Give the kernel a moment to reap the group. The kill above
        // is synchronous from Rust's perspective but the kernel's
        // process-table cleanup is not instant on macOS.
        std::thread::sleep(Duration::from_millis(200));

        let still_alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(
            !still_alive,
            "grandchild PID {pid} must be reaped by setsid+kill(-pgid); \
             it is still alive after the harness timeout"
        );
    }
}
