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
//!    composer / `⏺` tool-use / permission prompt) and auto-answer
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
    /// Claude Code v2.x is showing the bypass-permissions acceptance prompt —
    /// the blocking confirmation that appears the FIRST time `claude
    /// --permission-mode bypassPermissions` launches interactively. Its
    /// default-highlighted option is `1. No, exit`, so a bare Enter would
    /// quit the worker; it must be answered by selecting `2. Yes, I accept`
    /// (send "2" + Enter). See [`wait_ready`] for the handshake.
    BypassPermsPrompt,
    /// Claude is loading / initializing (spinner, "Loading..." etc.).
    Loading,
    /// Claude is ready for input (shows the `❯` prompt or "Type your message").
    Ready,
    /// Claude is actively working (tool calls, thinking, output streaming).
    Working,
    /// Claude is blocked waiting for user input (tool permission, confirmation).
    Blocked,
    /// The pane has painted a frame and is parked on it, waiting for a human —
    /// an onboarding menu, a consent screen, a free-text field nobody named.
    ///
    /// This is the closed default's *rendered* half, and it exists so the two
    /// questions of contract C0 stay two questions. Something drew that screen,
    /// so [`Self::liveness`] calls it [`Liveness::Live`]: the spawn
    /// postcondition asks *"did the binary run?"* and a painted frame is the
    /// strongest possible yes. [`ClaudeTuiProbe::await_live`] asks the other
    /// question — *"is it accepting work?"* — and overrides this to
    /// [`Liveness::Indeterminate`], because a screen waiting on a human is not
    /// a worker accepting a briefing.
    ///
    /// Distinct from [`Self::Unknown`], which is *nothing recognisable at all*
    /// (a blank pane, a crash log, output from something that is not the TUI).
    AwaitingHuman,
    /// The session is alive but the output does not match any known pattern.
    Unknown,
    /// The session is not alive (tmux session doesn't exist).
    Dead,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrustPrompt => f.write_str("trust-prompt"),
            Self::BypassPermsPrompt => f.write_str("bypass-perms-prompt"),
            Self::Loading => f.write_str("loading"),
            Self::Ready => f.write_str("idle"),
            Self::Working => f.write_str("working"),
            Self::Blocked => f.write_str("blocked"),
            Self::AwaitingHuman => f.write_str("awaiting-human"),
            Self::Unknown => f.write_str("unknown"),
            Self::Dead => f.write_str("dead"),
        }
    }
}

impl SessionStatus {
    /// Collapse the rich Claude-TUI verdict onto the substrate-agnostic
    /// [`Liveness`] axis.
    ///
    /// The seven states that prove the process *printed something a live
    /// claude would print* — `Loading`, `TrustPrompt`, `BypassPermsPrompt`,
    /// `Ready`, `Working`, `Blocked`, `AwaitingHuman` — all map to
    /// [`Liveness::Live`]. `Dead` maps to [`Liveness::Dead`]. `Unknown`
    /// (nothing recognisable rendered) maps to [`Liveness::Indeterminate`].
    ///
    /// `AwaitingHuman` belongs on the `Live` side for the same reason a
    /// rendered modal does, and this is contract clause C0: the *spawn
    /// postcondition* asks whether the binary ran, and an unnamed onboarding
    /// screen is a painted frame, therefore a yes. The refusal that shuts the
    /// door lives one layer up, in [`ClaudeTuiProbe::await_live`] — keeping the
    /// two questions apart is what stops a slow cold start whose first frame is
    /// a screen nobody named from being torn down.
    ///
    /// This is the load-bearing translation between Claude's pane signature
    /// and the contract a TUI-less Adapter answers: the caller never has to
    /// know which TUI string was matched, only whether the worker is alive.
    #[must_use]
    pub fn liveness(&self) -> Liveness {
        match self {
            Self::TrustPrompt
            | Self::BypassPermsPrompt
            | Self::Loading
            | Self::Ready
            | Self::Working
            | Self::Blocked
            | Self::AwaitingHuman => Liveness::Live,
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
    /// The bypass-permissions acceptance banner — Claude Code v2.x shows this
    /// the first time it launches under `--permission-mode bypassPermissions`.
    pub const BYPASS_PERMS_WARNING: &str = "Bypass Permissions mode";
    /// The actionable accept option in the same prompt — the anchor we key on
    /// to know the prompt is waiting for a `2` + Enter, not merely a passing
    /// mention of bypass mode in scrollback.
    pub const BYPASS_PERMS_ACCEPT: &str = "Yes, I accept";
    /// The chevron Claude Code paints at the head of an input line.
    ///
    /// **This character alone proves nothing.** It is the composer's prompt
    /// *and* the selection cursor of every menu the TUI draws — the trust
    /// dialog, the bypass-permissions consent, the first-run theme wizard, the
    /// login-method selector. Treating a bare sighting of it as "ready for
    /// input" is the open default that issue #20 walked through four times;
    /// see [`shows_composer`] for the evidence rule that replaced it.
    pub const READY_PROMPT: &str = "❯";
    /// The composer placeholder — an empty input box saying, in words, that it
    /// is waiting for a message. Positive evidence of a work-accepting pane
    /// **when it sits on the input line itself**; quoted in body prose it is
    /// just a sentence about the composer (see [`shows_composer`]).
    pub const READY_TYPE: &str = "Type your message";
    /// The permission-mode glyph Claude Code paints in the composer's footer.
    ///
    /// The next three constants are the composer's own footer — the status /
    /// hint line the TUI draws directly under the input box, and *only* there.
    /// They are **composer** evidence, not door names: they say "the input line
    /// beside me belongs to the composer", which is precisely the co-evidence a
    /// bare chevron lacks. Naming them does not re-open the corridor C2 shuts,
    /// because they widen what counts as a *composer*, never what counts as a
    /// recognised blocking screen.
    pub const COMPOSER_MODE_GLYPH: &str = "⏵⏵";
    /// The words of the same footer — the mode-cycling hint.
    pub const COMPOSER_MODE_HINT: &str = "shift+tab to cycle";
    /// The default-mode footer, shown when no permission mode is announced.
    pub const COMPOSER_SHORTCUTS_HINT: &str = "? for shortcuts";
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
    /// `~/.claude/config.json` settings yet). Naming it buys a *richer*
    /// verdict than the closed default would give: a wizard on screen is a
    /// cold start still in progress ([`SessionStatus::Loading`], which is
    /// `Live` for the spawn postcondition), where an unnamed menu is only
    /// `Unknown`. It is not what keeps the wizard out of `Ready` — that is
    /// [`shows_composer`]'s job now.
    pub const FIRST_RUN_THEME: &str = "Choose the text style";
    /// Companion marker for the same first-run wizard banner.
    pub const FIRST_RUN_WELCOME: &str = "Let's get started";
    /// The glyph a full-width horizontal rule is drawn from — `U+2500 BOX
    /// DRAWINGS LIGHT HORIZONTAL`.
    ///
    /// Claude Code 2.1.220 stopped boxing its composer and started *ruling* it:
    /// one full-width run of this character above the input line and one below,
    /// with nothing else on either. See [`is_horizontal_rule_line`] for why that
    /// pair is composer-specific evidence where a box frame was not.
    pub const RULE_CHAR: char = '─';
    /// The glyphs Claude Code cycles through in its status slot — the line it
    /// paints directly above the composer while a turn is in flight.
    ///
    /// Version-coupled by construction: this TUI rotates its spinner and has
    /// changed the set before. The fixtures under
    /// `tests/fixtures/claude-tui-2.1.220/` are what makes a future change fail
    /// loudly here instead of silently costing every dispatch its full budget.
    ///
    /// `◐` is deliberately **absent**. It occupies the same slot when the pane
    /// is idle (`◐ medium · /effort`), so admitting it would call every idle
    /// 2.1.220 pane `Working`.
    pub const SPINNER_GLYPHS: [char; 8] = ['✢', '✳', '✶', '✻', '✽', '✺', '✴', '❋'];
    /// The interrupt hint carried by the status line while a turn is running.
    pub const INTERRUPT_HINT: &str = "esc to interrupt";
}

/// How many trailing lines of the pane [`detect_status`] asks the backend for.
///
/// This is a **scrollback** window, not a screen: `TmuxBackend::capture_output`
/// captures from the start of history and hands back the last `lines` of it, so
/// a capture of 30 routinely reaches above the top of the visible pane and
/// carries output from earlier frames. Everything that classifies on
/// [`pane_tail`] is immune to that by construction; the whole-capture arms of
/// [`classify_output`] are not, which is why they are the ones that had to earn
/// their evidence too (see that function's `Loading` / `Working` arms).
const CAPTURE_LINES: usize = 30;

/// Inspect a session's terminal output and classify its state.
///
/// Reads the last `CAPTURE_LINES` (30) lines of the session's terminal and
/// matches against known patterns.
///
/// # Errors
///
/// Returns [`TransportError`] if the session cannot be queried.
pub fn detect_status(
    backend: &dyn TransportBackend,
    worker_id: &WorkerId,
) -> Result<SessionStatus, TransportError> {
    if !backend.is_alive(worker_id)? {
        crate::readiness_trace::record(
            &crate::readiness_trace::Sample::new("capture", worker_id.as_str())
                .status(&SessionStatus::Dead)
                .note("is_alive said no"),
        );
        return Ok(SessionStatus::Dead);
    }

    let output = backend.capture_output(worker_id, CAPTURE_LINES)?;
    let status = classify_output(&output);

    // The single most important line in the trace: the classified verdict
    // beside the exact bytes it was computed from. Arm C's contradiction —
    // classifier-refuses / dispatch-proceeds — cannot be resolved without
    // knowing whether the probe was even looking at the same screen the bench
    // captured afterwards.
    crate::readiness_trace::record(
        &crate::readiness_trace::Sample::new("capture", worker_id.as_str())
            .status(&status)
            .pane(&output),
    );

    Ok(status)
}

/// `true` when captured pane `output` shows Claude Code's bypass-permissions
/// acceptance prompt (the [`SessionStatus::BypassPermsPrompt`] gate).
///
/// Pure function — no I/O. Keyed on the co-presence of the warning banner and
/// the actionable accept option, so a mere mention of "bypass" in scrollback
/// does not trip it. The prompt is answered by [`wait_ready`] with `2` + Enter
/// (selecting `2. Yes, I accept`), never a bare Enter — its default option is
/// `1. No, exit`, which would tear the worker down.
#[must_use]
pub fn is_bypass_perms_prompt(output: &str) -> bool {
    output.contains(markers::BYPASS_PERMS_WARNING) && output.contains(markers::BYPASS_PERMS_ACCEPT)
}

/// How many trailing non-empty lines count as "what the pane is showing now".
///
/// Anything above this is scrollback: history, not current state.
const TAIL_LINES: usize = 5;

/// The trailing `TAIL_LINES` non-empty lines of `output`, newest first.
fn pane_tail(output: &str) -> Vec<&str> {
    output
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(TAIL_LINES)
        .collect()
}

/// The same window as [`pane_tail`], in the order the TUI painted it.
///
/// [`pane_tail`]'s callers all ask *"is any line in the tail like this?"*, for
/// which order is irrelevant. The 2.1.220 composer is recognised by a
/// three-line *arrangement* instead — rule, input line, rule — so it needs the
/// lines the right way round and adjacent to each other. Blank lines are
/// dropped by both, which is what lets the arrangement survive the blank row
/// this TUI leaves between its lower rule and its footer.
fn pane_tail_ordered(output: &str) -> Vec<&str> {
    let mut tail = pane_tail(output);
    tail.reverse();
    tail
}

/// The shortest run of [`markers::RULE_CHAR`] that counts as a rule.
///
/// The composer's rules span the whole pane (200 columns in the captured
/// fixtures). The floor exists only to keep a short divider drawn inside prose
/// — `───` between two paragraphs of an answer — from standing in for one.
const MIN_RULE_WIDTH: usize = 8;

/// How many trailing non-empty lines [`shows_work_in_flight`] searches for the
/// status slot.
///
/// Four of them are spent on the composer itself before the slot is reached;
/// the rest is headroom for whatever else the TUI parks down there (a tmux
/// warning occupies the first spare row in `streaming-1.pane`). See that
/// function's doc for why this window may be wider than [`TAIL_LINES`] without
/// letting scrollback speak.
const STATUS_SLOT_LINES: usize = 8;

/// `true` when `line` is a bare full-width horizontal rule and nothing else.
///
/// **Nothing else** is the load-bearing half. This TUI still boxes its modals
/// (`╭───╮` … `│ ❯ a) Re-authorise now │` … `╰───╯`), and a box corner or a
/// vertical bar anywhere on the line disqualifies it here — which is what keeps
/// [`shows_rule_framed_composer`] from re-opening the corridor that offering a
/// box frame as composer evidence once opened. Only the composer draws a rule
/// that is *only* a rule.
fn is_horizontal_rule_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= MIN_RULE_WIDTH && trimmed.chars().all(|c| c == markers::RULE_CHAR)
}

/// `true` when the pane's tail shows Claude Code 2.1.220's composer — an input
/// line sandwiched between two bare horizontal rules.
///
/// This is the composer signature that replaced the boxed one, and its absence
/// from [`markers`] is why seven real 2.1.220 panes — one idle, six mid-stream
/// — all classified [`SessionStatus::AwaitingHuman`]. The footer disjuncts in
/// [`is_composer_footer_line`] happened to rescue the *bypass-permissions*
/// spawn (its footer paints `⏵⏵`) and nothing else: the same build launched by
/// hand in manual mode painted `⏸ manual mode on` and could not be dispatched
/// into at all.
///
/// The arrangement is demanded, not merely the ingredients: rule, then an input
/// line that [`is_menu_option_line`] does not claim, then rule, adjacent in the
/// tail. A menu whose cursor rests on an option is still refused, so the closed
/// default of [`shows_composer`] is preserved rather than widened — this adds
/// one more way to *prove* a composer, never one more way to skip the proof.
fn shows_rule_framed_composer(output: &str) -> bool {
    pane_tail_ordered(output).windows(3).any(|w| {
        is_horizontal_rule_line(w[0])
            && is_horizontal_rule_line(w[2])
            && chevron_content(w[1]).is_some()
            && !is_menu_option_line(w[1])
    })
}

/// `true` when the pane's status slot — the line directly above the composer —
/// says a turn is **in flight right now**.
///
/// The evidence is a spinner glyph at the head of the line plus a running
/// clock: `✢ Coalescing… (3s · thinking with medium effort)`. Both halves are
/// required, and the second one is the whole reason this predicate is narrow.
/// When a turn finishes, the same slot keeps a summary — `✻ Baked for 16s` —
/// and keeps it until the *next* turn starts. Accepting that would call a pane
/// idle since yesterday `Working`, and would let the briefing-submit loop
/// declare a briefing delivered on the strength of the previous turn's
/// leftovers. A parenthesised elapsed timer, or the interrupt hint, only
/// appears while something is actually running.
///
/// # Why its window is not [`pane_tail`]'s
///
/// The 2.1.220 composer costs four non-empty lines on its own — footer, lower
/// rule, input line, upper rule — so a five-line tail leaves exactly one row
/// for the status slot, and in `streaming-1.pane` a tmux warning is sitting in
/// it. The slot is six rows up in that frame and the classifier could not see
/// it. [`STATUS_SLOT_LINES`] is the window that reaches it.
///
/// Widening is safe *here* and would not be for the other rules, which is why
/// it is not shared. The status slot is a fixed row this TUI **overwrites**,
/// never a line it appends: when a turn ends the running clock is replaced in
/// place by the `Baked for 16s` summary, so a `(3s · …` line cannot survive
/// into scrollback and go on certifying a turn that finished. The composer and
/// menu rules have no such property — a composer genuinely does scroll away
/// above a modal — so they keep the tight tail.
fn shows_work_in_flight(output: &str) -> bool {
    let window: Vec<&str> = output
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(STATUS_SLOT_LINES)
        .collect();
    window.iter().any(|line| {
        let content = unframe(line);
        let Some(first) = content.chars().next() else {
            return false;
        };
        if !markers::SPINNER_GLYPHS.contains(&first) {
            return false;
        }
        content.contains(markers::INTERRUPT_HINT) || has_elapsed_timer(content)
    })
}

/// `true` when `line` carries a parenthesised elapsed-seconds token — the
/// `(3s · …` of a status line whose clock is still running.
fn has_elapsed_timer(line: &str) -> bool {
    line.split('(').skip(1).any(|rest| {
        let digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        digits.len() < rest.len() && digits.starts_with('s')
    })
}

/// The box-drawing characters Claude Code paints around its composer.
const FRAME_CHARS: [char; 4] = ['│', '|', '┃', '║'];

/// Strip the leading whitespace and box-drawing frame Claude Code paints
/// around its composer, leaving the line's actual content.
fn unframe(line: &str) -> &str {
    line.trim().trim_start_matches(FRAME_CHARS).trim_start()
}

/// What the chevron on `line` points at, or `None` when the line carries no
/// chevron at its head.
///
/// `Some("")` is the REPL's idle input line; `Some("1. Split panes")` is a
/// menu's selection cursor; `Some("fix the failing test")` is either a composer
/// holding unsubmitted text or a menu option that is not numbered. Which of the
/// three it is cannot be read off this line alone — that is the whole reason
/// [`shows_composer`] demands co-evidence.
fn chevron_content(line: &str) -> Option<&str> {
    unframe(line)
        .strip_prefix(markers::READY_PROMPT)
        .map(unframe)
}

/// The bullet glyphs this TUI draws at the head of an unnumbered menu option.
///
/// Deliberately excludes `-` and `*`. Those open a Markdown list at least as
/// readily as a menu, so a composer holding an unsubmitted draft that begins
/// with one would be read as a menu and refused — the round-1 F5
/// over-correction, running in the other direction.
const MENU_BULLETS: [char; 3] = ['•', '‣', '▸'];

/// `true` when `rest` — the text a chevron points at — has the shape of a menu
/// option.
///
/// The shapes this TUI actually draws: a decimal index of any width followed by
/// its separator (`1. Split panes`, `10) Work offline`), a single-letter index
/// (`a) Re-authorise now`), or a bullet (`• Decide later`). The width was the
/// bug: keying on exactly two characters made `10.` and `a)` invisible, and each
/// miss turned a menu cursor into "not a menu option", which is the precondition
/// the boxed-menu escape exploited.
fn is_menu_option_shape(rest: &str) -> bool {
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if MENU_BULLETS.contains(&first) {
        return true;
    }
    if first.is_ascii_alphabetic() {
        return chars.next().is_some_and(|c| c == '.' || c == ')');
    }
    if !first.is_ascii_digit() {
        return false;
    }
    let after_digits = chars
        .as_str()
        .trim_start_matches(|c: char| c.is_ascii_digit());
    after_digits.starts_with('.') || after_digits.starts_with(')')
}

/// `true` when the chevron on `line` is a menu's selection cursor resting on an
/// option — `❯ 1. Split panes`, `❯ b) Work offline`, `❯ • Decide later`.
///
/// Keyed on menu *shape* rather than on "the chevron has content", so a
/// composer holding a suggestion, a reworded placeholder or a localised one is
/// not swept in with the menus.
fn is_menu_option_line(line: &str) -> bool {
    chevron_content(line).is_some_and(is_menu_option_shape)
}

/// `true` when `line` is the composer's own footer — the mode / shortcuts hint
/// the TUI paints directly under the input box, and nowhere else.
fn is_composer_footer_line(line: &str) -> bool {
    line.contains(markers::COMPOSER_MODE_GLYPH)
        || line.contains(markers::COMPOSER_MODE_HINT)
        || line.contains(markers::COMPOSER_SHORTCUTS_HINT)
}

/// `true` when the pane carries **positive evidence that the composer is on
/// screen and accepting input** — the closed default that shuts the corridor
/// behind noogram/cosmon#20 (contract clauses C1 and C2).
///
/// [`SessionStatus::Ready`] is *earned* here; it is never inherited from the
/// mere presence of a chevron. Every rule below is scoped to
/// [`pane_tail`] — what the pane is showing **now**. Scrollback is history, and
/// history must not certify the present: a capture that scrolled past a
/// composer thirty lines ago says nothing about the menu on screen today.
///
/// Three things count as evidence, and each needs an input line *plus*
/// something that identifies it as the composer's:
///
/// 1. the composer placeholder [`markers::READY_TYPE`] sitting **on** a chevron
///    line — an empty input box that says in words that it wants a message. In
///    body prose ("Type your message to get started") the same phrase is a
///    welcome banner talking *about* the composer, not a composer;
/// 2. a chevron line that is not a menu option ([`is_menu_option_line`]),
///    standing in the same tail as the composer's own footer
///    ([`is_composer_footer_line`]); and
/// 3. a chevron line that is not a menu option, ruled directly above and below
///    ([`shows_rule_framed_composer`]) — the arrangement Claude Code 2.1.220
///    draws, where the boxed composer of (2)'s era used to be.
///
/// A bare chevron on its own is deliberately **not** enough. It is the shape of
/// any empty input line, including the paste-the-authorization-code field one
/// step behind the login-method selector — accepting it would shut one screen
/// and re-open the corridor on the next.
///
/// A **box frame** is not enough either, and offering it as co-evidence is how
/// this rule briefly opened a door of its own: this TUI boxes its modals as
/// readily as its composer, so `│ ❯ a) Re-authorise now │` satisfied a
/// box-frame disjunct and became `Ready`. Only the two composer-*specific*
/// signals above survive — the placeholder and the footer, each of which says
/// "the input line beside me belongs to the composer" in a way a frame cannot.
///
/// Everything else is refused, including every screen nobody has named yet.
/// That refusal is the whole point. Claude Code's onboarding and consent
/// screens are menus, and every menu draws `❯` as its selection cursor, so the
/// original rule — *scan the last five lines for a chevron* — admitted any
/// blocking screen as ready. Each door shut so far (`TRUST_PROMPT`,
/// `BYPASS_PERMS_WARNING`, `FIRST_RUN_THEME`) was shut by adding one more name
/// to [`markers`], and the login-method selector was simply the first screen
/// nobody had named yet. Naming it would have shut a door and left the
/// corridor open. Demanding evidence closes the corridor: a screen this build
/// has never seen cannot become `Ready`, because it cannot produce, *on the
/// frame it is painting right now*, a composer it is not showing.
fn shows_composer(output: &str) -> bool {
    let tail = pane_tail(output);

    // (1) The placeholder ON the input line.
    if tail
        .iter()
        .any(|l| chevron_content(l).is_some_and(|rest| rest.contains(markers::READY_TYPE)))
    {
        return true;
    }

    // (2) A non-menu input line under the composer's own footer.
    if tail.iter().any(|l| is_composer_footer_line(l))
        && tail
            .iter()
            .any(|l| chevron_content(l).is_some() && !is_menu_option_line(l))
    {
        return true;
    }

    // (3) The 2.1.220 arrangement: a non-menu input line ruled above and below.
    shows_rule_framed_composer(output)
}

/// `true` when the pane's tail shows an input line that [`shows_composer`] has
/// already refused — a menu's selection cursor resting on an option, a blocking
/// free-text field, or a composer holding text nobody has submitted.
///
/// Only ever consulted *after* [`shows_composer`] has said no, so by
/// construction whatever chevron it finds is not certified composer evidence.
/// The three are not distinguishable from bytes, and they do not need to be:
/// all three mean the pane is parked waiting on a human, and none is proof of a
/// worker accepting work.
///
/// Its job is to stop *scrollback* from speaking over the current screen — a
/// `⏺` left from an earlier turn must not report `Working` while a question is
/// on screen right now. That matters in both directions: it keeps an unnamed
/// menu out of `Working` (the corridor stays shut even when the pane has
/// history), and it keeps a pasted-but-unsubmitted briefing out of `Working`
/// too — a pane whose composer still holds the briefing has not started work,
/// whatever an older `⏺` further up says.
///
/// Note what no longer depends on this: `cs tackle`'s briefing-submit loop used
/// to read `Working` as "delivered". It does not any more — delivery is proven
/// by the briefing text leaving the composer — because on Claude Code 2.1.220
/// this classifier never answers `Working` at all (COSMON #26-A).
fn awaits_a_human_at_a_chevron(output: &str) -> bool {
    pane_tail(output)
        .iter()
        .any(|l| chevron_content(l).is_some())
}

/// `true` when the pane's tail carries box-drawing characters — the strongest
/// substrate-free evidence available here that *something painted a frame*.
///
/// Used only as the last word before [`SessionStatus::Unknown`], and only to
/// answer the spawn postcondition's question. A login screen whose field is
/// drawn with `>` rather than `❯` still paints its box, so
/// [`awaits_a_human_at_a_chevron`] misses it while this does not; reading such
/// a pane as "nothing ran" is the surface lie inverted, and it costs the
/// operator a real diagnostic. It cannot open the dispatch gate:
/// `AwaitingHuman` is refused by [`ClaudeTuiProbe::await_live`].
fn pane_painted_a_frame(output: &str) -> bool {
    pane_tail(output)
        .iter()
        .any(|l| l.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)))
}

/// Classify raw terminal output into a session status.
///
/// Pure function — no I/O. Examines the last lines of output to determine
/// which state the Claude session is in.
///
/// # The closed default (noogram/cosmon#20)
///
/// Order is load-bearing, and so is the *end* of the order. A pane parked on an
/// input line this build cannot certify as a composer lands on
/// [`SessionStatus::AwaitingHuman`]; a pane matching nothing at all lands on
/// [`SessionStatus::Unknown`]. Both make [`ClaudeTuiProbe::await_live`] refuse
/// to dispatch, and they differ where the difference matters: `AwaitingHuman`
/// is `Live` at the spawn postcondition (something rendered), `Unknown` is
/// `Indeterminate` there (nothing did). `Ready` is reachable only through
/// positive evidence that the composer is on screen — the private
/// `shows_composer` predicate, whose doc comment records why naming one more
/// screen would not have been a fix.
#[must_use]
pub fn classify_output(output: &str) -> SessionStatus {
    // Check from most specific to least specific.
    // Trust prompt is the most urgent — it blocks everything.
    if output.contains(markers::TRUST_PROMPT) || output.contains(markers::TRUST_PROMPT_ALT) {
        return SessionStatus::TrustPrompt;
    }

    // The bypass-permissions acceptance prompt must be classified BEFORE the
    // generic `Blocked` check below: it carries the same `Esc to cancel`
    // footer as any yes/no dialog, but its default-highlighted option is
    // `1. No, exit`. Treating it as `Blocked` would make `wait_ready` send a
    // bare Enter, which selects "No, exit" and kills the worker — the exact
    // failure this detection exists to prevent (noogram/cosmon#6). Naming it
    // also earns it the `Live` verdict a rendered dialog deserves at the spawn
    // postcondition, where the closed default would only say `Unknown`.
    if is_bypass_perms_prompt(output) {
        return SessionStatus::BypassPermsPrompt;
    }

    // Check for blocked state — Claude is waiting for permission/confirmation.
    //
    // Tail-scoped, for the same reason `shows_composer` is: history must not
    // certify the present. Whole-capture, a permission question answered
    // twenty lines ago forced `Blocked` over whatever screen the pane is
    // painting now — and `Blocked` is the one arm that runs *before* the
    // composer evidence rule, so the scrollback verdict won outright.
    if pane_tail(output)
        .iter()
        .any(|l| l.contains(markers::TOOL_PERMISSION) || l.contains(markers::YES_NO_PROMPT))
    {
        return SessionStatus::Blocked;
    }

    // First-run wizard (Claude Code v2.1.140+). Named so a cold start caught
    // mid-wizard reads `Loading` — which is `Live` for the spawn
    // postcondition — rather than the `Unknown` the closed default would give
    // any unnamed menu. It no longer has to precede a chevron scan to stay out
    // of `Ready`; `shows_composer` refuses it on evidence.
    if output.contains(markers::FIRST_RUN_THEME) || output.contains(markers::FIRST_RUN_WELCOME) {
        return SessionStatus::Loading;
    }

    // A status line whose clock is still running outranks the composer, and
    // this ordering is the second half of the 2.1.220 repair.
    //
    // The composer used to be checked first, on the rule "an input box at the
    // bottom means idle, whatever the scrollback says". That rule was sound
    // while the prompt *vanished* for the duration of a turn — its truth came
    // entirely from the disappearance. In 2.1.220 the composer stays painted
    // for the whole stream, so the premise is gone and with it every path to
    // `Working`: six panes captured four seconds apart mid-stream showed the
    // composer in every frame. Downstream, `cs tackle`'s briefing-submit loop
    // has `Working` as its only early exit, so an unreachable arm is a flat
    // 90 s added to every dispatch.
    //
    // What replaces the disappearing prompt is `shows_work_in_flight`, which
    // reads the slot the composer's rules do not cover — and reads it narrowly
    // enough that a *finished* turn's leftover summary does not qualify. That
    // narrowness is what keeps the original rule's real purpose intact: a `⏺`
    // from an earlier turn still must not report `Working` over an idle pane,
    // and it cannot, because a completed turn leaves no running clock.
    if shows_work_in_flight(output) {
        return SessionStatus::Working;
    }

    // Then the composer — if the pane is showing an input box at the bottom and
    // nothing is in flight above it, Claude is idle regardless of past ⏺
    // markers in scrollback. This fixes false "working" detection from old
    // tool-use output.
    //
    // The evidence rule lives in `shows_composer`, and it is the closed
    // default that shuts the issue-#20 corridor: a chevron pointing at a menu
    // option is a cursor, not a prompt, so no unnamed onboarding screen can
    // arrive here and be called `Ready`.
    if shows_composer(output) {
        return SessionStatus::Ready;
    }

    // A pane parked at an input line that is not certified composer evidence
    // is a pane waiting for a human. Say so *before* the scrollback checks
    // below, so a `⏺` from an earlier turn cannot report `Working` over a
    // screen that is asking a question right now.
    //
    // `AwaitingHuman`, not `Unknown`: something painted that frame, and the
    // spawn postcondition is entitled to know it (contract C0). The refusal
    // that shuts the door is `await_live`'s, one layer up.
    if awaits_a_human_at_a_chevron(output) {
        return SessionStatus::AwaitingHuman;
    }

    // Only check for work indicators if we didn't find a composer.
    // This means Claude is mid-output (no prompt yet).
    if output.contains(markers::TOOL_USE) || output.contains(markers::THINKING) {
        return SessionStatus::Working;
    }

    // Check for loading state.
    if output.contains(markers::LOADING) {
        return SessionStatus::Loading;
    }

    // Last word before "nothing recognisable": a painted frame with no chevron
    // in it is still a painted frame. `AwaitingHuman` rather than `Unknown` so
    // the spawn postcondition keeps its "did the binary run?" answer — the
    // dispatch gate refuses both alike.
    if pane_painted_a_frame(output) {
        return SessionStatus::AwaitingHuman;
    }

    SessionStatus::Unknown
}

/// How many times the handshake re-answers one startup modal before giving up.
///
/// The predecessor answered each modal exactly once, latched on a `bool`. That
/// makes a single swallowed keystroke unrecoverable — and a cold container
/// painting its first frame is precisely where `tmux send-keys` lands before the
/// TUI's input handler is attached. The pane then sits on the question for the
/// rest of the window with the answer already "spent". Re-answering costs
/// nothing when the modal is gone (the arm is only reached while it is still on
/// screen) and is bounded so a genuinely wedged dialog cannot be hammered for
/// the whole timeout. Belt to [`crate::claude_trust`]'s braces: with consent
/// pre-granted the modal should never render at all.
const MODAL_ANSWER_ATTEMPTS: u32 = 3;

/// Wait for a session to reach `Ready` state, handling blocking prompts.
///
/// Polls the session every `poll_interval` until it is `Ready` or the
/// `timeout` expires. A `TrustPrompt` is answered with Enter (option 1 is
/// pre-highlighted) and a `BypassPermsPrompt` with `2` + Enter, each up to
/// `MODAL_ANSWER_ATTEMPTS` times.
///
/// Returns the final [`SessionStatus`] when ready or when timeout expires. A
/// caller deciding whether the worker will *accept work* must not read a
/// returned `TrustPrompt` / `BypassPermsPrompt` as success — see
/// [`ClaudeTuiProbe::await_live`], which maps those to
/// [`Liveness::Indeterminate`] for exactly that reason.
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
    let mut trust_answers = 0_u32;
    let mut bypass_answers = 0_u32;

    while start.elapsed() < timeout {
        let status = detect_status(backend, worker_id)?;

        match status {
            SessionStatus::Ready => {
                crate::readiness_trace::record(
                    &crate::readiness_trace::Sample::new("wait_ready.return", worker_id.as_str())
                        .elapsed_ms(start.elapsed().as_millis())
                        .status(&SessionStatus::Ready)
                        .note("composer evidence — returned before the window closed"),
                );
                return Ok(SessionStatus::Ready);
            }
            SessionStatus::Working => {
                crate::readiness_trace::record(
                    &crate::readiness_trace::Sample::new("wait_ready.return", worker_id.as_str())
                        .elapsed_ms(start.elapsed().as_millis())
                        .status(&SessionStatus::Working)
                        .note("work evidence — returned before the window closed"),
                );
                return Ok(SessionStatus::Working);
            }
            SessionStatus::Dead => return Err(TransportError::NotFound(worker_id.clone())),
            SessionStatus::TrustPrompt => {
                if trust_answers < MODAL_ANSWER_ATTEMPTS {
                    // The trust prompt is a TUI selection menu where option 1
                    // ("Yes, I trust this folder") is already highlighted, so a
                    // bare Enter confirms it.
                    crate::readiness_trace::record(
                        &crate::readiness_trace::Sample::new("handshake", worker_id.as_str())
                            .elapsed_ms(start.elapsed().as_millis())
                            .status(&SessionStatus::TrustPrompt)
                            .note("sending Enter to confirm the trust dialog"),
                    );
                    send_enter(backend, worker_id)?;
                    trust_answers += 1;
                }
                // Continue polling — Claude will transition to Loading then Ready.
            }
            SessionStatus::BypassPermsPrompt => {
                if bypass_answers < MODAL_ANSWER_ATTEMPTS {
                    // Unlike the trust prompt, the default-highlighted option
                    // here is `1. No, exit` — a bare Enter would quit the
                    // worker. Select `2. Yes, I accept` explicitly by sending
                    // the digit `2` followed by Enter (send_input appends the
                    // Enter).
                    crate::readiness_trace::record(
                        &crate::readiness_trace::Sample::new("handshake", worker_id.as_str())
                            .elapsed_ms(start.elapsed().as_millis())
                            .status(&SessionStatus::BypassPermsPrompt)
                            .note("sending 2 + Enter to accept bypass permissions"),
                    );
                    backend.send_input(worker_id, "2")?;
                    bypass_answers += 1;
                }
                // Continue polling — Claude dismisses the prompt and settles
                // on Loading then Ready / Working.
            }
            SessionStatus::Blocked => {
                // Session is blocked on a permission prompt.
                // Auto-accept by sending Enter (selects the default option).
                crate::readiness_trace::record(
                    &crate::readiness_trace::Sample::new("handshake", worker_id.as_str())
                        .elapsed_ms(start.elapsed().as_millis())
                        .status(&SessionStatus::Blocked)
                        .note("sending Enter to accept the default option"),
                );
                send_enter(backend, worker_id)?;
                // Continue polling — Claude will proceed after acceptance.
            }
            SessionStatus::Loading | SessionStatus::AwaitingHuman | SessionStatus::Unknown => {
                // Still booting, parked on a screen nobody named, or showing
                // nothing recognisable — keep waiting. Deliberately NOT
                // answered: cosmon does not drive onboarding, and pressing a
                // key into a screen it cannot read is how a briefing ended up
                // typed into a two-option menu.
            }
        }

        std::thread::sleep(poll_interval);
    }

    // Timeout — return whatever state we last observed.
    //
    // This is the path that carries the whole issue-#20 door-4 argument, and it
    // is why the trace names it: a status reached *by exhausting the window* is
    // not the same claim as a status reached by evidence, and for a caller
    // asking "is this worker accepting work?" the difference is the answer.
    let last = detect_status(backend, worker_id);
    if let Ok(status) = &last {
        crate::readiness_trace::record(
            &crate::readiness_trace::Sample::new("wait_ready.return", worker_id.as_str())
                .elapsed_ms(start.elapsed().as_millis())
                .status(status)
                .note("TIMEOUT — window exhausted, returning the last observed status"),
        );
    }
    last
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
    let started = Instant::now();
    let deadline = started + window;
    let mut last = Liveness::Indeterminate;
    loop {
        match probe.observe(backend, worker_id) {
            Ok(Liveness::Live) => {
                crate::readiness_trace::record(
                    &crate::readiness_trace::Sample::new(
                        "spawn_postcondition.return",
                        worker_id.as_str(),
                    )
                    .elapsed_ms(started.elapsed().as_millis())
                    .liveness(&Liveness::Live)
                    .note("evidence of life within the window"),
                );
                return Ok(Liveness::Live);
            }
            Ok(other) => last = other,
            // A transient query failure is not evidence of death — keep
            // polling within the window (pre-refactor `.unwrap_or` shape).
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            crate::readiness_trace::record(
                &crate::readiness_trace::Sample::new(
                    "spawn_postcondition.return",
                    worker_id.as_str(),
                )
                .elapsed_ms(started.elapsed().as_millis())
                .liveness(&last)
                .note("TIMEOUT — window exhausted"),
            );
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
        Self::await_live_with_status(backend, worker_id, timeout, poll_interval)
            .map(|(_status, liveness)| liveness)
    }
}

/// The dispatch gate's collapse: which Claude-TUI verdicts mean *"this worker
/// is accepting work"*.
///
/// An **allow-list**, and that is the whole of the issue-#20 door-4 fix.
///
/// # Why an allow-list
///
/// [`wait_ready`] has exactly two ways to return: it returns [`SessionStatus::Ready`]
/// or [`SessionStatus::Working`] the moment it *sees* them, and it returns
/// everything else only by running out of window. So *"did this status arrive
/// as evidence?"* and *"is this status `Ready` or `Working`?"* have the same
/// answer — and a gate whose question is *"is it accepting work?"* has no
/// business saying yes to anything else.
///
/// This used to be a deny-list inlined in [`ClaudeTuiProbe::await_live_with_status`]:
/// four named statuses forced to [`Liveness::Indeterminate`], everything else
/// collapsed through [`SessionStatus::liveness`]. It read as safe and it was
/// not, for the same reason naming one more marker was never a fix: **what is
/// not on the list dispatches**. `Loading` was not on the list.
///
/// # What the bench measured
///
/// On the instrumented issue-#20 container run of 2026-07-25 (arm C, virgin
/// `CLAUDE_CONFIG_DIR`), `readiness_trace` recorded the gap in two lines:
///
/// ```text
/// 30155  wait_ready.return  loading  TIMEOUT — window exhausted
///        dispatch_gate      loading  live
/// ```
///
/// For the entire 30 s window the pane was Claude Code's first-run **theme
/// wizard** — `Let's get started.` / `Choose the text style` — which
/// [`classify_output`] deliberately calls `Loading`, because a wizard on screen
/// genuinely is a cold start still in progress. The window closed with the
/// wizard unanswered (cosmon does not drive onboarding), `wait_ready` handed
/// back its last observation, and the deny-list let `Loading` through to
/// [`Liveness::Live`]. `cs tackle` then typed an 80-line briefing into the
/// wizard — whose keystrokes answered it and advanced the pane to the
/// login-method selector, which is why every capture taken *after* tackle
/// returned showed a selector the process had never once classified. That is
/// the whole of "unit-green and bench-red at the same seam": the unit tests
/// were reasoning about a pane the gate never saw.
///
/// # What it does not change
///
/// Contract C0 is untouched, and this is where to check it.
/// [`SessionStatus::liveness`] still calls a rendered frame `Live`, so
/// [`LiveProbe::observe`] and `cs tackle`'s spawn postcondition still answer
/// *"did the binary run?"* with a yes for a slow cold start. The two questions
/// stay two questions; only this one got its honest answer.
///
/// # Two deliberate shapes
///
/// The match is **exhaustive with no wildcard arm**. A wildcard is how the next
/// variant would silently inherit whichever side it happened to fall on; without
/// one, adding a [`SessionStatus`] breaks this build until someone decides, in
/// writing, whether that screen may be dispatched into.
///
/// [`SessionStatus::Dead`] cannot normally reach here — `wait_ready` converts a
/// dead session into `Err` — but its timeout path re-reads the pane, so the arm
/// is real and must stay [`Liveness::Dead`]: the caller's diagnostic for a
/// session that died is not the diagnostic for one that never settled.
fn dispatch_gate_liveness(status: &SessionStatus) -> Liveness {
    match status {
        SessionStatus::Ready | SessionStatus::Working => Liveness::Live,
        SessionStatus::Dead => Liveness::Dead,
        SessionStatus::TrustPrompt
        | SessionStatus::BypassPermsPrompt
        | SessionStatus::Loading
        | SessionStatus::Blocked
        | SessionStatus::AwaitingHuman
        | SessionStatus::Unknown => Liveness::Indeterminate,
    }
}

impl ClaudeTuiProbe {
    /// The same wait as [`LiveProbe::await_live`], returning the Claude-TUI
    /// verdict *beside* the collapsed one.
    ///
    /// # Why this exists
    ///
    /// A refusal that cannot name what it saw invites the reader to invent a
    /// cause. `cs tackle` used to print a hard-coded `(status=unknown)` in the
    /// same sentence that went on to describe a perfectly legible consent
    /// screen — the diagnostic contradicted itself because `await_live` had
    /// already thrown the name away. `SessionStatus` is `Display`, and
    /// `AwaitingHuman` renders as `awaiting-human`; the only thing missing was
    /// a way for the caller to receive it.
    ///
    /// # Why it does not re-open contract C0
    ///
    /// C0 is the separation of two *questions* — [`LiveProbe::observe`] asks
    /// "did the binary run?" and `await_live` asks "is it accepting work?" —
    /// not a rule about how much vocabulary crosses the boundary. Both
    /// questions are still answered here, by the same collapse, in one place:
    /// the `Liveness` half of the pair is computed exactly as before, and it
    /// remains the only value any caller branches on. The `SessionStatus` half
    /// is diagnostic payload, printed and never matched, so no call site can
    /// grow a second opinion about which panes may be dispatched into.
    ///
    /// It is an inherent method rather than a widening of [`LiveProbe`] for
    /// the same reason: the trait is the substrate-agnostic contract that an
    /// Aider REPL and a headless API worker also satisfy, and `SessionStatus`
    /// is Claude pane vocabulary. Pushing it into the trait would make every
    /// Adapter speak a language only one of them has.
    ///
    /// It carries no `self`: the probe is zero-sized, and the trait method
    /// above is the one that needs a receiver.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] if the session dies or cannot be queried —
    /// same conditions as [`wait_ready`], which this delegates to.
    pub fn await_live_with_status(
        backend: &dyn TransportBackend,
        worker_id: &WorkerId,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<(SessionStatus, Liveness), TransportError> {
        // `wait_ready` carries the Claude-TUI-specific handshake (it sends
        // Enter to dismiss the trust dialog and auto-accept permission
        // prompts). Mapping its rich verdict onto `Liveness` is the whole
        // job of this override.
        let status = wait_ready(backend, worker_id, timeout, poll_interval)?;
        let liveness = dispatch_gate_liveness(&status);
        // The verdict the caller will actually branch on, recorded beside the
        // pane verdict it was collapsed from. Read against the `capture` lines
        // above it, this line says in one place which screen the gate opened
        // for — the observation arm C could not make from outside the process.
        crate::readiness_trace::record(
            &crate::readiness_trace::Sample::new("dispatch_gate", worker_id.as_str())
                .status(&status)
                .liveness(&liveness),
        );
        Ok((status, liveness))
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
        // The idle REPL prompt, with the composer footer that identifies the
        // input line beside it as the composer's.
        let output = "some previous output\n\n  ? for shortcuts\n❯ ";
        assert_eq!(classify_output(output), SessionStatus::Ready);
    }

    /// A bare chevron is the shape of **any** empty input line, so it cannot
    /// certify a composer on its own. The paste-the-authorization-code field
    /// one step behind the login-method selector draws exactly this, and
    /// admitting it would shut one screen and re-open the corridor on the next.
    #[test]
    fn a_bare_chevron_without_composer_co_evidence_is_not_ready() {
        let paste_code_pane = " Paste the authorization code from your browser:
 (the code is shown after you approve the request)
 ❯
   Enter to submit · Esc to go back
";
        assert_eq!(
            classify_output(paste_code_pane),
            SessionStatus::AwaitingHuman,
            "a blocking free-text field is not a composer"
        );
    }

    /// The placeholder quoted in **body prose** is a sentence about the
    /// composer, not a composer. This test used to assert the opposite, and a
    /// green suite blessing it is why nothing flagged the whole-pane substring
    /// scan it depended on.
    #[test]
    fn test_classify_ready_type_message() {
        let banner = "Welcome to Claude Code!\n\nType your message to get started.\n";
        assert_ne!(
            classify_output(banner),
            SessionStatus::Ready,
            "a welcome banner mentioning the placeholder has no composer on it"
        );

        // On the input line, the same phrase IS the composer.
        assert_eq!(
            classify_output("Welcome to Claude Code!\n\n❯ Type your message\n"),
            SessionStatus::Ready
        );
    }

    /// Scrollback must not certify the present: a capture whose history holds
    /// a composer says nothing about the menu on screen now.
    #[test]
    fn a_composer_in_scrollback_does_not_certify_a_blocking_menu() {
        let pane = " ❯ Type your message
  (session reconnecting…)

 Your session needs to be re-authorised. Pick how to continue:
 ❯ 1. Re-authorise now
   2. Work offline
   3. Quit
   Enter to confirm
";
        assert_eq!(classify_output(pane), SessionStatus::AwaitingHuman);
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

    /// A real Claude Code v2.x bypass-permissions acceptance prompt, captured
    /// from the reproduction inside a Debian root container (noogram/cosmon#6).
    /// The folder-trust dialog exactly as Claude Code 2.1.220 renders it — the
    /// pane the container worker was found parked on.
    const TRUST_PANE: &str = r"
 Accessing workspace: /home/cosmon-worker/proj/.worktrees/task-20260725-fa33

 Quick safety check: Is this a project you created or one you trust?

 ❯ 1. Yes, I trust this folder
   2. No, exit

 Enter to confirm · Esc to cancel
";

    const BYPASS_PERMS_PANE: &str = r"
 WARNING: Claude Code running in Bypass Permissions mode

 By proceeding, you accept all responsibility for actions taken while running
 in Bypass Permissions mode.

 ❯ 1. No, exit
   2. Yes, I accept

 Enter to confirm · Esc to cancel
";

    #[test]
    fn test_classify_bypass_perms_prompt() {
        assert_eq!(
            classify_output(BYPASS_PERMS_PANE),
            SessionStatus::BypassPermsPrompt
        );
    }

    #[test]
    fn test_bypass_perms_takes_priority_over_blocked() {
        // The prompt shares the `Esc to cancel` footer with a generic yes/no
        // dialog. It must NOT be classified as Blocked — a Blocked verdict
        // makes wait_ready send a bare Enter, which selects the highlighted
        // `1. No, exit` and kills the worker.
        assert_ne!(
            classify_output(BYPASS_PERMS_PANE),
            SessionStatus::Blocked,
            "bypass-perms prompt mis-classified as Blocked — a bare Enter would exit the worker"
        );
    }

    #[test]
    fn test_bypass_perms_takes_priority_over_ready() {
        // The prompt renders the menu chevron `❯` on `❯ 1. No, exit`, which
        // the last-lines scan would otherwise read as Ready.
        assert_eq!(
            classify_output(BYPASS_PERMS_PANE),
            SessionStatus::BypassPermsPrompt
        );
    }

    #[test]
    fn test_is_bypass_perms_prompt_requires_both_markers() {
        // A passing mention of bypass mode in scrollback (no accept option) is
        // not the live prompt and must not trip the detector.
        assert!(!is_bypass_perms_prompt(
            "running in Bypass Permissions mode\n❯ "
        ));
        // The accept phrase without the warning banner is likewise not enough.
        assert!(!is_bypass_perms_prompt("2. Yes, I accept the terms\n"));
        assert!(is_bypass_perms_prompt(BYPASS_PERMS_PANE));
    }

    #[test]
    fn test_bypass_perms_liveness_is_live() {
        // The worker parked at the accept prompt is alive — the probe must not
        // tear it down as a failed spawn before the handshake answers it.
        assert_eq!(SessionStatus::BypassPermsPrompt.liveness(), Liveness::Live);
    }

    #[test]
    fn test_wait_ready_sends_accept_on_bypass_prompt() {
        use crate::mock::MockCall;
        use crate::MockBackend;

        let backend = MockBackend::new();
        let config = cosmon_core::transport::RuntimeConfig::default();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("test-bypass").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend.spawn(&agent, &config).unwrap();

        // The pane stays parked at the bypass prompt for the whole window, so
        // wait_ready times out returning the last observed status — but along
        // the way it MUST answer the prompt with `2` (never a bare Enter).
        backend.set_canned_output(BYPASS_PERMS_PANE);

        let status = wait_ready(
            &backend,
            &worker.id,
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .unwrap();

        assert_eq!(status, SessionStatus::BypassPermsPrompt);

        let calls = backend.calls();
        let accept_count = calls
            .iter()
            .filter(|c| matches!(c, MockCall::SendInput { input, .. } if input == "2"))
            .count();
        // The invariant is *bounded*, not *once*. The one-shot latch this
        // replaced made a single swallowed keystroke unrecoverable (issue #20:
        // a cold container drops the first send-keys and the pane sits on the
        // question for the rest of the window). What must never happen is
        // unbounded hammering for the whole timeout, so the bound is what is
        // pinned here — with at least one answer actually sent.
        assert!(
            (1..=MODAL_ANSWER_ATTEMPTS as usize).contains(&accept_count),
            "expected between 1 and {MODAL_ANSWER_ATTEMPTS} `2` accept keystrokes, got \
             {accept_count}"
        );
        // And crucially: no bare-Enter (empty input) was sent, which would
        // have selected `1. No, exit`.
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, MockCall::SendInput { input, .. } if input.is_empty())),
            "wait_ready sent a bare Enter on the bypass prompt — would select `No, exit`"
        );
    }

    /// Issue #20, the silent half of the container hang — frozen.
    ///
    /// A pane parked on a startup modal for the whole readiness window used to
    /// come back from `await_live` as `Liveness::Live`, because
    /// `SessionStatus::TrustPrompt.liveness()` is `Live` (correctly, for the
    /// *spawn postcondition*: a rendered dialog proves the binary ran). The
    /// tackle caller reads that verdict as "accepting work" and immediately
    /// types the briefing into a two-option menu. Nothing errors, nothing logs,
    /// the molecule stays `running`, and the operator sees a healthy worker that
    /// will never produce a token — the report's "worker waits indefinitely".
    ///
    /// Both modals are asserted: they reach the mapping through different
    /// handshake arms, and the trust dialog is the one the tester actually hit.
    #[test]
    fn await_live_refuses_a_worker_still_parked_on_a_startup_modal() {
        use super::LiveProbe as _;
        use crate::mock::MockBackend;

        for (label, pane) in [("trust", TRUST_PANE), ("bypass", BYPASS_PERMS_PANE)] {
            let backend = MockBackend::new();
            let config = cosmon_core::transport::RuntimeConfig::default();
            let agent = cosmon_core::transport::AgentDefinition {
                id: cosmon_core::id::AgentId::new("test-modal").unwrap(),
                role: cosmon_core::agent::AgentRole::Implementation,
                command: "echo".to_owned(),
                args: vec![],
            };
            let worker = backend.spawn(&agent, &config).unwrap();
            backend.set_canned_output(pane);

            let verdict = ClaudeTuiProbe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_millis(300),
                    Duration::from_millis(50),
                )
                .expect("probe queries the backend fine");

            assert_eq!(
                verdict,
                Liveness::Indeterminate,
                "a worker still parked on the {label} modal must not be reported Live — \
                 that is what let tackle type the briefing into the dialog"
            );
        }
    }

    /// The companion property: the *postcondition* probe must keep reading a
    /// rendered modal as `Live`. The two questions are different — "did the
    /// binary run?" versus "is it accepting work?" — and collapsing them would
    /// turn a slow-but-fine cold start into a torn-down spawn.
    #[test]
    fn observe_still_counts_a_startup_modal_as_proof_of_life() {
        use super::LiveProbe as _;
        use crate::mock::MockBackend;

        let backend = MockBackend::new();
        let config = cosmon_core::transport::RuntimeConfig::default();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("test-modal-observe").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend.spawn(&agent, &config).unwrap();
        backend.set_canned_output(TRUST_PANE);

        assert_eq!(
            ClaudeTuiProbe
                .observe(&backend, &worker.id)
                .expect("observe"),
            Liveness::Live
        );
    }

    /// Issue #20, door 4 — VERBATIM capture from the versioned bench, arm C:
    /// a virgin `CLAUDE_CONFIG_DIR` with both consent keys pre-granted and a
    /// placeholder credential present. This is evidence, not a reconstruction.
    const LOGIN_SELECTOR_PANE: &str = r" Select login method:
 ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise
   2. Anthropic Console account · API usage billing
   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI
";

    /// Issue #20, door 4 — the screen the process ACTUALLY saw. VERBATIM from
    /// the instrumented bench run of 2026-07-25, arm C, captured by
    /// `readiness_trace` from inside `cs tackle`'s own readiness loop: every
    /// one of the ~60 samples in the 30 s window classified this pane, and none
    /// of them ever saw the login-method selector.
    ///
    /// The distinction is the whole finding. `LOGIN_SELECTOR_PANE` is what the
    /// bench captured *after* tackle returned; this is what the gate decided
    /// on. Both are real, they are different screens, and every explanation
    /// built on the first one was reasoning about a pane the process never
    /// classified.
    const FIRST_RUN_THEME_WIZARD_PANE: &str = r"Welcome to Claude Code v2.1.220

 Let's get started.

 Choose the text style that looks best with your terminal
 To change this later, run /theme

   1. Auto (match terminal)
 ❯ 2. Dark mode ✔
   3. Light mode
   4. Dark mode (colorblind-friendly)
   5. Light mode (colorblind-friendly)
   6. Dark mode (ANSI colors only)
   7. Light mode (ANSI colors only)

  Syntax theme: Monokai Extended (ctrl+t to disable)
";

    /// An **invented** onboarding screen matching no marker in [`markers`].
    /// Deliberately not a real Claude screen: the property under test is that
    /// the build refuses a menu it has never seen. If a future marker ever
    /// claims this pane, replace the pane — never the assertion.
    const UNNAMED_MENU_PANE: &str = r" Pick a starting workspace layout:
 ❯ 1. Split panes
   2. Single pane
   3. Decide later
   Enter to confirm · Esc to go back
";

    /// The composer as the tester's container painted it — the one pane that
    /// legitimately means "accepting work".
    const COMPOSER_PANE: &str = r"
  ⏵⏵ bypass permissions on (shift+tab to cycle)     Not logged in · Run /login
 ❯ Type your message
";

    /// **The named door.** A pane parked on Claude Code's login-method
    /// selector is a pane waiting for a human. Before the fix it classified
    /// `Ready`, so `cs tackle` typed the briefing into a menu, exited 0, and
    /// the molecule stayed `running` forever with nothing reporting a fault.
    #[test]
    fn login_method_selector_is_not_ready() {
        assert_eq!(
            classify_output(LOGIN_SELECTOR_PANE),
            SessionStatus::AwaitingHuman,
            "the login-method selector is a menu awaiting a human, not a composer"
        );
    }

    /// **The closed default — the load-bearing one.** Shutting door 4 by
    /// adding `Select login method` to [`markers`] would turn the test above
    /// green and leave this one red, and the next unnamed screen would re-open
    /// the identical door. That is precisely how it was opened three times
    /// already (`TRUST_PROMPT`, `BYPASS_PERMS_WARNING`, `FIRST_RUN_THEME`).
    #[test]
    fn an_unrecognised_menu_is_not_ready() {
        assert_eq!(
            classify_output(UNNAMED_MENU_PANE),
            SessionStatus::AwaitingHuman,
            "a menu matching NO marker must not become Ready merely by drawing a chevron"
        );
    }

    /// The other half of contract C0, for the class the fix actually changed.
    /// The frozen harness guards C0 with the *named* trust dialog, which routes
    /// to `TrustPrompt` and stays `Live` whatever the default does — so it
    /// cannot see a closed default leaking into `observe`. This can.
    #[test]
    fn observe_still_counts_an_unnamed_rendered_screen_as_proof_of_life() {
        use super::LiveProbe as _;
        use crate::mock::MockBackend;

        for (label, pane) in [
            ("login-selector", LOGIN_SELECTOR_PANE),
            ("unnamed-menu", UNNAMED_MENU_PANE),
        ] {
            let backend = MockBackend::new();
            let agent = cosmon_core::transport::AgentDefinition {
                id: cosmon_core::id::AgentId::new("observe-unnamed").unwrap(),
                role: cosmon_core::agent::AgentRole::Implementation,
                command: "echo".to_owned(),
                args: vec![],
            };
            let worker = backend
                .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
                .unwrap();
            backend.set_canned_output(pane);

            assert_eq!(
                ClaudeTuiProbe.observe(&backend, &worker.id).unwrap(),
                Liveness::Live,
                "observe refused the rendered {label} screen — the spawn \
                 postcondition and the dispatch gate have collapsed into one \
                 question, and a slow cold start painting an unnamed first \
                 frame is now a torn-down spawn"
            );
        }
    }

    /// The dispatch verdict for both panes — the *composed* decision
    /// `cs tackle` actually makes, not the classifier's enum. A refactor that
    /// renamed the enum while still dispatching would pass the two tests above
    /// and fail this one.
    #[test]
    fn await_live_refuses_a_worker_parked_on_a_menu() {
        use super::LiveProbe as _;
        use crate::mock::MockBackend;

        for (label, pane) in [
            ("login-selector", LOGIN_SELECTOR_PANE),
            ("unnamed-menu", UNNAMED_MENU_PANE),
        ] {
            let backend = MockBackend::new();
            let agent = cosmon_core::transport::AgentDefinition {
                id: cosmon_core::id::AgentId::new("test-menu").unwrap(),
                role: cosmon_core::agent::AgentRole::Implementation,
                command: "echo".to_owned(),
                args: vec![],
            };
            let worker = backend
                .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
                .unwrap();
            backend.set_canned_output(pane);

            // Guard: a worker reading `Dead` would make the assertion below
            // pass vacuously — the harness would be broken, not the build.
            assert_ne!(
                ClaudeTuiProbe.observe(&backend, &worker.id).unwrap(),
                Liveness::Dead,
                "the {label} mock worker is not even alive"
            );

            assert_eq!(
                ClaudeTuiProbe
                    .await_live(
                        &backend,
                        &worker.id,
                        Duration::from_millis(300),
                        Duration::from_millis(50),
                    )
                    .unwrap(),
                Liveness::Indeterminate,
                "await_live reported Live for a pane parked on the {label} menu"
            );
        }
    }

    /// **The bench-red test.** Issue #20, door 4 — the one that was green at
    /// every other layer while the container dispatched anyway.
    ///
    /// The pane is the first-run theme wizard, verbatim from the instrumented
    /// arm C run, and the mock never changes it — which is exactly what the
    /// container did for the whole 30 s window. `classify_output` calls it
    /// `Loading` (correct: a wizard on screen IS a cold start in progress),
    /// `wait_ready` exhausts its window and hands that `Loading` back, and the
    /// gate used to collapse it to `Live` because `Loading` was simply not on
    /// the deny-list. That is how an 80-line briefing got typed into an
    /// onboarding wizard.
    ///
    /// If this ever goes green by someone adding a marker for the wizard,
    /// the fix has been undone: the property under test is that a status
    /// arriving by TIMEOUT is refused whatever it is called, not that this
    /// particular screen is recognised.
    #[test]
    fn await_live_refuses_a_worker_still_on_the_first_run_wizard_when_the_window_closes() {
        use super::LiveProbe as _;
        use crate::mock::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("test-wizard").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output(FIRST_RUN_THEME_WIZARD_PANE);

        // The premise, asserted rather than assumed: this pane really is the
        // `Loading` the deny-list waved through. If a future build reclassifies
        // it, this test would otherwise keep passing for the wrong reason.
        assert_eq!(
            classify_output(FIRST_RUN_THEME_WIZARD_PANE),
            SessionStatus::Loading,
            "the wizard is a cold start in progress — that classification is not the bug"
        );

        // C0's half: the spawn postcondition must still call it alive. A
        // refusal here would tear down every slow cold start, which is the
        // over-correction this contract exists to prevent.
        assert_eq!(
            ClaudeTuiProbe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "a painted wizard is still proof the binary ran"
        );

        // C1's half: the dispatch gate must refuse it.
        assert_eq!(
            ClaudeTuiProbe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_millis(300),
                    Duration::from_millis(50),
                )
                .unwrap(),
            Liveness::Indeterminate,
            "await_live certified a worker parked on the first-run wizard as accepting work"
        );
    }

    /// The gate's rule stated as a rule, not as a list of screens.
    ///
    /// `wait_ready` returns `Ready` / `Working` on sight and everything else
    /// only by running out of window, so "arrived as evidence" and "is `Ready`
    /// or `Working`" are the same set. This walks every `SessionStatus` and
    /// pins the collapse, so a future edit that re-opens one arm — the shape
    /// every previous door-4 regression took — fails here by name.
    #[test]
    fn only_ready_and_working_open_the_dispatch_gate() {
        for (status, expected) in [
            (SessionStatus::Ready, Liveness::Live),
            (SessionStatus::Working, Liveness::Live),
            (SessionStatus::Dead, Liveness::Dead),
            (SessionStatus::Loading, Liveness::Indeterminate),
            (SessionStatus::TrustPrompt, Liveness::Indeterminate),
            (SessionStatus::BypassPermsPrompt, Liveness::Indeterminate),
            (SessionStatus::Blocked, Liveness::Indeterminate),
            (SessionStatus::AwaitingHuman, Liveness::Indeterminate),
            (SessionStatus::Unknown, Liveness::Indeterminate),
        ] {
            assert_eq!(
                dispatch_gate_liveness(&status),
                expected,
                "the dispatch gate changed its mind about {status}"
            );
        }
    }

    /// The price-of-the-fix guard: closing the default must not cost the
    /// composer. If this goes red the build refuses every healthy worker.
    #[test]
    fn the_composer_is_still_ready() {
        assert_eq!(classify_output(COMPOSER_PANE), SessionStatus::Ready);
        // The idle REPL prompt under the composer's own footer.
        assert_eq!(
            classify_output("earlier output\n\n ⏵⏵ bypass permissions on\n ❯ "),
            SessionStatus::Ready
        );
        // The idle REPL prompt inside the composer's box frame — boxed *and*
        // under the footer. The box is decoration here; the footer is what
        // certifies it, which is why the same box without a footer is refused
        // by `a_bare_box_frame_is_not_composer_evidence` below.
        assert_eq!(
            classify_output("│ ❯                 │\n│ ? for shortcuts   │\n"),
            SessionStatus::Ready
        );
    }

    /// The mirror of the closed default. A healthy composer holding a
    /// suggestion, a reworded placeholder or a localised one still means
    /// "accepting work": the refusal is keyed on menu *shape*, not on "the
    /// chevron has content". Keying it the other way buys the door with a fleet
    /// that refuses every worker whose vendor UI string moved.
    #[test]
    fn a_composer_showing_a_suggestion_is_still_ready() {
        for pane in [
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)\n ❯ Try \"fix the failing test\"\n",
            "  ? for shortcuts\n ❯ Écris ton message\n",
            "│ ⏵⏵ bypass permissions on │\n│ ❯ Schreib deine Nachricht │\n",
        ] {
            assert_eq!(
                classify_output(pane),
                SessionStatus::Ready,
                "refused a healthy composer:\n{pane}"
            );
        }
    }

    /// The corridor this module exists to shut, re-opened once already by
    /// offering the box frame as composer co-evidence.
    ///
    /// This TUI boxes its modals as readily as its composer, so a frame
    /// certifies nothing. Each pane below is a *blocking* screen — a menu whose
    /// options are lettered, a paste-the-authorization-code field, a bare boxed
    /// input line — drawn exactly the way the composer is drawn, minus the
    /// composer's own footer and placeholder. All must be refused.
    #[test]
    fn a_bare_box_frame_is_not_composer_evidence() {
        for (label, pane) in [
            ("bare boxed input line", "│ ❯                 │\n"),
            (
                "boxed lettered menu",
                " How would you like to continue?\n \
                 ╭───────────────────────────╮\n \
                 │ ❯ a) Re-authorise now     │\n \
                 │   b) Work offline         │\n \
                 ╰───────────────────────────╯\n",
            ),
            (
                "boxed paste-the-code field",
                " Paste the authorization code from your browser:\n \
                 ╭───────────────────────────╮\n \
                 │ ❯                         │\n \
                 ╰───────────────────────────╯\n   Enter to submit\n",
            ),
        ] {
            assert_eq!(
                classify_output(pane),
                SessionStatus::AwaitingHuman,
                "a box frame certified the {label} as a composer:\n{pane}"
            );
        }
    }

    /// The rule sandwich is what makes a 2.1.220 composer legible, and it must
    /// not become a way for a *menu* to be legible as one.
    ///
    /// A modal ruled top and bottom is a plausible screen for this TUI to draw,
    /// and the only thing standing between it and `Ready` is that its chevron
    /// rests on an option. That is the same guard rule (2) relies on; this pins
    /// that rule (3) inherited it rather than opening a third path.
    #[test]
    fn a_ruled_menu_is_not_a_ruled_composer() {
        let pane = " Choose a login method\n\
                    ────────────────────────────────────────\n\
                    ❯ 1. Claude account\n\
                    ────────────────────────────────────────\n";
        assert_eq!(
            classify_output(pane),
            SessionStatus::AwaitingHuman,
            "a menu cursor between two rules was certified as a composer:\n{pane}"
        );
    }

    /// The 2.1.220 composer, reduced to the arrangement that identifies it.
    ///
    /// Neither footer marker is present — this is the manual-mode pane, whose
    /// `⏸ manual mode on` footer matches nothing in [`markers`]. Before rule (3)
    /// that made the whole session undispatchable.
    #[test]
    fn a_ruled_input_line_is_composer_evidence() {
        let pane = "  20 — vingt : deux dizaines.\n\
                    ────────────────────────────────────────\n\
                    ❯ \n\
                    ────────────────────────────────────────\n\
                    \n  ⏸ manual mode on · ← 1 agent\n";
        assert_eq!(classify_output(pane), SessionStatus::Ready);
    }

    /// A box border is drawn from the same glyph as a rule, and it is not one.
    ///
    /// `╭───╮` would satisfy a naive "does this line contain rule characters?"
    /// test, and satisfying it top and bottom is exactly what a boxed modal
    /// does. [`is_horizontal_rule_line`] demands the line be *nothing but* rule.
    #[test]
    fn a_box_border_is_not_a_horizontal_rule() {
        assert!(!is_horizontal_rule_line("╭───────────────╮"));
        assert!(!is_horizontal_rule_line("│───────────────│"));
        assert!(!is_horizontal_rule_line("──── Tips ──────"));
        assert!(!is_horizontal_rule_line("───"));
        assert!(is_horizontal_rule_line("  ────────────────  "));
    }

    /// The status slot holds one of three things, and only one of them means a
    /// turn is running.
    ///
    /// `◐ medium · /effort` is what an *idle* 2.1.220 pane parks there, and
    /// `✻ Baked for 16s` is what a *finished* turn leaves there until the next
    /// one starts. Admitting either would report `Working` for a pane that has
    /// been idle for hours — and would let the briefing-submit loop call a
    /// briefing delivered on the strength of the previous turn's leftovers.
    #[test]
    fn only_a_running_clock_counts_as_work_in_flight() {
        for idle_slot in [
            "◐ medium · /effort",
            "✻ Baked for 16s",
            "✻ Baked for 4m 12s",
        ] {
            let pane = format!(
                "{idle_slot}\n\
                 ────────────────────────────────────────\n\
                 ❯ \n\
                 ────────────────────────────────────────\n"
            );
            assert_eq!(
                classify_output(&pane),
                SessionStatus::Ready,
                "a status slot that is not running a clock was read as work in flight:\n{pane}"
            );
        }

        for running_slot in [
            "✢ Coalescing… (3s · thinking with medium effort)",
            "✻ Cogitating… (41s · ↑ 1.2k tokens · esc to interrupt)",
            "✽ Working… (esc to interrupt)",
        ] {
            let pane = format!(
                "{running_slot}\n\
                 ────────────────────────────────────────\n\
                 ❯ \n\
                 ────────────────────────────────────────\n"
            );
            assert_eq!(
                classify_output(&pane),
                SessionStatus::Working,
                "a running clock above the composer was read as idle:\n{pane}"
            );
        }
    }

    /// Work evidence outranks the composer, and that reordering must not undo
    /// what checking the composer first was protecting.
    ///
    /// The original order existed to stop a `⏺` left in scrollback from
    /// reporting `Working` over an idle pane. The guard survives the reorder
    /// because the new evidence is a *running clock*, which a finished turn does
    /// not leave behind — not because the composer still wins.
    #[test]
    fn stale_tool_use_still_does_not_outrank_a_ruled_composer() {
        let pane = "⏺ Read(src/lib.rs)\n\
                    ⏺ Done.\n\
                    ✻ Baked for 16s\n\
                    ────────────────────────────────────────\n\
                    ❯ \n\
                    ────────────────────────────────────────\n";
        assert_eq!(classify_output(pane), SessionStatus::Ready);
    }

    /// [`is_menu_option_line`] used to key on exactly two characters, so `10.`,
    /// `a)` and `• ` were all invisible to it — and an option shape it cannot
    /// see is a menu cursor promoted to composer evidence.
    #[test]
    fn menu_option_shapes_wider_than_two_characters_are_still_menus() {
        for rest in [
            "1. Split panes",
            "10. Decide later",
            "a) Re-authorise",
            "• Quit",
        ] {
            assert!(
                is_menu_option_shape(rest),
                "menu option shape not recognised: {rest}"
            );
        }
        // A composer holding a draft is not a menu, whatever it starts with.
        for rest in [
            "",
            "Type your message",
            "fix the failing test",
            "- fix the test",
        ] {
            assert!(
                !is_menu_option_shape(rest),
                "composer content misread as a menu option: {rest}"
            );
        }
    }

    /// Scrollback must not certify the present through the `Blocked` arm
    /// either — it runs *before* the composer evidence rule, so a whole-capture
    /// match there wins outright over whatever the pane is painting now.
    #[test]
    fn a_stale_permission_question_does_not_block_the_current_screen() {
        let pane = " ⏺ Bash(cargo test)\n   Do you want to proceed?\n   1. Yes  2. No\n \
                    ⏺ done\n\n Your session needs to be re-authorised. Pick how to continue:\n \
                    ❯ 1. Re-authorise now\n   2. Work offline\n   Enter to confirm\n";
        assert_eq!(classify_output(pane), SessionStatus::AwaitingHuman);
    }

    /// `Blocked` is `Live` for the spawn postcondition (a rendered dialog is
    /// proof the binary ran) and `Indeterminate` at the dispatch gate (a
    /// question that would not clear is not a worker accepting a briefing).
    /// Without the second half, any unnamed menu wearing the boilerplate
    /// `Esc to cancel` footer walked through the gate and the whole composer
    /// evidence rule was never consulted.
    #[test]
    fn await_live_refuses_a_pane_still_blocked_when_the_window_closes() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("blocked-pane").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output(
            " Pick a starting workspace layout:\n ❯ 1. Split panes\n   2. Single pane\n   \
             Enter to confirm · Esc to cancel\n",
        );

        let probe = ClaudeTuiProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "the pane painted a frame — the spawn postcondition must still say Live"
        );
        assert_eq!(
            probe
                .await_live(
                    &backend,
                    &worker.id,
                    Duration::from_millis(300),
                    Duration::from_millis(50),
                )
                .unwrap(),
            Liveness::Indeterminate,
            "await_live dispatched into an unnamed menu whose footer says `Esc to cancel`"
        );
    }

    /// The refusal a caller prints must be able to name what the probe saw.
    /// `await_live` alone collapses `awaiting-human` to `indeterminate`, which
    /// is why `cs tackle` used to print a hard-coded `(status=unknown)` beside
    /// a description of a screen it had in fact recognised.
    #[test]
    fn await_live_with_status_keeps_the_name_of_the_screen_it_refused() {
        use crate::MockBackend;

        let backend = MockBackend::new();
        let agent = cosmon_core::transport::AgentDefinition {
            id: cosmon_core::id::AgentId::new("named-refusal").unwrap(),
            role: cosmon_core::agent::AgentRole::Implementation,
            command: "echo".to_owned(),
            args: vec![],
        };
        let worker = backend
            .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
            .unwrap();
        backend.set_canned_output(UNNAMED_MENU_PANE);

        let (status, liveness) = ClaudeTuiProbe::await_live_with_status(
            &backend,
            &worker.id,
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .unwrap();
        assert_eq!(status, SessionStatus::AwaitingHuman);
        assert_eq!(liveness, Liveness::Indeterminate);
        assert_eq!(
            status.to_string(),
            "awaiting-human",
            "the operator reads this string in the refusal"
        );
    }

    /// A painted frame with no chevron in it is still a painted frame. The
    /// spawn postcondition asks "did the binary run?", and answering `Unknown`
    /// for a rendered login screen whose field uses `>` costs the operator a
    /// true diagnostic. The dispatch gate refuses it either way.
    #[test]
    fn a_rendered_frame_without_a_chevron_is_not_nothing() {
        let pane = " Open this URL to finish signing in:\n \
                    ╭───────────────────────────╮\n \
                    │ https://example.invalid/x │\n \
                    ╰───────────────────────────╯\n";
        assert_eq!(classify_output(pane), SessionStatus::AwaitingHuman);
        assert_eq!(
            SessionStatus::AwaitingHuman.liveness(),
            Liveness::Live,
            "something painted that frame"
        );
        // And nothing recognisable is still nothing.
        assert_eq!(
            classify_output("some random text\n"),
            SessionStatus::Unknown
        );
    }

    /// Scrollback must not speak over the current screen: a `⏺` left from an
    /// earlier turn cannot report `Working` while a question is on screen now.
    /// Without this the corridor re-opens for any menu that happens to follow
    /// a tool call.
    #[test]
    fn stale_tool_use_does_not_promote_a_menu_to_working() {
        let pane = format!("⏺ Read(config.toml)\n⏺ Bash(ls)\n{UNNAMED_MENU_PANE}");
        assert_eq!(classify_output(&pane), SessionStatus::AwaitingHuman);
    }

    /// The same rule, doing its other job. A composer still holding an
    /// unsubmitted pasted briefing must not read `Working`: it is the exact
    /// pane the re-`Enter` nudges exist to rescue (the 2026-07-20
    /// paste-sans-submit stall), and calling it a worker that started is how a
    /// patrol pass would walk past it.
    #[test]
    fn a_pasted_briefing_is_not_a_worker_that_started_working() {
        let pane = "⏺ Reading files...\n ❯ [Pasted text #1 +86 lines]\n";
        assert_ne!(classify_output(pane), SessionStatus::Working);
        assert_ne!(classify_output(pane), SessionStatus::Ready);
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
        backend.set_canned_output("Welcome!\n\n❯ Type your message");

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
        let ready_output = "❯ Type your message";
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
        // A painted frame parked on a question is still a painted frame: the
        // spawn postcondition (C0) is entitled to read it as proof of life.
        assert_eq!(SessionStatus::AwaitingHuman.liveness(), Liveness::Live);
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
        backend.set_canned_output("Welcome!\n\n❯ Type your message");

        let probe = ClaudeTuiProbe;
        assert_eq!(
            probe.observe(&backend, &worker.id).unwrap(),
            Liveness::Live,
            "a composer pane is positive evidence of liveness"
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
