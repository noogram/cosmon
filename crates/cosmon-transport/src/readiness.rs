// SPDX-License-Identifier: AGPL-3.0-only

//! Session readiness detection — poll a worker to determine if it is alive.
//!
//! The revival fluid for petrified agents.
//!
//! # Two layers (de-Claude-ification)
//!
//! This module was originally written assuming the worker is a Claude Code
//! TUI scrolling through a tmux pane. That assumption is real but *local* —
//! it belongs to one Adapter, not to the readiness concept. The module now
//! separates the two:
//!
//! 1. **Claude-TUI-specific layer** — [`SessionStatus`], [`classify_output`],
//!    the `markers` string table, [`detect_status`] and [`wait_ready`].
//!    These parse Claude Code's terminal output (`Loading` / trust prompt /
//!    `❯` ready chevron / `⏺` tool-use / permission prompt) and auto-answer
//!    the TUI's blocking dialogs. They assume TUI-typical *seconds*-scale
//!    timeouts and a scrollback to grep. **Nothing here is wrong** — it is
//!    simply Claude's pane signature, and it stays intact.
//! 2. **Substrate-agnostic layer** — the [`Liveness`] verdict, the
//!    [`LiveProbe`] contract, and the [`poll_until_live`] driver. This is
//!    the part a future Adapter without a Claude TUI (a Codex pane, a
//!    headless API ack, a `llama.cpp` FFI loop) can satisfy *without
//!    pretending to be a TUI*. The contract answers exactly one question —
//!    *"is the worker alive and accepting work?"* — and converts a *"no"* or
//!    *"timeout"* into the same propagated failure the Claude path produces
//!    today.
//!
//! [`ClaudeTuiProbe`] is the bridge: a zero-sized [`LiveProbe`] implementor
//! that delegates to the Claude-TUI layer. `cs tackle`'s spawn postcondition
//! and readiness wait both go through the [`LiveProbe`] contract, so the
//! surface-lie regression from task-4046 (tmux spawned, `claude` exec failed
//! silently, the operator saw a green light over a dead worker) is now
//! guarded at the *contract* level — see [`LiveProbe::observe`] and the
//! `probe_refuses_dead_worker` test.
//!
//! Replaces the fragile `thread::sleep(3s)` pattern with evidence-based
//! readiness detection.

use std::time::{Duration, Instant};

use cosmon_core::id::WorkerId;
use cosmon_core::transport::{TransportBackend, TransportError};

/// Send just an Enter keypress to a session (no preceding text).
///
/// Used to confirm TUI prompts like the workspace trust dialog where
/// the correct option is already highlighted.
fn send_enter(backend: &dyn TransportBackend, worker_id: &WorkerId) -> Result<(), TransportError> {
    // send_input sends [text, Enter]. Empty text + Enter = just Enter.
    backend.send_input(worker_id, "")
}

/// Observed state of a Claude Code session based on its terminal output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// Claude is showing the "trust this folder" prompt — needs "1" + Enter.
    TrustPrompt,
    /// Claude is loading / initializing (spinner, "Loading..." etc.).
    Loading,
    /// Claude is ready for input (shows the `❯` prompt or "Type your message").
    Ready,
    /// Claude is actively working (tool calls, thinking, output streaming).
    Working,
    /// Claude is blocked waiting for user input (tool permission, confirmation).
    Blocked,
    /// The session is alive but the output does not match any known pattern.
    Unknown,
    /// The session is not alive (tmux session doesn't exist).
    Dead,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrustPrompt => f.write_str("trust-prompt"),
            Self::Loading => f.write_str("loading"),
            Self::Ready => f.write_str("idle"),
            Self::Working => f.write_str("working"),
            Self::Blocked => f.write_str("blocked"),
            Self::Unknown => f.write_str("unknown"),
            Self::Dead => f.write_str("dead"),
        }
    }
}

impl SessionStatus {
    /// Collapse the rich Claude-TUI verdict onto the substrate-agnostic
    /// [`Liveness`] axis.
    ///
    /// The five states that prove the process *printed something a live
    /// claude would print* — `Loading`, `TrustPrompt`, `Ready`, `Working`,
    /// `Blocked` — all map to [`Liveness::Live`]. `Dead` maps to
    /// [`Liveness::Dead`]. `Unknown` (alive-but-unrecognised, or nothing
    /// rendered yet) maps to [`Liveness::Indeterminate`].
    ///
    /// This is the load-bearing translation between Claude's pane signature
    /// and the contract a TUI-less Adapter answers: the caller never has to
    /// know which TUI string was matched, only whether the worker is alive.
    #[must_use]
    pub fn liveness(&self) -> Liveness {
        match self {
            Self::TrustPrompt | Self::Loading | Self::Ready | Self::Working | Self::Blocked => {
                Liveness::Live
            }
            Self::Dead => Liveness::Dead,
            Self::Unknown => Liveness::Indeterminate,
        }
    }
}

/// Markers used to detect session state from captured terminal output.
mod markers {
    /// The trust prompt — Claude shows this in new/untrusted directories.
    pub const TRUST_PROMPT: &str = "Yes, I trust this folder";
    /// Alternative trust prompt marker.
    pub const TRUST_PROMPT_ALT: &str = "Quick safety check";
    /// Claude is ready for input.
    pub const READY_PROMPT: &str = "❯";
    /// Alternative ready indicator.
    pub const READY_TYPE: &str = "Type your message";
    /// Claude is initializing.
    pub const LOADING: &str = "Loading";
    /// Claude is actively using tools.
    pub const TOOL_USE: &str = "⏺";
    /// Claude is thinking.
    pub const THINKING: &str = "Thinking";
    /// Claude is blocked waiting for tool use permission.
    pub const TOOL_PERMISSION: &str = "Do you want to proceed?";
    /// Alternative blocked indicator — tool use header.
    /// Blocked on a yes/no question.
    pub const YES_NO_PROMPT: &str = "Esc to cancel";
    /// Claude Code v2+ first-run theme wizard.
    ///
    /// Shown the first time `claude` runs in a fresh environment (no
    /// `~/.claude/config.json` settings yet). The wizard contains the menu
    /// chevron `❯`, so it must be classified BEFORE the generic
    /// last-5-lines `❯` scan that would otherwise mis-classify it as Ready.
    pub const FIRST_RUN_THEME: &str = "Choose the text style";
    /// Companion marker for the same first-run wizard banner.
    pub const FIRST_RUN_WELCOME: &str = "Let's get started";
}

/// Inspect a session's terminal output and classify its state.
///
/// Reads the last 30 lines of the session's terminal and matches against
/// known patterns.
///
/// # Errors
///
/// Returns [`TransportError`] if the session cannot be queried.
pub fn detect_status(
    backend: &dyn TransportBackend,
    worker_id: &WorkerId,
) -> Result<SessionStatus, TransportError> {
    if !backend.is_alive(worker_id)? {
        return Ok(SessionStatus::Dead);
    }

    let output = backend.capture_output(worker_id, 30)?;

    Ok(classify_output(&output))
}

/// Classify raw terminal output into a session status.
///
/// Pure function — no I/O. Examines the last lines of output to determine
/// which state the Claude session is in.
#[must_use]
pub fn classify_output(output: &str) -> SessionStatus {
    // Check from most specific to least specific.
    // Trust prompt is the most urgent — it blocks everything.
    if output.contains(markers::TRUST_PROMPT) || output.contains(markers::TRUST_PROMPT_ALT) {
        return SessionStatus::TrustPrompt;
    }

    // Check for blocked state — Claude is waiting for permission/confirmation.
    if output.contains(markers::TOOL_PERMISSION) || output.contains(markers::YES_NO_PROMPT) {
        return SessionStatus::Blocked;
    }

    // First-run wizard (Claude Code v2.1.140+) must be detected BEFORE the
    // generic last-5-lines `❯` scan, because the wizard's menu chevron would
    // otherwise be mis-classified as Ready and tackle's 2 s spawn
    // postcondition would never see live-claude output.
    if output.contains(markers::FIRST_RUN_THEME) || output.contains(markers::FIRST_RUN_WELCOME) {
        return SessionStatus::Loading;
    }

    // Check ready prompt FIRST in the last few lines — if the prompt ❯ is
    // at the bottom, Claude is idle regardless of past ⏺ markers in scrollback.
    // This fixes false "working" detection from old tool-use output.
    let last_lines: Vec<&str> = output
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(5)
        .collect();

    for line in &last_lines {
        if line.contains(markers::READY_PROMPT) || line.contains(markers::READY_TYPE) {
            return SessionStatus::Ready;
        }
    }

    // Only check for work indicators if we didn't find a ready prompt.
    // This means Claude is mid-output (no ❯ yet).
    if output.contains(markers::TOOL_USE) || output.contains(markers::THINKING) {
        return SessionStatus::Working;
    }

    // Check for loading state.
    if output.contains(markers::LOADING) {
        return SessionStatus::Loading;
    }

    SessionStatus::Unknown
}

/// Wait for a session to reach `Ready` state, handling blocking prompts.
///
/// Polls the session every `poll_interval` until it is `Ready` or the
/// `timeout` expires. If a `TrustPrompt` is detected, automatically
/// sends "1" + Enter to accept it and continues polling.
///
/// Returns the final [`SessionStatus`] when ready or when timeout expires.
///
/// # Errors
///
/// Returns [`TransportError`] if the session dies or cannot be queried.
pub fn wait_ready(
    backend: &dyn TransportBackend,
    worker_id: &WorkerId,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<SessionStatus, TransportError> {
    let start = Instant::now();
    let mut trust_handled = false;

    while start.elapsed() < timeout {
        let status = detect_status(backend, worker_id)?;

        match status {
            SessionStatus::Ready => return Ok(SessionStatus::Ready),
            SessionStatus::Working => return Ok(SessionStatus::Working),
            SessionStatus::Dead => return Err(TransportError::NotFound(worker_id.clone())),
            SessionStatus::TrustPrompt => {
                if !trust_handled {
                    // The trust prompt is a TUI selection menu where option 1
                    // ("Yes, I trust this folder") is already highlighted.
                    // Send Enter to confirm the selection, then a second Enter
                    // after a brief pause to dismiss any follow-up prompt.
                    send_enter(backend, worker_id)?;
                    trust_handled = true;
                }
                // Continue polling — Claude will transition to Loading then Ready.
            }
            SessionStatus::Blocked => {
                // Session is blocked on a permission prompt.
                // Auto-accept by sending Enter (selects the default option).
                send_enter(backend, worker_id)?;
                // Continue polling — Claude will proceed after acceptance.
            }
            SessionStatus::Loading | SessionStatus::Unknown => {
                // Still booting or unrecognized — keep waiting.
            }
        }

        std::thread::sleep(poll_interval);
    }

    // Timeout — return whatever state we last observed.
    detect_status(backend, worker_id)
}

// ===========================================================================
// Substrate-agnostic liveness layer (task-20260426-d781)
// ===========================================================================

/// Substrate-agnostic verdict: is a freshly-spawned worker alive?
///
/// This is the projection of every Adapter's readiness onto a single axis,
/// so the spawn boundary in `cs tackle` never has to know whether it spawned
/// a Claude TUI, a Codex pane, or a headless API worker. The Claude-TUI
/// verdict [`SessionStatus`] collapses onto this via [`SessionStatus::liveness`].
///
/// The variants are deliberately three, not two: `Indeterminate` preserves
/// the distinction the task-4046 fix relied on — *"the process is gone"*
/// (`Dead`) is a different operator story from *"the process is there but
/// never printed anything we recognise"* (`Indeterminate`), and the two
/// produce different diagnostics at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The worker produced positive evidence that it started and is
    /// accepting work.
    Live,
    /// The worker is gone — the underlying session/process does not exist.
    Dead,
    /// Within the window, no positive evidence of liveness appeared. The
    /// worker may have failed to start, or it may be alive but emitting
    /// nothing the probe recognises. Treated as a failed spawn by callers.
    Indeterminate,
}

impl std::fmt::Display for Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live => f.write_str("live"),
            Self::Dead => f.write_str("dead"),
            Self::Indeterminate => f.write_str("indeterminate"),
        }
    }
}

/// The post-spawn liveness contract every Adapter satisfies.
///
/// This is the substrate-agnostic replacement for "call [`wait_ready`] and
/// hope it parsed a Claude TUI". An Adapter implements [`Self::observe`]
/// (one side-effect-free reading) and, if its startup involves blocking
/// prompts it must *answer* (the Claude TUI trust/permission dialogs),
/// overrides [`Self::await_live`]. Adapters whose startup needs no
/// hand-holding — a headless API ack, a pane that just needs to print —
/// inherit the default [`Self::await_live`], which simply polls
/// [`Self::observe`] until the worker is [`Liveness::Live`] or the timeout
/// expires.
///
/// # The anti-surface-lie contract (task-4046)
///
/// **No implementor may return [`Liveness::Live`] when the underlying
/// worker did not start.** [`Self::observe`] must report `Live` only on
/// *positive* evidence — a pane signature matched, a token advanced, an API
/// handshake completed. A probe that returns `Ok(Liveness::Live)` from the
/// absence of an error reproduces the task-4046 surface lie in a new
/// Adapter. The [`poll_until_live`] driver and the call sites in `cs tackle`
/// rely on this: they convert anything that is not `Live` into a torn-down
/// spawn with a truthful diagnostic.
///
/// The reusable contract check `assert_probe_refuses_dead_worker` (under
/// the `test-support` feature) lets every implementor's test suite assert
/// this property against a worker that never started.
pub trait LiveProbe {
    /// Take one side-effect-free reading of the worker's liveness *right
    /// now*. Must never perturb the worker (no keystrokes, no input).
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] if the worker cannot be queried at all.
    /// A queryable-but-absent worker is **not** an error — it is
    /// `Ok(`[`Liveness::Dead`]`)`.
    fn observe(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
    ) -> Result<Liveness, TransportError>;

    /// Block until the worker is alive and accepting work, or the timeout
    /// expires. Implementors whose startup involves prompts they must
    /// answer override this; the default polls [`Self::observe`] without
    /// perturbing the worker.
    ///
    /// Returns the final [`Liveness`] verdict. A non-[`Liveness::Live`]
    /// result is the signal for the caller to tear down the partial spawn.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] if the worker cannot be queried.
    fn await_live(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Liveness, TransportError> {
        poll_until_live(self, backend, worker_id, timeout, poll_interval)
    }
}

/// Poll a [`LiveProbe`] without perturbing the worker until it is
/// [`Liveness::Live`] or the `window` elapses.
///
/// This is the substrate-agnostic generalisation of `cs tackle`'s
/// `observe_spawn_postcondition` loop: it demands *evidence* of liveness
/// before returning `Live` and otherwise reports the last verdict seen on
/// timeout. Transient [`Self::observe`](LiveProbe::observe) errors are
/// swallowed and the poll continues — mirroring the pre-refactor
/// `.unwrap_or(...)` behaviour, where a momentary query failure must not be
/// mistaken for a dead worker.
///
/// The default [`LiveProbe::await_live`] delegates here.
///
/// # Errors
///
/// Never returns `Err` itself — transient [`LiveProbe::observe`] errors are
/// swallowed and the poll continues. The `Result` is kept so callers thread
/// the same error type as [`LiveProbe::await_live`] without a special case.
pub fn poll_until_live<P: LiveProbe + ?Sized>(
    probe: &P,
    backend: &dyn TransportBackend,
    worker_id: &WorkerId,
    window: Duration,
    poll_interval: Duration,
) -> Result<Liveness, TransportError> {
    let deadline = Instant::now() + window;
    let mut last = Liveness::Indeterminate;
    loop {
        match probe.observe(backend, worker_id) {
            Ok(Liveness::Live) => return Ok(Liveness::Live),
            Ok(other) => last = other,
            // A transient query failure is not evidence of death — keep
            // polling within the window (pre-refactor `.unwrap_or` shape).
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Ok(last);
        }
        std::thread::sleep(poll_interval);
    }
}

/// The Claude Code TUI [`LiveProbe`] — the historical readiness path, now
/// named as one Adapter's implementation of the substrate-agnostic contract.
///
/// Zero-sized: it carries no state, it simply routes [`LiveProbe::observe`]
/// through [`detect_status`] (Claude pane parse) and overrides
/// [`LiveProbe::await_live`] to use [`wait_ready`], which *answers* Claude's
/// trust and permission prompts as it polls. Behaviour for Claude workers is
/// byte-identical to the pre-refactor direct calls — the boundary moved, not
/// the logic.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeTuiProbe;

impl LiveProbe for ClaudeTuiProbe {
    fn observe(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
    ) -> Result<Liveness, TransportError> {
        Ok(detect_status(backend, worker_id)?.liveness())
    }

    fn await_live(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Liveness, TransportError> {
        // `wait_ready` carries the Claude-TUI-specific handshake (it sends
        // Enter to dismiss the trust dialog and auto-accept permission
        // prompts). Mapping its rich verdict onto `Liveness` is the whole
        // job of this override.
        Ok(wait_ready(backend, worker_id, timeout, poll_interval)?.liveness())
    }
}

// ===========================================================================
// Aider REPL liveness layer (task-20260607-3345 / B5)
// ===========================================================================

/// Markers that prove an Aider process printed something only a *live*
/// Aider would print.
///
/// Aider is not a Claude-style TUI — it is a Python REPL that opens with a
/// fixed banner (version line, model announcement, git-repo summary) and,
/// in interactive mode, settles on a `>` input prompt. Any of these is
/// positive evidence the `aider` binary actually exec'd and reached its own
/// startup output, as opposed to a tmux session whose pane immediately
/// `[exited]` because the binary was missing or crashed (the task-4046
/// surface lie, now guarded for the aider adapter too).
mod aider_markers {
    /// The Aider startup version banner — `Aider v0.x.y`. The single most
    /// reliable proof the binary launched.
    pub const BANNER: &str = "Aider v";
    /// The model announcement line printed right after the banner.
    pub const MAIN_MODEL: &str = "Main model:";
    /// The git-repo summary line printed at startup.
    pub const GIT_REPO: &str = "Git repo:";
    /// The first-run help hint printed at the end of the banner.
    pub const HELP_HINT: &str = "Use /help";
    /// The interactive REPL input prompt Aider settles on when it is
    /// waiting for the operator's next message.
    pub const REPL_PROMPT: &str = ">";
}

/// `true` when raw terminal `output` carries positive evidence that a live
/// Aider process printed it.
///
/// Pure function — no I/O. Mirrors [`classify_output`] for the Claude TUI,
/// but collapses straight onto the boolean "is this live aider output?"
/// rather than a rich status enum: the aider spawn path needs only the
/// substrate-agnostic [`Liveness`] verdict, not aider's full REPL state.
///
/// Evidence is either a banner marker (version / model / git-repo / help
/// hint) **anywhere** in the captured scrollback, or a trailing `>` REPL
/// prompt on the last non-empty line. The banner check is what makes a
/// fast `aider --message …` run that already printed its banner and exited
/// still read as `Live` — the proof-of-launch survives in the pane
/// scrollback even after the process is gone.
#[must_use]
pub fn aider_output_is_live(output: &str) -> bool {
    if output.contains(aider_markers::BANNER)
        || output.contains(aider_markers::MAIN_MODEL)
        || output.contains(aider_markers::GIT_REPO)
        || output.contains(aider_markers::HELP_HINT)
    {
        return true;
    }

    // The interactive REPL prompt is a bare `>` (optionally followed by the
    // operator's in-progress input) at the start of the last non-empty
    // line. Restricting to the last line avoids matching a `>` that appears
    // inside diff output or quoted text earlier in the scrollback.
    output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim_start)
        .is_some_and(|last| last == aider_markers::REPL_PROMPT || last.starts_with("> "))
}

/// The Aider REPL [`LiveProbe`] — the aider adapter's implementation of the
/// substrate-agnostic readiness contract.
///
/// Zero-sized, like [`ClaudeTuiProbe`]. It answers the one question every
/// probe answers — *"is the worker alive and accepting work?"* — by
/// capturing the pane scrollback and asking [`aider_output_is_live`].
/// Aider needs no startup-prompt handshake (its `--yes-always` flag
/// auto-confirms), so it inherits the default [`LiveProbe::await_live`],
/// which simply polls [`Self::observe`] via [`poll_until_live`].
///
/// # The anti-surface-lie contract (task-4046 → B5)
///
/// `observe` reports [`Liveness::Live`] **only** on positive aider output
/// evidence, never from the mere existence of the tmux session. This is the
/// load-bearing difference from the bespoke `2s` / `is_alive` loop B5
/// deleted: `is_alive` answered "does the session exist?", which is `true`
/// even for an `[exited]` carcass pane. A session that exists but never
/// printed aider's banner is [`Liveness::Indeterminate`]; a session that is
/// gone is [`Liveness::Dead`]. Output evidence is checked *before*
/// liveness, so a fast `--message` run that printed its banner and already
/// exited still reads `Live`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AiderProbe;

impl LiveProbe for AiderProbe {
    fn observe(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
    ) -> Result<Liveness, TransportError> {
        // Positive evidence first: did aider print something only a live
        // aider prints? A queryable-but-absent worker yields a transport
        // error here, which we treat as "no evidence" and fall through to
        // the liveness check below.
        if let Ok(output) = backend.capture_output(worker_id, 40) {
            if aider_output_is_live(&output) {
                return Ok(Liveness::Live);
            }
        }

        // No banner / prompt yet — distinguish a dead session (gone) from
        // one that is alive but still booting (no recognised output yet).
        if backend.is_alive(worker_id)? {
            Ok(Liveness::Indeterminate)
        } else {
            Ok(Liveness::Dead)
        }
    }
}

/// Output markers proving a live codex process printed to its pane.
///
/// Both codex launch modes are covered by the same marker set:
/// - **`codex exec`** (batch) prints a fixed startup preamble — the
///   `OpenAI Codex` banner plus a `model:` / `workdir:` summary block —
///   then streams its work, never settling on a `>` input prompt.
/// - **interactive** (`codex` with `--no-alt-screen`, the default since
///   task-20260711-246d) renders its TUI banner inline into the pane
///   scrollback, which also names `OpenAI Codex` (and the `codex` version
///   line). The `TOOL` / `BANNER` markers therefore fire for the
///   interactive banner too, so no separate interactive probe is needed —
///   `--no-alt-screen` is precisely what keeps that banner in the captured
///   scrollback rather than a hidden alternate screen.
///
/// The aider markers do not fire for codex, which is why codex carries its
/// own probe.
///
/// The marker set is deliberately broad and case-insensitive (see
/// [`codex_output_is_live`]) — any one of these lines is proof the `codex`
/// binary exec'd and reached its own output, as opposed to an `[exited]`
/// carcass pane (the task-4046 surface lie). The set is best-effort across
/// codex CLI versions; if a future release renames the preamble, widen it
/// here rather than loosening the probe to accept a bare live session.
mod codex_markers {
    /// The codex startup banner. The single most reliable proof the binary
    /// launched into `exec` mode.
    pub const BANNER: &str = "openai codex";
    /// Bare tool name — appears in the banner and most diagnostics.
    pub const TOOL: &str = "codex";
    /// The model announcement line in the `exec` preamble.
    pub const MODEL: &str = "model:";
    /// The working-directory line in the `exec` preamble.
    pub const WORKDIR: &str = "workdir:";
    /// The user-instructions section header `exec` prints before working.
    pub const USER_INSTRUCTIONS: &str = "user instructions";
}

/// `true` when raw terminal `output` carries positive evidence that a live
/// `codex exec` process printed it.
///
/// Pure function — no I/O. The codex counterpart of [`aider_output_is_live`].
/// Matching is **case-insensitive** because codex's preamble casing has
/// drifted across releases; a single `codex_markers` hit anywhere in the
/// captured scrollback is enough. As with aider, the evidence survives in the
/// pane scrollback even after a fast `codex exec` run has already exited, so a
/// completed run still reads as `Live`.
#[must_use]
pub fn codex_output_is_live(output: &str) -> bool {
    let haystack = output.to_ascii_lowercase();
    haystack.contains(codex_markers::BANNER)
        || haystack.contains(codex_markers::TOOL)
        || haystack.contains(codex_markers::MODEL)
        || haystack.contains(codex_markers::WORKDIR)
        || haystack.contains(codex_markers::USER_INSTRUCTIONS)
}

/// The `codex exec` [`LiveProbe`] — codex's implementation of the
/// substrate-agnostic readiness contract.
///
/// Zero-sized, like [`ClaudeTuiProbe`] and [`AiderProbe`]. It answers the one
/// question every probe answers — *"is the worker alive and accepting
/// work?"* — by capturing the pane scrollback and asking
/// [`codex_output_is_live`]. `codex exec` is non-interactive and needs no
/// startup-prompt handshake, so it inherits the default
/// [`LiveProbe::await_live`], which polls [`Self::observe`] via
/// [`poll_until_live`].
///
/// # The anti-surface-lie contract (task-4046 → B5)
///
/// `observe` reports [`Liveness::Live`] **only** on positive codex output
/// evidence, never from the mere existence of the tmux session — identical to
/// [`AiderProbe`]. A session that exists but never printed codex's banner is
/// [`Liveness::Indeterminate`]; a session that is gone is [`Liveness::Dead`].
/// Output evidence is checked *before* liveness, so a fast `codex exec` run
/// that printed its preamble and already exited still reads `Live`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodexProbe;

impl LiveProbe for CodexProbe {
    fn observe(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
    ) -> Result<Liveness, TransportError> {
        // Positive evidence first: did codex print something only a live
        // codex prints? A queryable-but-absent worker yields a transport
        // error here, treated as "no evidence" — fall through to liveness.
        if let Ok(output) = backend.capture_output(worker_id, 40) {
            if codex_output_is_live(&output) {
                return Ok(Liveness::Live);
            }
        }

        // No preamble yet — distinguish a dead session (gone) from one that
        // is alive but still booting (no recognised output yet).
        if backend.is_alive(worker_id)? {
            Ok(Liveness::Indeterminate)
        } else {
            Ok(Liveness::Dead)
        }
    }
}

/// Output markers proving a live `opencode run` process printed to its pane.
///
/// `opencode` (sst/opencode) is, like `codex`, an external coding-agent CLI
/// driven in its non-interactive automation mode — here `opencode run
/// '<prompt>'` (the counterpart of `codex exec`). It prints a startup
/// preamble naming itself and a session/share line before streaming work,
/// and never settles on a `>` REPL prompt, so neither the aider nor the
/// codex marker sets fire for it — hence opencode carries its own probe.
///
/// The marker set is deliberately broad and matched case-insensitively (see
/// [`opencode_output_is_live`]): any one of these lines is proof the
/// `opencode` binary exec'd and reached its own output, as opposed to an
/// `[exited]` carcass pane (the task-4046 surface lie). The set is
/// best-effort across opencode CLI versions; if a future release renames the
/// preamble, widen it here rather than loosening the probe to accept a bare
/// live session.
mod opencode_markers {
    /// The opencode banner / tool name. The single most reliable proof the
    /// binary launched — it appears in the startup preamble, the version
    /// string, and most diagnostics.
    pub const BANNER: &str = "opencode";
    /// The working-directory line opencode prints in its run preamble.
    pub const WORKDIR: &str = "workdir:";
    /// The model announcement line in the run preamble.
    pub const MODEL: &str = "model:";
    /// The session/share line opencode prints when it starts a run.
    pub const SHARE: &str = "share:";
}

/// `true` when raw terminal `output` carries positive evidence that a live
/// `opencode run` process printed it.
///
/// Pure function — no I/O. The opencode counterpart of
/// [`codex_output_is_live`]. Matching is **case-insensitive** because
/// opencode's preamble casing has drifted across releases; a single
/// `opencode_markers` hit anywhere in the captured scrollback is enough.
/// As with codex, the evidence survives in the pane scrollback even after a
/// fast `opencode run` has already exited, so a completed run still reads as
/// `Live`.
#[must_use]
pub fn opencode_output_is_live(output: &str) -> bool {
    let haystack = output.to_ascii_lowercase();
    haystack.contains(opencode_markers::BANNER)
        || haystack.contains(opencode_markers::WORKDIR)
        || haystack.contains(opencode_markers::MODEL)
        || haystack.contains(opencode_markers::SHARE)
}

/// The `opencode run` [`LiveProbe`] — opencode's implementation of the
/// substrate-agnostic readiness contract.
///
/// Zero-sized, like [`ClaudeTuiProbe`], [`AiderProbe`] and [`CodexProbe`]. It
/// answers the one question every probe answers — *"is the worker alive and
/// accepting work?"* — by capturing the pane scrollback and asking
/// [`opencode_output_is_live`]. `opencode run` is non-interactive and needs
/// no startup-prompt handshake, so it inherits the default
/// [`LiveProbe::await_live`], which polls [`Self::observe`] via
/// [`poll_until_live`].
///
/// # The anti-surface-lie contract (task-4046 → B5)
///
/// `observe` reports [`Liveness::Live`] **only** on positive opencode output
/// evidence, never from the mere existence of the tmux session — identical to
/// [`CodexProbe`]. A session that exists but never printed opencode's banner
/// is [`Liveness::Indeterminate`]; a session that is gone is
/// [`Liveness::Dead`]. Output evidence is checked *before* liveness, so a
/// fast `opencode run` that printed its preamble and already exited still
/// reads `Live`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpencodeProbe;

impl LiveProbe for OpencodeProbe {
    fn observe(
        &self,
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
    ) -> Result<Liveness, TransportError> {
        // Positive evidence first: did opencode print something only a live
        // opencode prints? A queryable-but-absent worker yields a transport
        // error here, treated as "no evidence" — fall through to liveness.
        if let Ok(output) = backend.capture_output(worker_id, 40) {
            if opencode_output_is_live(&output) {
                return Ok(Liveness::Live);
            }
        }

        // No preamble yet — distinguish a dead session (gone) from one that
        // is alive but still booting (no recognised output yet).
        if backend.is_alive(worker_id)? {
            Ok(Liveness::Indeterminate)
        } else {
            Ok(Liveness::Dead)
        }
    }
}

/// Reusable contract check: a [`LiveProbe`] pointed at a worker that never
/// started MUST NOT report [`Liveness::Live`].
///
/// This is the generalised task-4046 surface-lie regression — any future
/// Adapter's test suite can call it against a ghost (never-spawned) worker
/// to prove its probe refuses to lie. The Claude path exercises it in
/// `probe_refuses_dead_worker`.
///
/// # Panics
///
/// Panics if `observe` or `await_live` reports `Live` for `ghost`, or if
/// `observe` returns a transport error for a queryable-but-absent worker.
#[cfg(any(test, feature = "test-support"))]
pub fn assert_probe_refuses_dead_worker<P: LiveProbe>(
    probe: &P,
    backend: &dyn TransportBackend,
    ghost: &WorkerId,
) {
    let observed = probe
        .observe(backend, ghost)
        .expect("a queryable-but-absent worker is Dead, not a transport error");
    assert_ne!(
        observed,
        Liveness::Live,
        "LiveProbe::observe reported Live for a worker that never started — surface lie"
    );
    let awaited = probe.await_live(
        backend,
        ghost,
        Duration::from_millis(100),
        Duration::from_millis(20),
    );
    assert!(
        !matches!(awaited, Ok(Liveness::Live)),
        "LiveProbe::await_live reported Live for a worker that never started — surface lie (got {awaited:?})"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_trust_prompt() {
        let output = r"
────────────────────────────────────────
 Accessing workspace:

 /private/tmp/cosmon-test-abc123

 Quick safety check: Is this a project you created or one you trust?

 ❯ 1. Yes, I trust this folder
   2. No, exit
";
        assert_eq!(classify_output(output), SessionStatus::TrustPrompt);
    }

    #[test]
    fn test_classify_blocked_tool_permission() {
        let output = r"
 Tool use

   cosmon - cosmon_list(limit: 20) (MCP)
   List molecules with filtering.

 Do you want to proceed?
 ❯ 1. Yes
   2. No

 Esc to cancel · Tab to amend
";
        assert_eq!(classify_output(output), SessionStatus::Blocked);
    }

    #[test]
    fn test_classify_blocked_takes_priority_over_working() {
        // Output contains ⏺ (tool use marker) from previous output
        // but also contains the permission prompt — should be Blocked.
        let output = "⏺ Reading file...\n\n Do you want to proceed?\n Esc to cancel\n";
        assert_eq!(classify_output(output), SessionStatus::Blocked);
    }

    #[test]
    fn test_classify_ready_prompt() {
        let output = "some previous output\n\n❯ ";
        assert_eq!(classify_output(output), SessionStatus::Ready);
    }

    #[test]
    fn test_classify_ready_type_message() {
        let output = "Welcome to Claude Code!\n\nType your message to get started.\n";
        assert_eq!(classify_output(output), SessionStatus::Ready);
    }

    #[test]
    fn test_classify_working() {
        let output = "⏺ Reading file: src/main.rs\n\nAnalyzing the code...\n";
        assert_eq!(classify_output(output), SessionStatus::Working);
    }

    #[test]
    fn test_classify_thinking() {
        let output = "Thinking about the problem...\n";
        assert_eq!(classify_output(output), SessionStatus::Working);
    }

    #[test]
    fn test_classify_loading() {
        let output = "Loading project configuration...\n";
        assert_eq!(classify_output(output), SessionStatus::Loading);
    }

    #[test]
    fn test_classify_first_run_theme_wizard() {
        // Captured from a real Claude Code v2.1.140 first-run session
        // (smithy T25, Path A) — the wizard contains `❯` as a menu cursor
        // and must be classified as Loading, not Ready.
        let output = r"
 Welcome to Claude Code v2.1.140

 Let's get started.
 Choose the text style that looks best with your terminal

 ❯ 1. Dark mode
   2. Light mode
   3. Dark mode (colorblind-friendly)
";
        assert_eq!(classify_output(output), SessionStatus::Loading);
    }

    #[test]
    fn test_classify_first_run_welcome() {
        // The "Let's get started" banner alone is enough to classify as
        // Loading, even before the theme menu has rendered.
        let output = "Welcome to Claude Code v2.1.140\n\nLet's get started.\n";
        assert_eq!(classify_output(output), SessionStatus::Loading);
    }

    #[test]
    fn test_classify_first_run_takes_priority_over_menu_chevron() {
        // Regression: the menu chevron `❯` in the wizard's last 5 lines must
        // not produce a false Ready verdict. Order is load-bearing in
        // classify_output — first-run detection runs before the chevron scan.
        let output = r"
 Let's get started.
 Choose the text style that looks best with your terminal

 ❯ 1. Dark mode
";
        assert_eq!(classify_output(output), SessionStatus::Loading);
    }

    #[test]
    fn test_classify_empty_output() {
        assert_eq!(classify_output(""), SessionStatus::Unknown);
    }

    #[test]
    fn test_classify_unknown() {
        let output = "some random text that matches nothing\n";
        assert_eq!(classify_output(output), SessionStatus::Unknown);
    }

    #[test]
    fn test_trust_prompt_takes_priority_over_ready() {
        // The trust prompt contains ❯ as a cursor marker.
        // Trust detection must take priority.
        let output = r"
 Quick safety check: Is this a project you created?

 ❯ 1. Yes, I trust this folder
   2. No, exit
";
        assert_eq!(classify_output(output), SessionStatus::TrustPrompt);
    }

    #[test]
    fn test_wait_ready_with_mock_immediate_ready() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let config = cosmon_core::transport::RuntimeConfig::default();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("test-ready").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend.spawn(&agent, &config).unwrap();

        // Set output to show ready prompt.
        backend.set_canned_output("Welcome!\n\n❯ ");

        let status = wait_ready(
            &backend,
            &worker.id,
            Duration::from_secs(5),
            Duration::from_millis(100),
        )
        .unwrap();

        assert_eq!(status, SessionStatus::Ready);
    }

    #[test]
    fn test_wait_ready_handles_trust_then_ready() {
        #![allow(unused_imports)]
        use crate::MockBackend;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        // We can't dynamically change MockBackend's canned output mid-poll,
        // but we can verify classify_output handles the trust→ready transition.
        let trust_output = r"
 Quick safety check: Is this a project you created?
 ❯ 1. Yes, I trust this folder
   2. No, exit
";
        assert_eq!(classify_output(trust_output), SessionStatus::TrustPrompt);

        // After accepting trust, Claude transitions to ready.
        let ready_output = "❯ ";
        assert_eq!(classify_output(ready_output), SessionStatus::Ready);
    }

    #[test]
    fn test_wait_ready_dead_session_returns_error() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        // Don't spawn any session — the worker doesn't exist.
        let wid = cosmon_core::id::WorkerId::new("ghost").unwrap();

        let result = wait_ready(
            &backend,
            &wid,
            Duration::from_secs(1),
            Duration::from_millis(100),
        );

        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Substrate-agnostic liveness layer (task-20260426-d781)
    // -----------------------------------------------------------------------

    #[test]
    fn liveness_maps_every_session_status() {
        // The five "live claude printed something" states.
        assert_eq!(SessionStatus::TrustPrompt.liveness(), Liveness::Live);
        assert_eq!(SessionStatus::Loading.liveness(), Liveness::Live);
        assert_eq!(SessionStatus::Ready.liveness(), Liveness::Live);
        assert_eq!(SessionStatus::Working.liveness(), Liveness::Live);
        assert_eq!(SessionStatus::Blocked.liveness(), Liveness::Live);
        // Terminal / unrecognised.
        assert_eq!(SessionStatus::Dead.liveness(), Liveness::Dead);
        assert_eq!(SessionStatus::Unknown.liveness(), Liveness::Indeterminate);
    }

    #[test]
    fn claude_tui_probe_observes_live_on_ready_pane() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("probe-live").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output("Welcome!\n\n❯ ");

        let probe = ClaudeTuiProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "a ready ❯ pane is positive evidence of liveness"
        );
        assert_eq!(
            probe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_secs(5),
                    Duration::from_millis(50),
                )
                .unwrap(),
            Liveness::Live
        );
    }

    /// The generalised task-4046 surface-lie regression: a probe pointed at
    /// a worker that never started must refuse to report `Live`. This runs
    /// the reusable contract check [`assert_probe_refuses_dead_worker`]
    /// against the Claude TUI probe.
    #[test]
    fn probe_refuses_dead_worker() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        // Never spawned — `is_alive` is false, so the probe must see Dead,
        // never Live. This is the contract that stops a dead worker from
        // wearing a green light.
        let ghost = cosmon_core::id::WorkerId::new("ghost-worker").unwrap();

        let probe = ClaudeTuiProbe;
        assert_eq!(probe.observe(&backend, &ghost).unwrap(), Liveness::Dead);
        assert_probe_refuses_dead_worker(&probe, &backend, &ghost);
    }

    /// The default [`LiveProbe::await_live`] (used by TUI-less Adapters)
    /// polls `observe` and must also refuse to lie about a dead worker.
    #[test]
    fn default_await_live_refuses_dead_worker() {
        use crate::MockBackend;

        /// A probe with no `await_live` override — exercises the default
        /// poll path that future headless Adapters inherit.
        struct DefaultProbe;
        impl LiveProbe for DefaultProbe {
            fn observe(
                &self,
                backend: &dyn TransportBackend,
                worker_id: &WorkerId,
            ) -> Result<Liveness, TransportError> {
                Ok(detect_status(backend, worker_id)?.liveness())
            }
        }

        let backend = MockBackend::new();
        let ghost = cosmon_core::id::WorkerId::new("ghost-default").unwrap();
        assert_probe_refuses_dead_worker(&DefaultProbe, &backend, &ghost);
    }

    // -----------------------------------------------------------------------
    // Aider REPL liveness layer (task-20260607-3345 / B5)
    // -----------------------------------------------------------------------

    #[test]
    fn aider_output_is_live_on_banner() {
        let banner = "Aider v0.86.1\nMain model: kimi-k2.6 with diff edit format\nGit repo: .git with 12 files\n";
        assert!(aider_output_is_live(banner));
    }

    #[test]
    fn aider_output_is_live_on_each_banner_marker() {
        assert!(aider_output_is_live("Aider v0.99.0\n"));
        assert!(aider_output_is_live("Main model: gemini-3.1-pro\n"));
        assert!(aider_output_is_live("Git repo: .git with 3 files\n"));
        assert!(aider_output_is_live("Use /help <question> for help\n"));
    }

    #[test]
    fn aider_output_is_live_on_trailing_repl_prompt() {
        // A bare `>` on the last line is the interactive ready prompt.
        assert!(aider_output_is_live("some earlier output\n\n> "));
        assert!(aider_output_is_live("> "));
        // With in-progress operator input after the prompt.
        assert!(aider_output_is_live("> fix the bug"));
    }

    #[test]
    fn aider_output_not_live_on_empty_or_unrelated() {
        assert!(!aider_output_is_live(""));
        assert!(!aider_output_is_live("bash-5.2$ "));
        // A `>` buried mid-scrollback (e.g. a quoted diff line) is not the
        // trailing prompt and carries no banner marker.
        assert!(!aider_output_is_live(
            "> old quote\nsome later non-prompt line\n"
        ));
    }

    #[test]
    fn aider_probe_observes_live_on_banner_pane() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("aider-live").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "aider".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output("Aider v0.86.1\nMain model: kimi-k2.6\n\n> ");

        let probe = AiderProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "an aider banner is positive evidence of liveness"
        );
        assert_eq!(
            probe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_secs(5),
                    Duration::from_millis(50),
                )
                .unwrap(),
            Liveness::Live
        );
    }

    #[test]
    fn aider_probe_indeterminate_when_alive_but_silent() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("aider-booting").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "aider".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        // Session exists but has printed nothing aider-recognisable yet.
        backend.set_canned_output("");

        let probe = AiderProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Indeterminate,
            "alive-but-silent must not be reported as Live (surface-lie guard)"
        );
    }

    /// The generalised task-4046 surface-lie regression for the aider
    /// adapter: a probe pointed at a worker that never started must refuse
    /// to report `Live`. Mirror of [`probe_refuses_dead_worker`] for the
    /// Claude path, exercising the same reusable contract check against
    /// [`AiderProbe`].
    #[test]
    fn aider_probe_refuses_dead_worker() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let ghost = cosmon_core::id::WorkerId::new("ghost-aider").unwrap();

        let probe = AiderProbe;
        assert_eq!(probe.observe(&backend, &ghost).unwrap(), Liveness::Dead);
        assert_probe_refuses_dead_worker(&probe, &backend, &ghost);
    }

    #[test]
    fn codex_output_is_live_on_preamble() {
        let preamble =
            "OpenAI Codex v0.49.2\n--------\nworkdir: /tmp/wt\nmodel: gpt-5-codex\n--------\n";
        assert!(codex_output_is_live(preamble));
    }

    #[test]
    fn codex_output_is_live_on_each_marker_case_insensitive() {
        assert!(codex_output_is_live("OpenAI Codex v0.49.2\n"));
        // Bare tool name appears in lower-case diagnostics too.
        assert!(codex_output_is_live("codex: running exec\n"));
        assert!(codex_output_is_live("Model: gpt-5-codex\n"));
        assert!(codex_output_is_live("Workdir: /tmp/wt\n"));
        assert!(codex_output_is_live("User instructions:\n"));
    }

    /// task-20260711-246d — the interactive TUI banner (rendered inline by
    /// `--no-alt-screen`) must also read `Live`, so the same `CodexProbe`
    /// governs both launch modes with no separate interactive probe.
    #[test]
    fn codex_output_is_live_on_interactive_banner() {
        let interactive_banner =
            ">_ OpenAI Codex (v0.144.1)\n\n  To get started, describe a task or try one of these commands\n";
        assert!(codex_output_is_live(interactive_banner));
        // The bare version line codex prints on startup also names the tool.
        assert!(codex_output_is_live("codex-cli 0.144.1\n"));
    }

    #[test]
    fn codex_output_not_live_on_empty_or_unrelated() {
        assert!(!codex_output_is_live(""));
        assert!(!codex_output_is_live("bash-5.2$ "));
        assert!(!codex_output_is_live("some unrelated build log line\n"));
    }

    #[test]
    fn codex_probe_observes_live_on_preamble_pane() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("codex-live").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "codex".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output("OpenAI Codex v0.49.2\nworkdir: /tmp/wt\nmodel: gpt-5-codex\n");

        let probe = CodexProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "a codex exec preamble is positive evidence of liveness"
        );
        assert_eq!(
            probe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_secs(5),
                    Duration::from_millis(50),
                )
                .unwrap(),
            Liveness::Live
        );
    }

    #[test]
    fn codex_probe_indeterminate_when_alive_but_silent() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("codex-booting").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "codex".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        // Session exists but has printed nothing codex-recognisable yet.
        backend.set_canned_output("");

        let probe = CodexProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Indeterminate,
            "alive-but-silent must not be reported as Live (surface-lie guard)"
        );
    }

    /// The generalised task-4046 surface-lie regression for the codex
    /// adapter: a probe pointed at a worker that never started must refuse
    /// to report `Live`. Mirror of [`aider_probe_refuses_dead_worker`].
    #[test]
    fn codex_probe_refuses_dead_worker() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let ghost = cosmon_core::id::WorkerId::new("ghost-codex").unwrap();

        let probe = CodexProbe;
        assert_eq!(probe.observe(&backend, &ghost).unwrap(), Liveness::Dead);
        assert_probe_refuses_dead_worker(&probe, &backend, &ghost);
    }

    #[test]
    fn opencode_output_is_live_on_preamble() {
        let preamble =
            "opencode v0.3.1\n--------\nworkdir: /tmp/wt\nmodel: claude-sonnet-4-6\n--------\n";
        assert!(opencode_output_is_live(preamble));
    }

    #[test]
    fn opencode_output_is_live_on_each_marker_case_insensitive() {
        assert!(opencode_output_is_live("OpenCode v0.3.1\n"));
        assert!(opencode_output_is_live("Workdir: /tmp/wt\n"));
        assert!(opencode_output_is_live("Model: claude-sonnet-4-6\n"));
        assert!(opencode_output_is_live(
            "Share: https://opencode.ai/s/abc\n"
        ));
    }

    #[test]
    fn opencode_output_not_live_on_empty_or_unrelated() {
        assert!(!opencode_output_is_live(""));
        assert!(!opencode_output_is_live("bash-5.2$ "));
        assert!(!opencode_output_is_live("some unrelated build log line\n"));
    }

    #[test]
    fn opencode_probe_observes_live_on_preamble_pane() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("opencode-live").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "opencode".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output("opencode v0.3.1\nworkdir: /tmp/wt\nmodel: claude-sonnet-4-6\n");

        let probe = OpencodeProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "an opencode run preamble is positive evidence of liveness"
        );
        assert_eq!(
            probe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_secs(5),
                    Duration::from_millis(50),
                )
                .unwrap(),
            Liveness::Live
        );
    }

    #[test]
    fn opencode_probe_indeterminate_when_alive_but_silent() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("opencode-booting").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "opencode".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        // Session exists but has printed nothing opencode-recognisable yet.
        backend.set_canned_output("");

        let probe = OpencodeProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Indeterminate,
            "alive-but-silent must not be reported as Live (surface-lie guard)"
        );
    }

    /// The generalised task-4046 surface-lie regression for the opencode
    /// adapter: a probe pointed at a worker that never started must refuse
    /// to report `Live`. Mirror of [`codex_probe_refuses_dead_worker`].
    #[test]
    fn opencode_probe_refuses_dead_worker() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let ghost = cosmon_core::id::WorkerId::new("ghost-opencode").unwrap();

        let probe = OpencodeProbe;
        assert_eq!(probe.observe(&backend, &ghost).unwrap(), Liveness::Dead);
        assert_probe_refuses_dead_worker(&probe, &backend, &ghost);
    }

    #[test]
    fn poll_until_live_times_out_to_dead_for_absent_worker() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let ghost = cosmon_core::id::WorkerId::new("ghost-poll").unwrap();
        let probe = ClaudeTuiProbe;
        // Short window, fast poll — the worker never comes alive, so the
        // driver reports the last verdict (Dead), never Live.
        let verdict = poll_until_live(
            &probe,
            &backend,
            &ghost,
            Duration::from_millis(60),
            Duration::from_millis(20),
        )
        .unwrap();
        assert_eq!(verdict, Liveness::Dead);
        assert_ne!(verdict, Liveness::Live);
    }
}
