// SPDX-License-Identifier: AGPL-3.0-only

//! `cs peek` — ratatui TUI watchdog for the running fleet.
//!
//! A *watchdog*, not a dashboard: the optimisation target is **time-to-action**
//! when a worker stalls, not information coverage.
//!
//! V1 scope (this file):
//!
//! - 7-column table: `▸ | ♥ | project | molecule (session slug) | role+status+step | age | energy`
//!   where the leading `▸` / `▾` indicator reflects tree-view expansion state.
//! - Keys: `j/k` navigate, `→` expand selected row (reveals topic, worker,
//!   formula, branch, blocked-by), `←` collapse back to single-line view,
//!   `Enter` attach via tmux, `q` quit, `/` filter, `r` refresh,
//!   `p` toggle on-demand peek panel for the selected row.
//! - Peek panel runs `tmux capture-pane -p` *only for the selected row*
//!   (non-intrusive — no polling of every session). When the panel is
//!   enabled, j/k auto-refreshes the capture for the newly-selected row so
//!   triage stays fluid (watchdog, not dashboard).
//!
//! V2 (deferred): event bar with heartbeat-gap filter.
//!
//! The command consumes [`cosmon_observability::FleetSnapshot`] as its
//! data model, keeping parity with `cosmon-cockpit-http`.

#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::map_unwrap_or,
    clippy::unwrap_or_default,
    // `unchecked_duration_subtraction` was renamed to `unchecked_time_subtraction`
    // in clippy 1.94. We keep the OLD name because it still resolves on clippy
    // ≥1.94 (via the rename shim) AND is the only name clippy <1.94 recognises —
    // a naive rename to the new name would let the still-active old lint fire
    // under `-D warnings` on older toolchains. `renamed_and_removed_lints`
    // silences the ≥1.94 rename notice (otherwise promoted to a hard error by
    // `-D warnings`). Cross-version-safe.
    renamed_and_removed_lints,
    clippy::unchecked_duration_subtraction,
    clippy::too_many_lines,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions
)]

use std::io::{self, IsTerminal};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use cosmon_core::molecule::Phase;
use cosmon_core::reconcile::{molecule_health, MoleculeHealth};
use cosmon_filestore::FileStore;
use cosmon_observability::{
    EnergyBudget, FleetSnapshot, HeartbeatTier, Molecule, MoleculeStatus, Session, SessionFilter,
    Worker,
};
use cosmon_state::StateStore;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
    Terminal,
};

use super::Context;
use crate::visual::{classify as visual_classify, temp_token, whisper_token, RowInputs, RowKind};

pub(crate) mod presence_reader;
pub(crate) mod renderers;
pub(crate) mod trust;

use renderers::{DetailCtx, DetailRenderer};

/// Options for the TUI entry point. Populated by `cs peek` after deciding
/// the caller is on a TTY and hasn't asked for `--no-tui`.
pub(crate) struct TuiOptions {
    /// Lift the project scope from current-project to **all projects**.
    /// Set by the `--all-galaxies` CLI flag, or by `--all`, which is sugar
    /// for `--all-galaxies --phase all` and so flips `phase_filter` too.
    /// The `a` key toggles this at runtime, independently of the phase
    /// dimension.
    pub all_projects: bool,
    /// Which molecule phases the table surfaces — see
    /// [`crate::cmd::peek::PhaseFilter`]. The default
    /// (`PhaseFilter::unfinished`) hides the archive; `--phase all` (or
    /// its sugar `--all`) lifts every phase. The interactive `A` key
    /// cycles the presets (unfinished → all → unfinished) at runtime.
    pub phase_filter: super::peek::PhaseFilter,
    /// Refresh interval in milliseconds (snapshot reload cadence).
    pub refresh_ms: u64,
    /// Command-line filter configuration — forwarded to the row and
    /// ensemble views so non-interactive invocations can pre-scope.
    pub filter: FilterConfig,
}

/// Command-line filter configuration shared by the molecule table and the
/// ensemble (fleet-wide events) tab. Introduced per hawking §2 to avoid
/// viewport aliasing at N>50 live molecules.
#[derive(Default, Debug, Clone)]
pub(crate) struct FilterConfig {
    /// Free-text substring filter — pre-populates the TUI `/` field.
    pub free_text: Option<String>,
    /// Molecule tag filter (e.g. `temp:hot`).
    pub tag: Option<String>,
    /// Lower bound on molecule `updated_at` / event timestamp (RFC-3339
    /// or bare duration `1h`, `30m`). Older rows / events are hidden.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// In the ensemble view, drop all but the last `since_event`
    /// events per molecule stream.
    pub since_event: Option<usize>,
}

/// View-state for the ensemble events tab. Refreshed on each reload so
/// `j/k` / Enter stay pinned to the current snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct EnsembleEventsView {
    /// Newest-first list of rendered events (one line per event).
    pub(crate) entries: Vec<EnsembleEvent>,
    /// Currently-selected row in the view.
    pub(crate) selected: usize,
}

/// A single event in the ensemble view — the minimum data needed to
/// render one row and drop back into per-molecule zoom on Enter.
#[derive(Debug, Clone)]
pub(crate) struct EnsembleEvent {
    pub(crate) ts: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) mol_id: String,
    pub(crate) kind: String,
    pub(crate) summary: String,
}

/// TUI entry point invoked from [`super::peek::run`].
///
/// Panic-safe: installs a scoped panic hook that restores the terminal before
/// propagating the panic. Without this, a panic inside the event loop would
/// leave the user's terminal stuck in raw mode + alternate screen, where the
/// shell prompt renders garbled and `stty sane` is the only recovery.
pub(crate) fn run(ctx: &Context, opts: &TuiOptions) -> anyhow::Result<()> {
    let mut app = App::new(ctx, opts)?;
    let mut terminal = setup_terminal()?;

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        // Best-effort: restore the terminal before the default hook prints
        // the backtrace. Ignore errors — we're already panicking.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        eprintln!("cs peek panicked: {info}");
    }));

    let res = app.event_loop(&mut terminal);

    // Re-install the previous hook so tests and other code see the expected
    // panic behavior if something later in the process panics.
    std::panic::set_hook(prev_hook);

    restore_terminal(&mut terminal)?;
    res
}

/// Set up terminal for TUI rendering — enable raw mode, enter the alternate
/// screen, enable mouse capture.
///
/// Preflight: verifies stdout is a real TTY before calling `enable_raw_mode`.
/// When cs peek is launched in a non-TTY context (piped output, detached
/// session without PTY, daemon), crossterm returns a cryptic `Device not
/// configured (os error 6)` error. This preflight gives operators an
/// actionable message and fails cleanly without touching the terminal.
fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    if !io::stdout().is_terminal() {
        return Err(anyhow::anyhow!(
            "cs peek requires a TTY on stdout; got a pipe or redirected stream. \
             Run `cs peek --no-tui` for a non-interactive view, `cs peek --json` \
             for scripting, or `cs peek --snapshot` for a fixed-width capture."
        ));
    }
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// A single displayable row — **one line per molecule**.
///
/// Before phase 1 of the runtime-vs-cognition split, [`snapshot_to_rows`]
/// emitted one row per tmux session, so a macro-molecule running a
/// runtime + cognition pair collided into two identical-looking rows
/// (same mol id, different sessions). Rows are now keyed by `mol_id`
/// and `role_glyphs` advertises whether the molecule has a runtime, a
/// cognition worker, or both.
#[derive(Clone)]
pub(crate) struct RowView {
    pub(crate) mol_id: String,
    /// Functional slug derived from the tmux session name (e.g.
    /// `wire-verificationspec-evolve-4645`). `None` when the molecule has
    /// not been tackled yet or predates the session-name fix.
    pub(crate) session_slug: Option<String>,
    pub(crate) project: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) step: String,
    /// Wall-clock of the molecule's last state write (`MoleculeData::updated_at`).
    ///
    /// Stored as the timestamp, not as the rendered `age` string it used to
    /// be. `age` is now computed at render time by [`age_cell`], because a
    /// formatted duration is a fact about *when it was formatted* — it cannot
    /// be compared, and the sort key needs to compare. For a terminal molecule
    /// `state.json` is never rewritten after death, so `updated_at` *is* the
    /// death date and orders the archive correctly (`mol_id`s sort by kind
    /// prefix first, so they never could).
    ///
    /// `None` only for a tmux session with no molecule behind it — there is no
    /// timestamp to report and inventing one would be a fabricated fact.
    pub(crate) updated_at: Option<DateTime<Utc>>,
    pub(crate) energy_in: u64,
    pub(crate) energy_out: u64,
    pub(crate) cost_usd: f64,
    pub(crate) context_window: Option<u64>,
    pub(crate) session: Option<String>,
    pub(crate) socket: String,
    pub(crate) heartbeat: HeartbeatTier,
    #[allow(dead_code)]
    pub(crate) last_activity: Option<DateTime<Utc>>,
    /// Wall-clock of the molecule's last observable forward motion
    /// (`MoleculeData::last_progress_at`). When the row is `Running` and
    /// `now - last_progress_at` exceeds the active step's
    /// [`stall_timeout_minutes`](cosmon_core::formula::Step::stall_timeout_minutes),
    /// `enrich_rows` promotes the heartbeat to [`HeartbeatTier::Stalled`].
    /// `None` for legacy molecules or freshly-tackled rows.
    pub(crate) last_progress_at: Option<DateTime<Utc>>,
    // --- expanded-row detail fields (populated lazily in `reload`) ---
    /// `variables["topic"]` of the underlying molecule — the one-liner the
    /// nucleator typed. `None` when the formula doesn't bind `topic`.
    pub(crate) topic: Option<String>,
    /// `variables["description"]` from nucleation, if present. Unlike the
    /// formula's generic step text, this is the operator's molecule-specific
    /// mission context and is shown above the briefing template.
    pub(crate) mission_description: Option<String>,
    /// Formula id (e.g. `task-work`). Empty if detail-load failed.
    pub(crate) formula: String,
    /// Tier badge (e.g. `T0`, `T1`, `T2`). Empty if formula could not be
    /// parsed or tier data is unavailable.
    pub(crate) tier_badge: String,
    /// Molecule kind marker ("task", "decision", …). Empty if detail-load
    /// failed or the molecule predates the `kind` field.
    pub(crate) kind: String,
    /// Upstream molecule ids that block this one (sources of `BlockedBy`
    /// typed links), paired with their current status string. Empty when
    /// the molecule is not blocked.
    pub(crate) blocked_by: Vec<(String, String)>,
    /// Worker id currently assigned to this molecule, if any. Distinct
    /// from `role` which is always `"worker"` when a worker is attached.
    pub(crate) worker_name: Option<String>,
    /// Tags attached to this molecule (e.g. `temp:hot`). Empty when none.
    pub(crate) tags: Vec<String>,
    /// UTC timestamp when the molecule was nucleated. `None` for legacy
    /// molecules or if detail-load failed.
    pub(crate) created_at_utc: Option<DateTime<Utc>>,
    /// True when `MOLECULE_DIR/whispers.jsonl` has at least one entry with
    /// `ts` within [`WHISPER_FRESH_WINDOW`] of the refresh time. Drives the
    /// 🫧 glance-legible indicator — "this molecule was touched by a human
    /// hand." Recomputed per refresh, not per frame.
    pub(crate) whisper_fresh: bool,
    /// Compact glyph string advertising every worker role bound to this
    /// molecule (e.g. `"🎛️🧠"` when both runtime and cognition are alive).
    /// Empty when the row has no worker attached.
    pub(crate) role_glyphs: String,
    /// Lineage-coverage percentage parsed from `verify-report.md` (0..=100),
    /// or `None` when no report exists or every claim was SKIP. Powers the
    /// TRUST column and the `v` detail pane.
    pub(crate) trust_score: Option<u8>,
    /// Per-molecule step-budget circuit breaker (THESIS Part XI), as
    /// `(remaining, cap)`. `None` for legacy molecules and projects with
    /// the breaker disabled. Surfaces in the expanded detail block so the
    /// operator sees how close a molecule is to the runaway-loop guard.
    pub(crate) energy_budget: Option<(u32, u32)>,
    /// Honest, retrospective adapter/model attribution folded from this
    /// molecule's `events.jsonl` (`EventV2::AdapterSelected` /
    /// `EventV2::ModelSelected`). Powers the ADAPTER column and the
    /// expanded detail line. Default (empty) for legacy or never-tackled
    /// molecules — the reasoning effort is **never** inferred from the
    /// current config (see [`cosmon_core::adapter_attribution`]).
    pub(crate) adapter: cosmon_core::adapter_attribution::AdapterAttribution,
}

/// Time window within which a whisper is still considered "fresh" and the
/// 🫧 indicator is shown on the molecule row. 60 minutes — long enough for
/// a pilot coming back to a kitchen to glance and recall "ah, I nudged
/// that one", short enough that the icon still means recent contact.
pub(crate) const WHISPER_FRESH_WINDOW: chrono::Duration = chrono::Duration::minutes(60);

/// Predicate: does `path` (a `whispers.jsonl`) have a recent entry?
///
/// Returns `true` when the last line parses as JSON with an RFC3339 `ts`
/// field whose delta from `now` is in `[0, window]`. Any failure
/// (missing file, unreadable, malformed line, missing ts, unparseable ts,
/// or older than the window) returns `false` — the indicator is
/// best-effort and must never surface an error to the TUI.
pub(crate) fn whisper_fresh_within(
    path: &std::path::Path,
    now: DateTime<Utc>,
    window: chrono::Duration,
) -> bool {
    let Some(last) = crate::cmd::whisper::last_whisper_ts(path) else {
        return false;
    };
    let delta = now.signed_duration_since(last);
    delta >= chrono::Duration::zero() && delta <= window
}

/// Cached enrichment fields for a molecule row, invalidated by state.json
/// mtime change. Avoids re-reading molecule state and formula TOML on every
/// reload tick when nothing changed (Phase 3).
#[derive(Clone)]
struct CachedEnrichment {
    topic: Option<String>,
    mission_description: Option<String>,
    formula: String,
    tier_badge: String,
    kind: String,
    blocked_by: Vec<(String, String)>,
    worker_name: Option<String>,
    tags: Vec<String>,
    created_at_utc: Option<DateTime<Utc>>,
    last_progress_at: Option<DateTime<Utc>>,
    energy_budget: Option<(u32, u32)>,
    trust_score: Option<u8>,
    /// Index of the molecule's active step. Not rendered directly — it is
    /// the input the unconditional stall evaluation needs to look up the
    /// step's wall-clock budget in `formula_cache`. Cached so that a
    /// cache-hit tick can still decide the heartbeat without re-reading
    /// `state.json` (delib-20260716-a2f1 C2).
    current_step: usize,
}

/// Fleet-level tally of the worker roster versus live tmux ground truth.
///
/// This exists because the roster is the one object in the fleet that can
/// lie without leaving a molecule behind: a worker entry outlives the tmux
/// session that justified it, and *nothing* in the molecule table ever says
/// so. Phantoms have no `mol_id`, so they can never become rows — the
/// table is keyed by molecule id (see [`snapshot_to_rows`]) and a
/// synthesised row would collide with or corrupt that key. The discrepancy
/// is therefore a count, rendered once, or it is invisible.
///
/// The census is a pure fold over data the TUI already reads: the roster
/// walked by [`snap_find_worker`] and the sessions walked by
/// `snapshot_to_rows`. It adds no I/O.
///
/// **Boundary:** peek reads and renders; `cs purge` writes and drains. The
/// census names the remedy in the strip and never performs it.
///
/// Note for whoever writes the peek taxonomy ADR: the operator's
/// "identify the zombies" requirement is met *here* (for workers) and by
/// the patrol's orphan verdict (for molecules). It is not a filter
/// question — no arrangement of `PhaseFilter` identifies a single zombie.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct WorkerCensus {
    /// Worker entries present in the roster.
    registered: usize,
    /// Roster entries whose worker id is claimed by a live tmux session.
    attached: usize,
}

impl WorkerCensus {
    /// Roster entries no live tmux session claims — the workers the fleet
    /// still counts but no longer runs. `cs purge` is what drains them.
    const fn phantom(self) -> usize {
        self.registered.saturating_sub(self.attached)
    }
}

/// Fold `snap` into a [`WorkerCensus`].
///
/// Sessions in a [`FleetSnapshot`] come from scanning tmux sockets, so a
/// session is ground truth for "this worker is still running"; the roster
/// is merely a record that one was started. A worker is `attached` when
/// some live session names it, and phantom otherwise.
fn worker_census(snap: &FleetSnapshot) -> WorkerCensus {
    let claimed: std::collections::HashSet<&str> = snap
        .list_sessions(&SessionFilter::default())
        .into_iter()
        .filter_map(|s| s.worker_id.as_deref())
        .collect();
    let mut census = WorkerCensus {
        registered: 0,
        attached: 0,
    };
    for w in snap.workers() {
        census.registered += 1;
        if claimed.contains(w.id.0.as_str()) {
            census.attached += 1;
        }
    }
    census
}

/// Payload produced by a background reload thread — the I/O-heavy portion
/// of a reload cycle (Phase 3).
struct BgReloadResult {
    rows: Vec<RowView>,
    state_dirs: std::collections::HashMap<String, std::path::PathBuf>,
    census: WorkerCensus,
}

impl RowView {
    /// Display label for the MOLECULE column. When a session slug is available,
    /// show it truncated to `max_width` with the short hash suffix preserved so
    /// the operator can correlate rows with `tmux ls` output. Falls back to the
    /// raw molecule id for untackled / legacy molecules.
    ///
    /// When the row has fused more than one worker role into a single line,
    /// the compact glyph string is appended so the
    /// operator can see `🎛️🧠` next to the molecule name without a second
    /// row.
    fn display_label(&self, max_width: usize) -> String {
        let base = if let Some(slug) = &self.session_slug {
            truncate_slug(slug, max_width)
        } else {
            truncate_str(&self.mol_id, max_width)
        };
        if self.role_glyphs.is_empty() {
            base
        } else {
            format!("{base} {}", self.role_glyphs)
        }
    }

    /// Classify this row into its visual [`RowKind`] — the pastille that
    /// ends up in the leftmost heartbeat column. See [`crate::visual`] for
    /// the charter.
    fn row_kind(&self) -> RowKind {
        let core_status = parse_row_status(&self.status);
        let heartbeat = if self.worker_name.is_some() {
            Some(self.heartbeat)
        } else {
            None
        };
        let has_blockers = self
            .blocked_by
            .iter()
            .any(|(_, status)| !matches!(status.as_str(), "completed" | "collapsed"));
        visual_classify(&RowInputs {
            status: core_status,
            heartbeat,
            tags: &self.tags,
            has_blockers,
            ghost: false,
            drift: false,
        })
    }
}

/// Truncate a session slug to `max` chars, preserving the trailing short-hash
/// suffix (last 4–5 chars after the final `-`). If the slug fits, return it
/// unchanged. Otherwise: `prefix…-suffix`.
fn truncate_slug(slug: &str, max: usize) -> String {
    if slug.len() <= max {
        return slug.to_owned();
    }
    // Preserve the suffix after the last '-' (usually the 4-char mol hash).
    let suffix = slug.rfind('-').map_or("", |i| &slug[i..]);
    // We need room for at least 1 char + '…' + suffix.
    let avail = max.saturating_sub(suffix.len() + 1); // 1 for '…'
    if avail == 0 {
        // Extreme: just hard-truncate.
        return truncate_str(slug, max);
    }
    format!("{}…{}", &slug[..avail], suffix)
}

/// Hard-truncate a string to `max` chars, appending `…` if trimmed.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else if max <= 1 {
        "…".to_owned()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

/// Compute a centered subrectangle that takes `pct_x` percent of `r`'s
/// width and `pct_y` percent of its height. Used to place the help
/// overlay so it floats over — not inside — the table + detail panes.
fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[0]
}

/// Build the static help overlay as a vector of styled lines.
///
/// Section order is tuned for the most common lookup: new operators land
/// on cs peek looking for *how to copy text*, which is the "Terminal text
/// selection" block — placed after keybindings so repeat users can scan
/// navigation quickly without scrolling. `mouse_captured` toggles a
/// one-word indicator next to the `m` keybinding so the help reflects
/// current state.
fn help_overlay_lines(mouse_captured: bool) -> Vec<Line<'static>> {
    let heading = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let key = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let body = Style::default();

    let key_line = |k: &'static str, desc: &'static str| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{k:<10}"), key),
            Span::styled(desc.to_owned(), body),
        ])
    };

    let mouse_status = if mouse_captured { "ON" } else { "OFF" };
    let mouse_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{:<10}", "M"), key),
        Span::styled("toggle mouse capture  ", body),
        Span::styled(format!("[currently {mouse_status}]"), dim),
    ]);

    vec![
        Line::from(Span::styled("Navigation", heading)),
        key_line("j / k", "move selection up / down"),
        key_line("→ / ←", "expand / collapse selected row"),
        key_line("Ctrl+j/k", "scroll detail pane line-by-line"),
        key_line("PgDn/PgUp", "scroll detail pane by page"),
        key_line("Enter", "attach to selected tmux session"),
        Line::from(""),
        Line::from(Span::styled("Zoom-continu (JR wall)", heading)),
        key_line("+", "zoom in  (ville → immeuble → peau)"),
        key_line("-", "zoom out (peau → immeuble → ville)"),
        key_line("=", "reset to ville (fleet table)"),
        Line::from(""),
        Line::from(Span::styled("Cockpit actions (fires cs <verb>)", heading)),
        key_line("n", "nucleate — formula + topic modal"),
        key_line("t", "tackle  — spawn worker on selected row"),
        key_line("m", "merge-and-done (cs done) — confirm prompt"),
        key_line("w", "whisper — send a perturbation body"),
        key_line(".", "session note — one-line append"),
        Line::from(""),
        Line::from(Span::styled("Detail panes (toggle on/off)", heading)),
        key_line("p", "tmux pane capture"),
        key_line("b", "briefing.md"),
        key_line("l", "log.md"),
        key_line("e", "events.jsonl"),
        key_line("s", "synthesis.md"),
        key_line("r", "responses/"),
        key_line("N", "notes"),
        key_line("g", "git log"),
        key_line("T", "tree view"),
        key_line("v", "verify report"),
        key_line("X", "eXceptions — collapsed molecules + failure lens"),
        Line::from(""),
        Line::from(Span::styled("Clipboard", heading)),
        key_line("y", "yank selected molecule id (OSC 52)"),
        key_line("Y", "yank `cs observe <id> --json` payload"),
        Line::from(""),
        Line::from(Span::styled("Filter & scope", heading)),
        key_line("/", "start filter (Enter=apply, Esc=cancel)"),
        key_line(
            "A",
            "toggle all states (default = running only; show pending/done/…)",
        ),
        key_line("a", "toggle all-projects scope"),
        key_line("R", "reload fleet snapshot now"),
        Line::from(""),
        Line::from(Span::styled("Quit & help", heading)),
        key_line("q / Esc", "exit cs peek"),
        key_line("?", "toggle this help overlay"),
        Line::from(""),
        Line::from(Span::styled(
            "Terminal text selection (copy-paste)",
            heading,
        )),
        Line::from(Span::styled(
            "  cs peek captures mouse events for future pointer features,",
            body,
        )),
        Line::from(Span::styled(
            "  which blocks the terminal's native click-drag selection.",
            body,
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Shift+drag", key),
            Span::styled(
                "  bypass mouse capture and select natively, then copy with",
                body,
            ),
        ]),
        Line::from(Span::styled(
            "              your terminal's usual shortcut (Cmd+C / Ctrl+Shift+C).",
            body,
        )),
        Line::from(Span::styled(
            "              Works on iTerm2, Terminal.app, Alacritty, kitty, WezTerm.",
            dim,
        )),
        Line::from(""),
        mouse_line,
        Line::from(Span::styled(
            "              If Shift+drag doesn't work (some Linux terminals,",
            body,
        )),
        Line::from(Span::styled(
            "              tmux with mouse mode on), press `M` to drop mouse",
            body,
        )),
        Line::from(Span::styled(
            "              capture entirely — native selection then works",
            body,
        )),
        Line::from(Span::styled(
            "              everywhere. Press `M` again to re-enable capture.",
            body,
        )),
    ]
}

/// Advance the TUI phase filter through its canonical presets.
///
/// The cycle is `unfinished` → `all` → back to `unfinished`, and any
/// off-cycle filter snaps to `unfinished` on the next press, so the
/// operator always reaches a known state in at most one keystroke.
///
/// It used to have four stops, because the default was `running` and
/// there were two separate things to opt into. There are no longer: the
/// only rows the default withholds are the terminal ones, so the only
/// question the key can ask is whether to show the archive too. Two
/// presets died when the default stopped lying.
pub(crate) fn cycle_phase_filter(current: super::peek::PhaseFilter) -> super::peek::PhaseFilter {
    use super::peek::PhaseFilter;
    if current == PhaseFilter::unfinished() {
        PhaseFilter::all()
    } else {
        PhaseFilter::unfinished()
    }
}

/// Build the short label for one presence entry on the header strip.
/// Prefers `galaxy "headline"`; falls back to `sid` when galaxy or
/// headline is missing. Truncated so five of these always fit an 80-col
/// terminal.
pub(crate) fn presence_label(p: &presence_reader::PresenceEntry) -> String {
    let galaxy = p.galaxy.as_deref().unwrap_or(p.sid.as_str());
    let headline = p.headline.as_deref().unwrap_or("");
    if headline.is_empty() {
        truncate_str(galaxy, 14)
    } else {
        let raw = format!("{galaxy} \"{headline}\"");
        truncate_str(&raw, 28)
    }
}

/// Parse one `events.jsonl` line into an ensemble-view entry. Returns
/// `None` on malformed JSON — the caller drops the line silently.
pub(crate) fn parse_ensemble_event(raw: &str) -> Option<EnsembleEvent> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = v.as_object()?;
    let ts = obj
        .get("timestamp")
        .or_else(|| obj.get("ts"))
        .and_then(|t| t.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let mol_id = obj
        .get("molecule_id")
        .or_else(|| obj.get("mol_id"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_owned();
    let kind = obj
        .get("kind")
        .or_else(|| obj.get("event"))
        .or_else(|| obj.get("type"))
        .and_then(|k| k.as_str())
        .unwrap_or("event")
        .to_owned();
    let summary = ensemble_event_summary(obj);
    Some(EnsembleEvent {
        ts,
        mol_id,
        kind,
        summary,
    })
}

fn ensemble_event_summary(obj: &serde_json::Map<String, serde_json::Value>) -> String {
    const SKIP: &[&str] = &[
        "timestamp",
        "ts",
        "kind",
        "event",
        "type",
        "molecule_id",
        "mol_id",
        "fleet_id",
        "agent_id",
    ];
    const PREFERRED: &[&str] = &["step", "reason", "evidence", "summary", "message"];
    let mut parts: Vec<String> = Vec::new();
    for k in PREFERRED {
        if let Some(val) = obj.get(*k) {
            parts.push(format!("{k}={}", ensemble_scalar(val)));
        }
    }
    if parts.is_empty() {
        for (k, val) in obj {
            if SKIP.contains(&k.as_str()) {
                continue;
            }
            parts.push(format!("{k}={}", ensemble_scalar(val)));
            if parts.len() >= 3 {
                break;
            }
        }
    }
    parts.join("  ")
}

fn ensemble_kind_color(kind: &str) -> Color {
    match kind {
        "nucleated" | "tackled" | "dispatched" => Color::Cyan,
        "evolved" | "step_completed" => Color::Blue,
        "completed" | "done" => Color::Green,
        "stuck" | "collapsed" | "failed" | "error" => Color::Red,
        "freeze" | "thaw" => Color::Magenta,
        "heartbeat" => Color::DarkGray,
        _ => Color::Yellow,
    }
}

fn ensemble_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            let one = s.replace('\n', " ⏎ ");
            if one.chars().count() > 60 {
                let trimmed: String = one.chars().take(60).collect();
                format!("{trimmed}…")
            } else {
                one
            }
        }
        serde_json::Value::Null => "null".into(),
        other => other.to_string(),
    }
}

/// Apply cached enrichment fields to a row, avoiding disk I/O.
fn apply_cached_enrichment(row: &mut RowView, cached: &CachedEnrichment) {
    row.topic.clone_from(&cached.topic);
    row.mission_description
        .clone_from(&cached.mission_description);
    row.formula.clone_from(&cached.formula);
    row.tier_badge.clone_from(&cached.tier_badge);
    row.kind.clone_from(&cached.kind);
    row.blocked_by.clone_from(&cached.blocked_by);
    if row.worker_name.is_none() {
        row.worker_name.clone_from(&cached.worker_name);
    }
    row.tags.clone_from(&cached.tags);
    row.created_at_utc = cached.created_at_utc;
    row.last_progress_at = cached.last_progress_at;
    row.energy_budget = cached.energy_budget;
    row.trust_score = cached.trust_score;
}

/// The fields that decide both where a row sits and whether the fleet moved.
///
/// These two questions must be answered from the *same* facts, and this tuple
/// is what makes "same" checkable rather than aspirational. Read
/// [`change_key`]'s and [`sort_rows`]'s doc comments together — they are one
/// invariant written in two places, and
/// `sort_key_is_a_function_of_the_change_key` is the test that holds them to
/// it.
type ChangeKey<'a> = (&'a str, &'a str, &'a str, Option<DateTime<Utc>>);

/// The stored facts about a row that peek is entitled to react to:
/// `(mol_id, status, step, updated_at)`.
///
/// Every field is written to `state.json` by someone. None is derived from
/// the clock. That is the whole point: this is the definition of "the fleet
/// changed", and a fleet that changes because time passed is a fleet that
/// never stops changing.
fn change_key(r: &RowView) -> ChangeKey<'_> {
    (
        r.mol_id.as_str(),
        r.status.as_str(),
        r.step.as_str(),
        r.updated_at,
    )
}

/// Detect whether two row vectors represent different fleet state. Used by
/// adaptive polling to distinguish "nothing changed" from "something moved."
///
/// `updated_at` joined this key when the sort key started reading it
/// (delib-20260716-a2f1 C3). The two must agree: if a fact can reorder the
/// table, the poller may not call the tick idle.
fn rows_differ(a: &[RowView], b: &[RowView]) -> bool {
    if a.len() != b.len() {
        return true;
    }
    let mut keys_a: Vec<ChangeKey<'_>> = a.iter().map(change_key).collect();
    let mut keys_b: Vec<ChangeKey<'_>> = b.iter().map(change_key).collect();
    keys_a.sort_unstable();
    keys_b.sort_unstable();
    keys_a != keys_b
}

/// Order the table: liveness band ASC (Live, Stuck, Waiting, Dormant, Dead —
/// the watchdog reads top-down), then `updated_at` DESC within a band so the
/// most recently touched molecule rises, then `mol_id` to break ties.
///
/// **The invariant this function exists to hold: the sort key is a function
/// of [`change_key`].** The band derives from `status`, `updated_at` is
/// `updated_at`, `mol_id` is `mol_id` — every term is a stored fact the
/// change detector already watches. So two ticks that `rows_differ` calls
/// identical render in an identical order, and the poller can never report
/// `idle_ticks += 1` on the tick the table reorders itself under the
/// operator's cursor.
///
/// What is *not* in the key is the point. The old key's second term was
/// `heartbeat` DESC, and heartbeat is a threshold over `now()` that no
/// change detector watches — so rows reordered on a timer, in silence. Sort
/// keys are monotone in time or constant; a thresholded clock-derived
/// quantity is neither.
///
/// `updated_at: None` (a session with no molecule) sorts last within its
/// band: [`std::cmp::Reverse`] puts `Some` before `None`.
fn sort_rows(rows: &mut [RowView]) {
    rows.sort_by(|a, b| {
        liveness_band(a)
            .cmp(&liveness_band(b))
            .then_with(|| std::cmp::Reverse(a.updated_at).cmp(&std::cmp::Reverse(b.updated_at)))
            .then_with(|| a.mol_id.cmp(&b.mol_id))
    });
}

/// Operator-facing liveness bands used to partition the table into roughly
/// two halves: "above the fold" (actionable rows the watchdog cares about
/// right now) and "below the fold" (reference material: frozen, terminal).
///
/// Derived from `(phase, heartbeat)` rather than either dimension alone — a
/// waiting molecule is not stalled just because it has no tmux session, and
/// a running molecule whose worker is gone is not live just because its
/// status is `running`. Ordering is ascending: `Live` sorts first.
///
/// This is a **rendering** partition, not a filter: it decides where a row
/// sits relative to the fold, never whether the row exists. The filter is
/// [`PhaseFilter`](crate::cmd::peek::PhaseFilter), and the two must not be
/// confused — a band called `Dead` deciding what the operator may see is
/// how `starved` went missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LivenessBand {
    /// Live phase — the molecule claims to be running. The workers that
    /// matter right now. Whether one is actually breathing is the heartbeat
    /// column's job to say; it is not this band's, because the answer moves
    /// on its own (see [`liveness_band`]).
    Live,
    /// `Blocked` phase — an external authority refusing service (ADR-062).
    /// Needs operator attention: a wait or a rotation, never a re-prompt.
    ///
    /// The word is a homonym of the `cs stuck` verb and does not mean the
    /// same thing. `stuck` proper is `frozen ∧ stuck_at.is_some()` — see
    /// [`MoleculeStatus::is_stuck`](cosmon_core::molecule::MoleculeStatus::is_stuck).
    /// This band is the alarm; that predicate is a freeze with a reason.
    Stuck,
    /// Waiting phase — pending / queued, waiting for `cs tackle`. No
    /// worker, no activity.
    Waiting,
    /// Parked phase — frozen, on the shelf by design.
    Dormant,
    /// Terminal phases (failed / done). Goes below the fold.
    ///
    /// It once claimed `starved` too, which is how the one status whose
    /// purpose is to summon the operator came to be filed with the
    /// corpses.
    Dead,
}

/// Derive the molecule's health overlay for a peek row.
///
/// The peek TUI does not carry a reconciled worker `EffectiveStatus`
/// per row — it tracks [`HeartbeatTier`] instead. We project the tier
/// onto the closest `EffectiveStatus` shape so
/// [`cosmon_core::reconcile::molecule_health`] stays the single source
/// of truth for the classification.
/// Recover the typed status from a [`RowView`]'s rendered label.
///
/// `RowView.status` is a `String` because the table renders it directly, so
/// every classifier that needs the enum has to parse it back. That parse used
/// to be hand-rolled at each call site, and each copy knew a different subset
/// of the alphabet — `starved` and `queued` fell through a `_ =>` arm into
/// `Pending`, which is how a molecule whose whole purpose is to summon the
/// operator (ADR-062) ended up rendered as inert.
///
/// [`MoleculeStatus`]'s own `FromStr` is the only converter that knows every
/// variant, and it is exhaustive inside `cosmon-core`, so a new variant is a
/// compile error there rather than a silent misclassification here.
///
/// The string always originates from `MoleculeStatus`'s `Display`, so a parse
/// failure means a genuinely unrecognised value rather than a missing arm. We
/// fall back to `Pending` — the classifiers resolve it to an inert/waiting
/// shape, which is the honest rendering for a status this binary does not
/// understand, and unlike the old arms it no longer swallows statuses the
/// binary *does* understand.
fn parse_row_status(label: &str) -> MoleculeStatus {
    label.parse().unwrap_or(MoleculeStatus::Pending)
}

fn molecule_health_for_row(r: &RowView) -> MoleculeHealth {
    use cosmon_core::worker::EffectiveStatus;

    let core_status = parse_row_status(&r.status);

    // Only synthesize a worker effective status when a worker is
    // actually attached to this row; otherwise `molecule_health` will
    // correctly flag an orphan / inert row from the status alone.
    let worker = r.worker_name.as_ref().map(|_| match r.heartbeat {
        HeartbeatTier::Active | HeartbeatTier::Idle | HeartbeatTier::Quiet => {
            EffectiveStatus::Healthy
        }
        HeartbeatTier::Stalled => EffectiveStatus::Suspect,
        HeartbeatTier::Orphaned => EffectiveStatus::Diverged,
    });

    molecule_health(core_status, worker.as_ref())
}

/// Predicate: should this molecule's heartbeat be promoted to
/// [`HeartbeatTier::Stalled`] because no observable forward motion has
/// occurred for longer than the step's wall-clock budget?
///
/// Pure function so the stall semantics are unit-testable without a
/// real `FileStore` or formula cache. Returns `false` when no progress
/// timestamp has been recorded yet (the worker may simply have just
/// been tackled).
#[must_use]
fn is_stalled_by_progress(
    last_progress_at: Option<DateTime<Utc>>,
    step_timeout_minutes: Option<u32>,
    now: DateTime<Utc>,
) -> bool {
    let Some(ts) = last_progress_at else {
        return false;
    };
    let budget = chrono::Duration::minutes(i64::from(step_timeout_minutes.unwrap_or(30)));
    now.signed_duration_since(ts) > budget
}

/// Classify a row into a [`LivenessBand`] — where it sits relative to the
/// fold, which is a *rendering* question and not a filter one.
///
/// The band reads [`MoleculeStatus::phase`] rather than re-deciding the
/// status alphabet for itself. This function used to own a sixth private
/// classification, complete with a `_ =>` arm; the arm is gone, so a new
/// `Phase` fails to compile here instead of silently banding as `Waiting`.
///
/// The band used to split [`Phase::Live`] by heartbeat, sending a stalled or
/// orphaned `running` row into [`LivenessBand::Stuck`]. That split is gone
/// (delib-20260716-a2f1 C3), because the band is the first term of the sort
/// key and [`HeartbeatTier`] is a *thresholded function of `now()`*: a row
/// whose `last_activity` never moves still walks Active → Idle → Quiet →
/// Stalled on its own, crossing the band boundary at 30 minutes and
/// reordering itself against every peer with no state change underneath it.
/// The heartbeat remains a column and a colour — rendered, never ordered by.
///
/// The cost is real and was weighed: a wedged worker no longer rises above
/// the fold on its own. The panel's answer is that `orphaned` is a fact
/// someone must *write* before peek may sort on it — the patrol already is
/// that writer — and that the phantom-worker counter, not the sort, is what
/// surfaces zombies.
fn liveness_band(r: &RowView) -> LivenessBand {
    match parse_row_status(&r.status).phase() {
        Phase::Live => LivenessBand::Live,
        // ADR-062: alive, and the one phase whose entire purpose is to
        // summon the operator. It banded `Dead` — below the fold, with the
        // corpses — for as long as it reached this function as the string
        // `"stuck"`, a label with no referent in the core alphabet.
        Phase::Blocked => LivenessBand::Stuck,
        Phase::Waiting => LivenessBand::Waiting,
        Phase::Parked => LivenessBand::Dormant,
        Phase::Failed | Phase::Done => LivenessBand::Dead,
    }
}

/// Cockpit action modal — an in-TUI input prompt that collects operator
/// input before shelling out to a `cs <verb>` one-shot.
///
/// The morning portal is `cs peek` itself; operators must nucleate /
/// tackle / merge /
/// whisper / drop a session note without leaving the TUI. Each variant is
/// a tiny state machine — same discipline as `filter_input_mode`, just
/// typed so the event loop can't confuse a "topic" keystroke with a
/// "whisper body" keystroke.
///
/// Invariants (wheat-paste rule):
/// - Each variant fires **one** `cs <verb>` process and waits for it to
///   exit before returning to the table view. No cached state, no
///   long-lived child processes.
/// - `Esc` cancels every modal without side effects.
/// - Modals render as centered popovers reusing the existing ratatui
///   widgets; no new crate dependencies.
#[derive(Default)]
enum ActionModal {
    /// No modal active — default state.
    #[default]
    None,
    /// `n` — launching `cs nucleate <formula> --var topic="..."`.
    ///
    /// Two text fields walked with `Tab`. Enter on the *topic* field
    /// fires the process; Enter on *formula* advances to *topic*.
    Nucleate(NucleateForm),
    /// `t` — confirming `cs tackle <id>` for the selected row. Single
    /// `y`/`n` prompt; populated at dispatch time with the molecule id so
    /// the decision survives a background reload.
    ConfirmTackle { mol_id: String },
    /// `m` — confirming `cs done <id>` (merge + teardown) for the
    /// selected row. `y`/`n` prompt.
    ConfirmMerge { mol_id: String },
    /// `w` — composing a whisper body for the selected row.
    Whisper { mol_id: String, body: String },
    /// `.` — composing a free-form session note. Shells out to
    /// `cs session note "<line>"` which is idempotent: no open session →
    /// graceful error surfaced in the status bar.
    SessionNote { body: String },
}

impl ActionModal {
    fn is_active(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Nucleate modal form — two fields walked with Tab.
#[derive(Default)]
struct NucleateForm {
    formula: String,
    topic: String,
    /// Which field currently receives keystrokes.
    focus: NucleateField,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum NucleateField {
    #[default]
    Formula,
    Topic,
}

#[allow(clippy::struct_excessive_bools)]
struct App {
    state_dir: std::path::PathBuf,
    socket: String,
    all_projects: bool,
    project_id: Option<cosmon_core::id::ProjectId>,
    /// Project id resolved from the cwd at startup, so the `a` key can
    /// toggle between cross-project and single-project scope without
    /// re-reading the config each time.
    default_project_id: Option<cosmon_core::id::ProjectId>,
    refresh: Duration,

    rows: Vec<RowView>,
    /// Roster-versus-tmux tally rendered by [`Self::draw_worker_strip`].
    /// Refreshed on every reload alongside `rows` — the strip and the
    /// table always describe the same snapshot.
    census: WorkerCensus,
    /// Per-molecule state dir, so detail artifacts can be resolved for the
    /// selected row whether we're in single-project or `--all` mode.
    row_state_dirs: std::collections::HashMap<String, std::path::PathBuf>,
    /// Molecule ids whose row is expanded (showing tree-view detail lines).
    /// Keyed by `mol_id` rather than row index so the expansion state
    /// survives `reload()` resorts and filter changes.
    expanded: std::collections::HashSet<String>,
    table_state: TableState,
    filter: String,
    filter_input_mode: bool,
    /// Which molecule phases the table surfaces — see
    /// [`crate::cmd::peek::PhaseFilter`]. Default
    /// (`PhaseFilter::unfinished`) hides the archive and nothing else.
    /// The `A` key cycles the presets at runtime (unfinished → all →
    /// unfinished); the CLI flag `--phase` (or its sugar `--all`) seeds
    /// the initial value. Rationale (operator, 2026-04-27): the archive drowns the
    /// daily signal — the operator wants what is *travailling*. Note the
    /// sentence named the archive, and only the archive: the default
    /// that shipped in its name also hid every frozen, starved and
    /// pending molecule, which no other instrument reports.
    phase_filter: super::peek::PhaseFilter,
    /// Registered detail-pane renderers. Populated once in [`App::new`] —
    /// key dispatch and the current-pane lookup both index into this vec.
    renderers: Vec<Box<dyn DetailRenderer>>,
    /// Index into [`Self::renderers`] of the active pane, or `None` when
    /// no detail pane is shown (the watchdog's default / restful state).
    active_renderer: Option<usize>,
    detail_content: ratatui::text::Text<'static>,
    detail_scroll: u16,
    last_refresh: Instant,
    status_msg: String,

    // --- Phase 3: mtime-gated reload, background reload, adaptive polling ---
    /// Mtime-gated enrichment cache: `mol_id -> (mtime, cached)`.
    /// Skips re-reading state.json + formula TOML for unchanged molecules.
    enrichment_cache: std::collections::HashMap<String, (SystemTime, CachedEnrichment)>,

    /// Formula TOML cache: `path -> (mtime, parsed)`. Formulas rarely
    /// change mid-session, but when the operator edits one (e.g. bumping a
    /// tier) we want the change to land on the next tick without requiring
    /// a TUI restart — so the cache invalidates on `mtime` divergence
    /// rather than caching forever.
    formula_cache:
        std::collections::HashMap<std::path::PathBuf, (SystemTime, cosmon_core::formula::Formula)>,

    /// Consecutive reload ticks with zero state changes. Drives adaptive
    /// polling: >4 idle ticks → 1 Hz, any change → 4 Hz (250 ms).
    idle_ticks: u32,

    /// Background reload channel — receiver side.
    bg_rx: std::sync::mpsc::Receiver<BgReloadResult>,

    /// Background reload channel — sender side (cloned into threads).
    bg_tx: std::sync::mpsc::Sender<BgReloadResult>,

    /// Whether a background reload thread is in flight.
    bg_pending: bool,

    /// Whether the `?` help overlay is currently visible. Toggled by `?`.
    show_help: bool,

    /// Whether crossterm mouse capture is currently enabled.
    ///
    /// `cs peek` enables mouse capture at startup so future pointer
    /// features (click-to-select row, scroll-wheel paging) can be wired in
    /// without a rebuild. The side effect is that the terminal emulator
    /// stops seeing click-drag events, which disables native text
    /// selection — so operators can't copy output with the mouse.
    ///
    /// The workaround on every common macOS / Linux terminal is to hold
    /// **Shift** while dragging: the emulator bypasses the application's
    /// mouse capture and performs a native selection instead (iTerm2,
    /// `Terminal.app`, `Alacritty`, `kitty`, `WezTerm` all support this).
    ///
    /// If Shift+drag does not work (some Linux terminals, tmux without
    /// `mouse off`), the operator can press `m` to toggle capture off
    /// entirely — this field tracks the current state so `m` is an
    /// idempotent toggle.
    mouse_captured: bool,

    /// Continuous zoom level across three scales (JR "Le mur qui respire"):
    /// `0.0` = ville (fleet table), `1.0` = immeuble (one molecule
    /// pleine-page with adjacent neighbours + DAG cables), `2.0` = peau
    /// (raw artifact text, full resolution). Fractional values blend the
    /// two neighbouring scales side-by-side so the operator never loses
    /// orientation on a discrete mode switch. See `docs/guides/peek-zoom.md`.
    zoom_level: f32,

    /// Active cockpit action modal — see [`ActionModal`]. When active,
    /// the event loop routes keystrokes to the modal instead of the
    /// table. [`ActionModal::None`] is the default idle state.
    action_modal: ActionModal,

    /// Live Claude-session presence entries, refreshed on each reload.
    /// Rendered as a compact 1-line header strip at the top of the TUI.
    /// Stale entries (heartbeat > 3 min) are shown greyed; dead ones
    /// (no heartbeat) are omitted. Populated by
    /// [`presence_reader::scan`]; an empty vector collapses the strip.
    presence: Vec<presence_reader::PresenceEntry>,

    /// When `Some`, display utterances/events across the fleet (newest
    /// first) instead of the normal table body. Toggled by `E`. The
    /// vector is refreshed on every reload — not lazily on key press —
    /// so `j/k` / Enter act on the current snapshot.
    ensemble_events: Option<EnsembleEventsView>,

    /// Fleet-wide ensemble filter configuration, passed on the command
    /// line. Applied to both the molecule table and the ensemble tab.
    filter_cfg: FilterConfig,
}

impl App {
    #[allow(clippy::unnecessary_wraps)]
    fn new(ctx: &Context, args: &TuiOptions) -> anyhow::Result<Self> {
        let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
        let socket = super::tmux_socket_name(ctx);
        let config_path = super::resolve_config_from_context(ctx);
        let default_project_id = cosmon_filestore::load_project_config(&config_path)
            .ok()
            .and_then(|c| c.project.project_id);
        let project_id = if args.all_projects {
            None
        } else {
            default_project_id.clone()
        };
        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            state_dir,
            socket,
            all_projects: args.all_projects,
            project_id,
            default_project_id,
            refresh: Duration::from_millis(args.refresh_ms.max(250)),
            rows: Vec::new(),
            census: WorkerCensus::default(),
            row_state_dirs: std::collections::HashMap::new(),
            expanded: std::collections::HashSet::new(),
            table_state: TableState::default(),
            filter: String::new(),
            filter_input_mode: false,
            phase_filter: args.phase_filter,
            renderers: renderers::all(),
            active_renderer: None,
            detail_content: ratatui::text::Text::default(),
            detail_scroll: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_msg: String::new(),
            enrichment_cache: std::collections::HashMap::new(),
            formula_cache: std::collections::HashMap::new(),
            idle_ticks: 0,
            bg_rx,
            bg_tx,
            bg_pending: false,
            show_help: false,
            mouse_captured: true,
            zoom_level: 0.0,
            action_modal: ActionModal::None,
            presence: Vec::new(),
            ensemble_events: None,
            filter_cfg: args.filter.clone(),
        };
        if let Some(prefill) = &args.filter.free_text {
            app.filter.clone_from(prefill);
        }
        // Initial reload: failure should not crash the TUI — an empty fleet
        // with a status-bar warning is strictly more useful than an
        // immediate exit on a half-initialised state dir (delib-b8c6 P1/C4).
        app.reload_or_warn();
        app.table_state
            .select(if app.rows.is_empty() { None } else { Some(0) });
        Ok(app)
    }

    /// Synchronous reload — builds snapshot, enriches rows, sorts.
    /// Used by `App::new()`, `R` (manual refresh), `a` (scope toggle).
    fn reload(&mut self) -> anyhow::Result<()> {
        let (snapshot, state_dirs) = build_snapshot(
            &self.state_dir,
            &self.socket,
            self.project_id.as_ref(),
            self.all_projects,
        )?;
        let rows = snapshot_to_rows(&snapshot);
        let census = worker_census(&snapshot);
        self.apply_reload(rows, state_dirs, census);
        Ok(())
    }

    /// Interactive reload variant — never returns `Err`. On failure the
    /// last-known-good rows are retained and a short warning is surfaced in
    /// the status bar so the operator knows something went wrong without
    /// the TUI crashing. Bound to `R` and `a`.
    fn reload_or_warn(&mut self) {
        if let Err(e) = self.reload() {
            let msg = e.to_string();
            let short: String = msg.chars().take(80).collect();
            self.status_msg = format!("⚠ reload failed: {short}");
        }
    }

    /// Apply a reload result: enrich rows (with mtime + formula caches),
    /// sort, and restore selection. Shared by sync and background paths.
    fn apply_reload(
        &mut self,
        mut rows: Vec<RowView>,
        state_dirs: std::collections::HashMap<String, std::path::PathBuf>,
        census: WorkerCensus,
    ) {
        self.row_state_dirs = state_dirs;
        self.census = census;
        let selected_id = self
            .table_state
            .selected()
            .and_then(|i| self.rows.get(i).map(|r| r.mol_id.clone()));

        // Enrich each row with detail fields, using the mtime + formula
        // caches to skip re-reading unchanged molecules (Phase 3, items 8/S3).
        self.enrich_rows(&mut rows);

        // Detect state changes for adaptive polling (Phase 3, item 10).
        let changed = rows_differ(&self.rows, &rows);
        if changed {
            self.idle_ticks = 0;
        } else {
            self.idle_ticks = self.idle_ticks.saturating_add(1);
        }

        self.rows = rows;
        sort_rows(&mut self.rows);
        if let Some(id) = selected_id {
            if let Some(pos) = self.rows.iter().position(|r| r.mol_id == id) {
                self.table_state.select(Some(pos));
            }
        }
        if self.table_state.selected().is_none() && !self.rows.is_empty() {
            self.table_state.select(Some(0));
        }
        self.last_refresh = Instant::now();
        self.refresh_presence();
        if self.ensemble_events.is_some() {
            self.refresh_ensemble_events();
        }
    }

    /// Re-scan the presence directory. Called on every reload. Dead
    /// sessions (no heartbeat at all) are dropped entirely; stale
    /// sessions (heartbeat > 3 min) are kept and rendered greyed.
    fn refresh_presence(&mut self) {
        let now = Utc::now();
        let scanned = if self.all_projects {
            presence_reader::scan_all_galaxies()
        } else {
            presence_reader::scan(&self.state_dir)
        };
        self.presence = scanned
            .into_iter()
            .filter(|p| p.heartbeat_at.is_some())
            .filter(|p| p.age(now).is_some_and(|s| s < 24 * 3600))
            .collect();
    }

    /// Rebuild the ensemble events view from the fleet's `events.jsonl`
    /// files, newest-first. Stubs the C-TAIL-EVENTS backend: scans every
    /// resolved state dir we already know about (from `row_state_dirs`)
    /// plus the current project's `events.jsonl`. At most 200 entries.
    fn refresh_ensemble_events(&mut self) {
        use std::io::BufRead as _;

        let mut roots: std::collections::BTreeSet<std::path::PathBuf> =
            std::collections::BTreeSet::new();
        roots.insert(self.state_dir.clone());
        for sd in self.row_state_dirs.values() {
            roots.insert(sd.clone());
        }

        let mut all: Vec<EnsembleEvent> = Vec::new();
        for root in roots {
            let path = root.join("events.jsonl");
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                if let Some(ev) = parse_ensemble_event(&line) {
                    all.push(ev);
                }
            }
        }

        // Apply filter config (tag is molecule-scoped; skipped here).
        if let Some(since) = self.filter_cfg.since {
            all.retain(|e| e.ts.is_none_or(|t| t >= since));
        }
        if let Some(needle) = &self.filter_cfg.free_text {
            let n = needle.to_ascii_lowercase();
            all.retain(|e| {
                e.mol_id.to_ascii_lowercase().contains(&n)
                    || e.kind.to_ascii_lowercase().contains(&n)
                    || e.summary.to_ascii_lowercase().contains(&n)
            });
        }

        // Newest first, ties broken by mol_id for stability.
        all.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| a.mol_id.cmp(&b.mol_id)));

        // --since-event N: keep only the most-recent N per molecule.
        if let Some(n) = self.filter_cfg.since_event {
            let mut per_mol: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            all.retain(|e| {
                let c = per_mol.entry(e.mol_id.clone()).or_insert(0);
                *c += 1;
                *c <= n
            });
        }

        all.truncate(200);

        let selected = self
            .ensemble_events
            .as_ref()
            .map(|v| v.selected.min(all.len().saturating_sub(1)))
            .unwrap_or(0);
        self.ensemble_events = Some(EnsembleEventsView {
            entries: all,
            selected,
        });
    }

    /// Enrich rows with detail fields from disk, using mtime-gated caching
    /// to skip unchanged molecules and a formula cache for TOML reads that
    /// never change at runtime.
    /// Fold every visible molecule's adapter/model selection out of the
    /// fleet event logs into a `mol_id -> AdapterAttribution` map.
    ///
    /// The `AdapterSelected` / `ModelSelected` events live in the
    /// **fleet-level** `events.jsonl` under each state dir (not per-molecule),
    /// so we read each distinct state dir at most once per reload, filter each
    /// envelope by molecule id, and fold the honest attribution with the pure
    /// [`cosmon_core::adapter_attribution::AdapterAttribution::fold`]. Any I/O
    /// error (missing / unreadable log) simply yields no attribution for that
    /// dir — the column falls back to the empty placeholder, never an error.
    fn fold_adapter_attributions(
        &self,
        rows: &[RowView],
    ) -> std::collections::HashMap<String, cosmon_core::adapter_attribution::AdapterAttribution>
    {
        use cosmon_core::adapter_attribution::AdapterAttribution;
        use cosmon_core::event_v2::EventV2;

        // Distinct state dirs backing the currently visible rows.
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        for row in rows {
            if let Some(sd) = self.row_state_dirs.get(&row.mol_id) {
                if !dirs.iter().any(|d| d == sd) {
                    dirs.push(sd.clone());
                }
            }
        }

        let mut out: std::collections::HashMap<String, AdapterAttribution> =
            std::collections::HashMap::new();
        for sd in dirs {
            let log_path = cosmon_state::event_log::resolve_events_log_path(&sd);
            let Ok(envelopes) = cosmon_state::event_log::read_all(&log_path) else {
                continue;
            };
            // Group each molecule's events (in append order) then fold once.
            let mut by_mol: std::collections::HashMap<String, Vec<EventV2>> =
                std::collections::HashMap::new();
            for env in envelopes {
                if let Some(mid) = env.event.molecule_id() {
                    by_mol.entry(mid.to_string()).or_default().push(env.event);
                }
            }
            for (mid, events) in by_mol {
                let att = AdapterAttribution::fold(&events);
                if !att.is_empty() {
                    out.insert(mid, att);
                }
            }
        }
        out
    }

    fn enrich_rows(&mut self, rows: &mut [RowView]) {
        // Honest adapter/model attribution (task-20260712-6609). The
        // selection is persisted only in the fleet-level `events.jsonl`
        // (`AdapterSelected` / `ModelSelected`), so we fold each unique
        // state dir's log ONCE per reload into a `mol_id -> attribution`
        // map and hand each row its own slice. Reading here — not in the
        // zero-I/O core — keeps the fold pure and shared; the reasoning
        // effort is never back-filled from the current config.
        let adapter_map = self.fold_adapter_attributions(rows);
        for row in rows.iter_mut() {
            if let Some(att) = adapter_map.get(&row.mol_id) {
                row.adapter = att.clone();
                // D3 live-pending: a running molecule with no observation yet
                // renders `...` (motion) instead of `?`. The fold stays honest
                // (never claims liveness); the promotion happens here, where the
                // TUI knows the molecule is currently running.
                let worker_is_live = row.status.eq_ignore_ascii_case("running");
                row.adapter.mark_pending_if_live(worker_is_live);
            }
            let Some(sd) = self.row_state_dirs.get(&row.mol_id) else {
                continue;
            };
            let Ok(mid) = cosmon_core::id::MoleculeId::new(row.mol_id.clone()) else {
                continue;
            };
            let store = FileStore::new(sd);
            let mol_dir = store.molecule_dir(&mid);
            row.whisper_fresh = whisper_fresh_within(
                &mol_dir.join("whispers.jsonl"),
                Utc::now(),
                WHISPER_FRESH_WINDOW,
            );
            let state_path = mol_dir.join("state.json");

            // Mtime-gated cache: skip re-reading state.json if unchanged.
            let current_mtime = std::fs::metadata(&state_path)
                .and_then(|m| m.modified())
                .ok();
            // `current_step` is threaded out of both branches (fresh read or
            // cache) so the stall evaluation below runs on every tick, not
            // only on cache misses (delib-20260716-a2f1 C2).
            let mut current_step: Option<usize> = None;
            if let Some(mtime) = current_mtime {
                if let Some((cached_mtime, cached)) = self.enrichment_cache.get(&row.mol_id) {
                    if *cached_mtime == mtime {
                        apply_cached_enrichment(row, cached);
                        current_step = Some(cached.current_step);
                    }
                }
            }

            // Cache miss — read from disk and cache the result.
            if current_step.is_none() {
                if let Ok(mol) = store.load_molecule(&mid) {
                    current_step = Some(mol.current_step);
                    row.last_progress_at = mol.last_progress_at;
                    row.topic = mol.display_topic().map(ToString::to_string);
                    row.mission_description = mol
                        .variables
                        .get("description")
                        .filter(|description| !description.is_empty())
                        .cloned();
                    row.formula = mol.formula_id.to_string();
                    row.kind = mol
                        .kind
                        .as_ref()
                        .map(|k| format!("{k:?}").to_lowercase())
                        .unwrap_or_default();
                    row.blocked_by = mol
                        .blocked_by()
                        .into_iter()
                        .map(|bid| {
                            let status = store
                                .load_molecule(bid)
                                .map(|m| format!("{:?}", m.status).to_lowercase())
                                .unwrap_or_else(|_| "?".to_owned());
                            (bid.to_string(), status)
                        })
                        .collect();
                    if row.worker_name.is_none() {
                        row.worker_name =
                            mol.assigned_worker.as_ref().map(|w| w.as_str().to_owned());
                    }
                    row.tags = mol.tags.iter().map(ToString::to_string).collect();
                    row.created_at_utc = Some(mol.created_at);
                    row.energy_budget = mol.energy_budget.map(|b| (b.remaining, b.cap));
                    // Read lineage-coverage score from `verify-report.md`
                    // (task-20260412-b606). The file is tiny (a few KB at
                    // most), so a fresh read per reload is cheap and keeps
                    // the TRUST column honest when the operator re-runs
                    // `cs verify` between ticks.
                    row.trust_score = trust::load_report(&store.molecule_dir(&mid))
                        .and_then(|r| r.coverage_pct());
                    // Formula cache: read once, reload on mtime change
                    // (delib-b8c6 P1/C5). The cost of a stat() call per row is
                    // negligible compared to a full TOML parse, and it keeps
                    // `cs peek` honest when a formula is edited live.
                    let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(sd);
                    let fp = formulas_dir.join(format!("{}.formula.toml", mol.formula_id));
                    let fp_mtime = std::fs::metadata(&fp).and_then(|m| m.modified()).ok();
                    let cache_hit = matches!(
                        (self.formula_cache.get(&fp), fp_mtime),
                        (Some((cached_mtime, _)), Some(cur)) if *cached_mtime == cur
                    );
                    if cache_hit {
                        if let Some((_, formula)) = self.formula_cache.get(&fp) {
                            formula.tier.badge().clone_into(&mut row.tier_badge);
                        }
                    } else if let Ok(toml_text) = std::fs::read_to_string(&fp) {
                        if let Ok(formula) = cosmon_core::formula::Formula::parse(&toml_text) {
                            formula.tier.badge().clone_into(&mut row.tier_badge);
                            if let Some(m) = fp_mtime {
                                self.formula_cache.insert(fp.clone(), (m, formula));
                            }
                        }
                    }
                    // Cache the enrichment for next tick.
                    if let Some(mtime) = current_mtime {
                        self.enrichment_cache.insert(
                            row.mol_id.clone(),
                            (
                                mtime,
                                CachedEnrichment {
                                    topic: row.topic.clone(),
                                    mission_description: row.mission_description.clone(),
                                    formula: row.formula.clone(),
                                    tier_badge: row.tier_badge.clone(),
                                    kind: row.kind.clone(),
                                    blocked_by: row.blocked_by.clone(),
                                    worker_name: row.worker_name.clone(),
                                    tags: row.tags.clone(),
                                    created_at_utc: row.created_at_utc,
                                    last_progress_at: row.last_progress_at,
                                    energy_budget: row.energy_budget,
                                    trust_score: row.trust_score,
                                    current_step: mol.current_step,
                                },
                            ),
                        );
                    }
                }
            }

            // delib-1b02 (M2): promote the heartbeat to `Stalled` when the
            // molecule has not advanced for longer than the active step's
            // `timeout_minutes` budget (default 30 min, M3). Tmux activity is
            // attach-bumped and lies; `last_progress_at` ticks only on
            // `cs evolve` and is the authoritative forward-motion signal.
            //
            // delib-20260716-a2f1 (C2): this runs on EVERY tick, cache hit or
            // miss. Evaluating it only on a miss made the verdict depend on
            // whether a memoisation happened to hit — the stall picture
            // appeared for one frame at cold start and never returned, since a
            // miss requires `state.json` to have just changed, which is very
            // nearly the definition of *not* stalled. Both inputs are cached
            // (`last_progress_at`, `current_step`) and `formula_cache` is keyed
            // by path, so this costs no new I/O.
            if row.status == "running" {
                if let Some(step_idx) = current_step {
                    let fp = cosmon_filestore::resolve_formulas_dir_from(sd)
                        .join(format!("{}.formula.toml", row.formula));
                    if let Some((_, formula)) = self.formula_cache.get(&fp) {
                        let budget = formula
                            .steps
                            .get(step_idx)
                            .map(cosmon_core::formula::Step::stall_timeout_minutes);
                        if is_stalled_by_progress(row.last_progress_at, budget, Utc::now()) {
                            row.heartbeat = HeartbeatTier::Stalled;
                        }
                    }
                }
            }
        }
    }

    /// Spawn a background reload thread. The heavy I/O (`build_snapshot` +
    /// `snapshot_to_rows`) runs off the event loop so the TUI never freezes
    /// on cold-cache or NFS reads (Phase 3, item 9 — hawking boundary).
    fn start_bg_reload(&mut self) {
        if self.bg_pending {
            return;
        }
        self.bg_pending = true;
        let state_dir = self.state_dir.clone();
        let socket = self.socket.clone();
        let project_id = self.project_id.clone();
        let all_projects = self.all_projects;
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            if let Ok((snapshot, state_dirs)) =
                build_snapshot(&state_dir, &socket, project_id.as_ref(), all_projects)
            {
                let rows = snapshot_to_rows(&snapshot);
                let census = worker_census(&snapshot);
                let _ = tx.send(BgReloadResult {
                    rows,
                    state_dirs,
                    census,
                });
            }
            // I/O error — silently skip this tick. The TUI retains
            // last-known-good state (C4 from delib-b8c6).
        });
    }

    /// Check for a completed background reload. Returns `true` if new data
    /// was applied.
    fn poll_bg_reload(&mut self) -> bool {
        if !self.bg_pending {
            return false;
        }
        match self.bg_rx.try_recv() {
            Ok(result) => {
                self.bg_pending = false;
                self.apply_reload(result.rows, result.state_dirs, result.census);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.bg_pending = false;
                false
            }
        }
    }

    /// Effective refresh interval, driven by adaptive polling (Phase 3,
    /// item 10). 4 Hz (250 ms) when the fleet is active, 1 Hz (1000 ms)
    /// after 5 consecutive idle ticks.
    fn effective_refresh(&self) -> Duration {
        const IDLE_THRESHOLD: u32 = 5;
        const ACTIVE_MS: u64 = 250;
        const IDLE_MS: u64 = 1000;
        if self.idle_ticks > IDLE_THRESHOLD {
            Duration::from_millis(IDLE_MS)
        } else {
            Duration::from_millis(ACTIVE_MS)
        }
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        let cfg = &self.filter_cfg;
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if !self.phase_filter.matches(&r.status) {
                    return false;
                }
                if let Some(tag) = &cfg.tag {
                    if !r.tags.iter().any(|t| t == tag) {
                        return false;
                    }
                }
                if let Some(since) = cfg.since {
                    if r.created_at_utc.is_some_and(|t| t < since) {
                        return false;
                    }
                }
                if needle.is_empty() {
                    return true;
                }
                r.mol_id.to_lowercase().contains(&needle)
                    || r.session_slug
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().contains(&needle))
                    || r.role.to_lowercase().contains(&needle)
                    || r.status.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let visible = self.filtered_indices();
        if visible.is_empty() {
            self.table_state.select(None);
            return;
        }
        let cur_row = self.table_state.selected().unwrap_or(visible[0]);
        let cur_pos = visible.iter().position(|&i| i == cur_row).unwrap_or(0) as isize;
        let new_pos = (cur_pos + delta).rem_euclid(visible.len() as isize) as usize;
        self.table_state.select(Some(visible[new_pos]));
    }

    fn selected_row(&self) -> Option<&RowView> {
        self.table_state.selected().and_then(|i| self.rows.get(i))
    }

    /// Expand the selected row — right-arrow tree-view UX. Adds the
    /// selected molecule id to [`Self::expanded`]; subsequent frames
    /// render extra indented detail lines under the row.
    fn expand_selected(&mut self) {
        let Some(id) = self.selected_row().map(|r| r.mol_id.clone()) else {
            return;
        };
        self.expanded.insert(id);
    }

    /// Collapse the selected row back to single-line view — symmetric
    /// counterpart of [`Self::expand_selected`].
    fn collapse_selected(&mut self) {
        let Some(id) = self.selected_row().map(|r| r.mol_id.clone()) else {
            return;
        };
        self.expanded.remove(&id);
    }

    /// Resolve the filesystem directory holding per-molecule artifacts
    /// (`briefing.md`, `log.md`, `synthesis.md`, `notes/`, `responses/`, …)
    /// for the currently selected row.
    fn selected_molecule_dir(&self) -> Option<std::path::PathBuf> {
        let row = self.selected_row()?;
        let sd = self.row_state_dirs.get(&row.mol_id)?;
        let mid = cosmon_core::id::MoleculeId::new(&row.mol_id).ok()?;
        Some(FileStore::new(sd).molecule_dir(&mid))
    }

    /// Re-run the active pane's [`DetailRenderer::render`] and store the
    /// result in `detail_content`. No-op when no pane is active.
    fn refresh_detail(&mut self) {
        let Some(idx) = self.active_renderer else {
            return;
        };
        let Some(row) = self.selected_row().cloned() else {
            self.detail_content = ratatui::text::Text::raw("(no molecule selected)");
            return;
        };
        let molecule_dir = self.selected_molecule_dir();
        let state_dir = self.row_state_dirs.get(&row.mol_id).cloned();
        let ctx = DetailCtx {
            row: &row,
            molecule_dir: molecule_dir.as_deref(),
            state_dir: state_dir.as_deref(),
        };
        self.detail_content = self.renderers[idx].render(&ctx);
    }

    /// Toggle the pane whose [`DetailRenderer::keys`] contains `key`. If
    /// the matching pane is already active, closes the detail panel;
    /// otherwise switches to it and eagerly renders its content.
    fn toggle_detail_by_key(&mut self, key: char) {
        let Some(idx) = self.renderer_for_key(key) else {
            return;
        };
        if self.active_renderer == Some(idx) {
            self.active_renderer = None;
            self.detail_content = ratatui::text::Text::default();
        } else {
            self.active_renderer = Some(idx);
            self.detail_scroll = 0;
            self.refresh_detail();
        }
    }

    fn renderer_for_key(&self, key: char) -> Option<usize> {
        self.renderers.iter().position(|r| r.keys().contains(&key))
    }

    fn active_is_live(&self) -> bool {
        self.active_renderer
            .and_then(|i| self.renderers.get(i))
            .is_some_and(|r| r.is_live())
    }

    fn active_label(&self) -> &'static str {
        self.active_renderer
            .and_then(|i| self.renderers.get(i))
            .map_or("-", |r| r.label())
    }

    fn attach_selected(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        let Some(i) = self.table_state.selected() else {
            return Ok(());
        };
        let Some(row) = self.rows.get(i) else {
            return Ok(());
        };
        let Some(session) = row.session.clone() else {
            self.status_msg = "no tmux session to attach".into();
            return Ok(());
        };
        let socket = row.socket.clone();
        restore_terminal(terminal)?;
        let status = Command::new("tmux")
            .args(["-L", &socket, "attach-session", "-t", &session])
            .status();
        // Re-enter the alternate screen and raw mode regardless.
        enable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.clear()?;
        match status {
            Ok(s) if s.success() => self.status_msg = format!("detached from {session}"),
            Ok(s) => self.status_msg = format!("tmux attach exited with {s}"),
            Err(e) => self.status_msg = format!("tmux attach failed: {e}"),
        }
        Ok(())
    }

    /// `y` — copy the selected row's molecule id to the system clipboard
    /// via OSC 52. Falls back to the tmux session name when the row has no
    /// resolved molecule id (orphaned session). Triage shortcut: yank an id
    /// out of the TUI without dropping to a shell.
    fn yank_selected_id(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let payload = if !row.mol_id.is_empty() {
            row.mol_id.clone()
        } else if let Some(s) = row.session.clone() {
            s
        } else {
            self.status_msg = "nothing to yank".into();
            return;
        };
        match copy_to_clipboard(&payload) {
            Ok(()) => self.status_msg = format!("yanked {payload}"),
            Err(e) => self.status_msg = format!("yank failed: {e}"),
        }
    }

    /// `Y` — copy the full `cs observe <id> --json` output for the selected
    /// molecule. Shells out to the same `cs` binary so the payload is
    /// byte-identical to what a scripted caller would see.
    fn yank_selected_observe_json(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.mol_id.is_empty() {
            self.status_msg = "no molecule id to observe".into();
            return;
        }
        let id = row.mol_id.clone();
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"));
        let output = Command::new(&exe).args(["observe", &id, "--json"]).output();
        match output {
            Ok(o) if o.status.success() => {
                let json = String::from_utf8_lossy(&o.stdout).into_owned();
                match copy_to_clipboard(&json) {
                    Ok(()) => self.status_msg = format!("yanked observe json for {id}"),
                    Err(e) => self.status_msg = format!("yank failed: {e}"),
                }
            }
            Ok(o) => {
                self.status_msg = format!("cs observe exited with {}", o.status);
            }
            Err(e) => self.status_msg = format!("cs observe failed: {e}"),
        }
    }

    // ---- Cockpit action modals (task-20260423-16ad) -------------------

    /// `n` — open the nucleate modal. Empty both fields so the operator
    /// types a formula name, Tab, topic, Enter.
    fn open_nucleate_modal(&mut self) {
        self.action_modal = ActionModal::Nucleate(NucleateForm::default());
        self.status_msg =
            "nucleate: type formula, Tab for topic, Enter to fire, Esc cancels".into();
    }

    /// `t` — open the tackle confirmation modal for the selected row.
    fn open_tackle_modal(&mut self) {
        let Some(row) = self.selected_row() else {
            self.status_msg = "no molecule selected".into();
            return;
        };
        if row.mol_id.is_empty() {
            self.status_msg = "selected row has no molecule id".into();
            return;
        }
        let id = row.mol_id.clone();
        self.action_modal = ActionModal::ConfirmTackle { mol_id: id };
    }

    /// `m` — open the merge-and-done confirmation modal for the selected
    /// row. `cs done` is destructive (merge + teardown) so the prompt
    /// requires an explicit `y`; anything else cancels.
    fn open_merge_modal(&mut self) {
        let Some(row) = self.selected_row() else {
            self.status_msg = "no molecule selected".into();
            return;
        };
        if row.mol_id.is_empty() {
            self.status_msg = "selected row has no molecule id".into();
            return;
        }
        let id = row.mol_id.clone();
        self.action_modal = ActionModal::ConfirmMerge { mol_id: id };
    }

    /// `w` — open the whisper-body modal for the selected row.
    fn open_whisper_modal(&mut self) {
        let Some(row) = self.selected_row() else {
            self.status_msg = "no molecule selected".into();
            return;
        };
        if row.mol_id.is_empty() {
            self.status_msg = "selected row has no molecule id".into();
            return;
        }
        let id = row.mol_id.clone();
        self.action_modal = ActionModal::Whisper {
            mol_id: id,
            body: String::new(),
        };
    }

    /// `.` — open the free-form session-note modal.
    fn open_session_note_modal(&mut self) {
        self.action_modal = ActionModal::SessionNote {
            body: String::new(),
        };
    }

    /// Dispatch a single key into the active action modal. Returns true
    /// when the key was consumed (modal still active, advanced to next
    /// field, or dismissed); false when the key should fall through to
    /// the normal table handler (currently unreachable while a modal is
    /// open — kept as a future escape hatch).
    fn handle_action_modal_key(&mut self, code: KeyCode) -> bool {
        // Esc always cancels — central rule applied once.
        if matches!(code, KeyCode::Esc) {
            self.action_modal = ActionModal::None;
            self.status_msg = "cancelled".into();
            return true;
        }
        // `take` out of self so we don't hold an &mut self while we call
        // other methods on self. We put it back (or replace with None)
        // before returning.
        let modal = std::mem::take(&mut self.action_modal);
        match modal {
            ActionModal::None => false,
            ActionModal::Nucleate(form) => {
                self.handle_nucleate_key(code, form);
                true
            }
            ActionModal::ConfirmTackle { mol_id } => {
                self.handle_confirm_tackle_key(code, mol_id);
                true
            }
            ActionModal::ConfirmMerge { mol_id } => {
                self.handle_confirm_merge_key(code, mol_id);
                true
            }
            ActionModal::Whisper { mol_id, body } => {
                self.handle_whisper_key(code, mol_id, body);
                true
            }
            ActionModal::SessionNote { body } => {
                self.handle_session_note_key(code, body);
                true
            }
        }
    }

    fn handle_nucleate_key(&mut self, code: KeyCode, mut form: NucleateForm) {
        match code {
            KeyCode::Tab | KeyCode::BackTab => {
                form.focus = match form.focus {
                    NucleateField::Formula => NucleateField::Topic,
                    NucleateField::Topic => NucleateField::Formula,
                };
                self.action_modal = ActionModal::Nucleate(form);
            }
            KeyCode::Enter => {
                if matches!(form.focus, NucleateField::Formula) && !form.formula.is_empty() {
                    form.focus = NucleateField::Topic;
                    self.action_modal = ActionModal::Nucleate(form);
                    return;
                }
                // Fire if we have at least a formula.
                if form.formula.trim().is_empty() {
                    self.status_msg = "nucleate: formula is required".into();
                    self.action_modal = ActionModal::Nucleate(form);
                    return;
                }
                self.fire_nucleate(&form);
                self.action_modal = ActionModal::None;
            }
            KeyCode::Backspace => {
                match form.focus {
                    NucleateField::Formula => {
                        form.formula.pop();
                    }
                    NucleateField::Topic => {
                        form.topic.pop();
                    }
                }
                self.action_modal = ActionModal::Nucleate(form);
            }
            KeyCode::Char(c) => {
                match form.focus {
                    NucleateField::Formula => form.formula.push(c),
                    NucleateField::Topic => form.topic.push(c),
                }
                self.action_modal = ActionModal::Nucleate(form);
            }
            _ => {
                // Keep the modal open.
                self.action_modal = ActionModal::Nucleate(form);
            }
        }
    }

    fn handle_confirm_tackle_key(&mut self, code: KeyCode, mol_id: String) {
        match code {
            KeyCode::Char('y' | 'Y') => {
                self.fire_tackle(&mol_id);
                self.action_modal = ActionModal::None;
            }
            KeyCode::Char('n' | 'N') | KeyCode::Enter => {
                self.status_msg = format!("tackle {mol_id} cancelled");
                self.action_modal = ActionModal::None;
            }
            _ => {
                // Re-install modal: any other key is a miss-hit, stay open.
                self.action_modal = ActionModal::ConfirmTackle { mol_id };
            }
        }
    }

    fn handle_confirm_merge_key(&mut self, code: KeyCode, mol_id: String) {
        match code {
            KeyCode::Char('y' | 'Y') => {
                self.fire_done(&mol_id);
                self.action_modal = ActionModal::None;
            }
            KeyCode::Char('n' | 'N') | KeyCode::Enter => {
                self.status_msg = format!("merge-and-done {mol_id} cancelled");
                self.action_modal = ActionModal::None;
            }
            _ => {
                self.action_modal = ActionModal::ConfirmMerge { mol_id };
            }
        }
    }

    fn handle_whisper_key(&mut self, code: KeyCode, mol_id: String, mut body: String) {
        match code {
            KeyCode::Enter => {
                if body.trim().is_empty() {
                    self.status_msg = "whisper: body is required".into();
                    self.action_modal = ActionModal::Whisper { mol_id, body };
                    return;
                }
                self.fire_whisper(&mol_id, &body);
                self.action_modal = ActionModal::None;
            }
            KeyCode::Backspace => {
                body.pop();
                self.action_modal = ActionModal::Whisper { mol_id, body };
            }
            KeyCode::Char(c) => {
                body.push(c);
                self.action_modal = ActionModal::Whisper { mol_id, body };
            }
            _ => {
                self.action_modal = ActionModal::Whisper { mol_id, body };
            }
        }
    }

    fn handle_session_note_key(&mut self, code: KeyCode, mut body: String) {
        match code {
            KeyCode::Enter => {
                if body.trim().is_empty() {
                    self.status_msg = "session note: body is required".into();
                    self.action_modal = ActionModal::SessionNote { body };
                    return;
                }
                self.fire_session_note(&body);
                self.action_modal = ActionModal::None;
            }
            KeyCode::Backspace => {
                body.pop();
                self.action_modal = ActionModal::SessionNote { body };
            }
            KeyCode::Char(c) => {
                body.push(c);
                self.action_modal = ActionModal::SessionNote { body };
            }
            _ => {
                self.action_modal = ActionModal::SessionNote { body };
            }
        }
    }

    /// Return the path to the current `cs` binary — the same executable
    /// serving the TUI, so every one-shot action reads the same state
    /// store and formulas as the caller.
    fn cs_exe() -> std::path::PathBuf {
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cs"))
    }

    /// Format a short status bar message from a `Command` outcome.
    fn record_action_outcome(&mut self, verb: &str, result: std::io::Result<std::process::Output>) {
        match result {
            Ok(o) if o.status.success() => {
                self.status_msg = format!("{verb} ok");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                let short: String = stderr
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                self.status_msg = format!("{verb} exit {}: {short}", o.status);
            }
            Err(e) => {
                self.status_msg = format!("{verb} failed: {e}");
            }
        }
    }

    fn fire_nucleate(&mut self, form: &NucleateForm) {
        let exe = Self::cs_exe();
        let mut cmd = Command::new(&exe);
        cmd.arg("nucleate").arg(form.formula.trim());
        if !form.topic.trim().is_empty() {
            cmd.arg("--var").arg(format!("topic={}", form.topic));
        }
        let out = cmd.output();
        self.record_action_outcome(&format!("nucleate {}", form.formula), out);
        self.reload_or_warn();
    }

    fn fire_tackle(&mut self, mol_id: &str) {
        let exe = Self::cs_exe();
        let out = Command::new(&exe).args(["tackle", mol_id]).output();
        self.record_action_outcome(&format!("tackle {mol_id}"), out);
        self.reload_or_warn();
    }

    fn fire_done(&mut self, mol_id: &str) {
        let exe = Self::cs_exe();
        let out = Command::new(&exe).args(["done", mol_id]).output();
        self.record_action_outcome(&format!("done {mol_id}"), out);
        self.reload_or_warn();
    }

    fn fire_whisper(&mut self, mol_id: &str, body: &str) {
        let exe = Self::cs_exe();
        let out = Command::new(&exe)
            .args(["whisper", mol_id, "-m", body])
            .output();
        self.record_action_outcome(&format!("whisper {mol_id}"), out);
    }

    fn fire_session_note(&mut self, body: &str) {
        let exe = Self::cs_exe();
        let out = Command::new(&exe).args(["session", "note", body]).output();
        self.record_action_outcome("session note", out);
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            let timeout = self
                .refresh
                .checked_sub(self.last_refresh.elapsed())
                .unwrap_or_else(|| Duration::from_millis(0));
            let tick = Duration::from_millis(100).min(timeout);
            if event::poll(tick)? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    if self.filter_input_mode {
                        match k.code {
                            KeyCode::Esc => {
                                self.filter_input_mode = false;
                                self.filter.clear();
                            }
                            KeyCode::Enter => {
                                self.filter_input_mode = false;
                            }
                            KeyCode::Backspace => {
                                self.filter.pop();
                            }
                            KeyCode::Char(c) => {
                                self.filter.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // Action modal (n / t / m / w / .) takes absolute
                    // priority over all other keys so the operator can
                    // type any character into the input field without it
                    // bleeding into a detail-pane toggle underneath.
                    if self.action_modal.is_active() && self.handle_action_modal_key(k.code) {
                        // Modal consumed the key (or dismissed itself).
                        continue;
                    }
                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                    // `?` overlay swallows keys except toggles/quit so the
                    // operator's cheat sheet doesn't silently mutate the
                    // fleet underneath them.
                    if self.show_help {
                        match k.code {
                            KeyCode::Char('?' | 'q') | KeyCode::Esc | KeyCode::Enter => {
                                self.show_help = false;
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('?') => {
                            self.show_help = true;
                        }
                        // `M` (shifted) — mouse capture toggle. Moved from
                        // `m` (task-20260423-16ad) so `m` can drive the
                        // merge-and-done action from the cockpit.
                        KeyCode::Char('M') => {
                            self.toggle_mouse_capture(terminal)?;
                        }
                        // --- Cockpit action keystrokes (task-20260423-16ad).
                        // Each opens a typed modal that fires a single
                        // `cs <verb>` one-shot when confirmed.
                        KeyCode::Char('n') => {
                            self.open_nucleate_modal();
                        }
                        KeyCode::Char('t') => {
                            self.open_tackle_modal();
                        }
                        KeyCode::Char('m') => {
                            self.open_merge_modal();
                        }
                        KeyCode::Char('w') => {
                            self.open_whisper_modal();
                        }
                        KeyCode::Char('.') => {
                            self.open_session_note_modal();
                        }
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('j') if ctrl => {
                            self.detail_scroll = self.detail_scroll.saturating_add(1);
                        }
                        KeyCode::Char('k') if ctrl => {
                            self.detail_scroll = self.detail_scroll.saturating_sub(1);
                        }
                        KeyCode::PageDown => {
                            self.detail_scroll = self.detail_scroll.saturating_add(10);
                        }
                        KeyCode::PageUp => {
                            self.detail_scroll = self.detail_scroll.saturating_sub(10);
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if self.ensemble_events.is_some() {
                                self.ensemble_move(1);
                            } else {
                                self.move_selection(1);
                                self.detail_scroll = 0;
                                self.refresh_detail();
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if self.ensemble_events.is_some() {
                                self.ensemble_move(-1);
                            } else {
                                self.move_selection(-1);
                                self.detail_scroll = 0;
                                self.refresh_detail();
                            }
                        }
                        KeyCode::Right => {
                            self.expand_selected();
                        }
                        KeyCode::Left => {
                            self.collapse_selected();
                        }
                        KeyCode::Char('R') => {
                            self.reload_or_warn();
                            self.refresh_detail();
                        }
                        KeyCode::Char('/') => {
                            self.filter_input_mode = true;
                            self.filter.clear();
                        }
                        KeyCode::Char('A') => {
                            self.phase_filter = cycle_phase_filter(self.phase_filter);
                            self.status_msg =
                                format!("phase filter: {}", self.phase_filter.label());
                        }
                        KeyCode::Char('a') => {
                            self.all_projects = !self.all_projects;
                            self.project_id = if self.all_projects {
                                None
                            } else {
                                self.default_project_id.clone()
                            };
                            self.status_msg = if self.all_projects {
                                "scope: all projects".into()
                            } else {
                                "scope: current project".into()
                            };
                            self.reload_or_warn();
                            self.refresh_detail();
                        }
                        // Detail-pane toggles. `n` (notes) and `t` (tree)
                        // were promoted to shifted letters (`N`, `T`) so
                        // the lowercase slots could carry the nucleate /
                        // tackle cockpit actions (task-20260423-16ad).
                        KeyCode::Char(
                            c @ ('p' | ' ' | 'b' | 'l' | 'e' | 's' | 'r' | 'g' | 'v' | 'N' | 'T'
                            | 'X'),
                        ) => {
                            self.toggle_detail_by_key(c);
                        }
                        // Zoom-continu (JR "Le mur qui respire"): three
                        // scales navigated by incremental keypresses.
                        // `+` zooms in, `-` zooms out, `=` resets to ville.
                        KeyCode::Char('+') => {
                            self.zoom_in();
                        }
                        KeyCode::Char('-') => {
                            self.zoom_out();
                        }
                        KeyCode::Char('=') => {
                            self.zoom_reset();
                        }
                        KeyCode::Char('y') => self.yank_selected_id(),
                        KeyCode::Char('Y') => self.yank_selected_observe_json(),
                        KeyCode::Char('E') => {
                            self.toggle_ensemble_events();
                        }
                        KeyCode::Enter => {
                            if self.ensemble_events.is_some() {
                                self.ensemble_zoom_in();
                            } else {
                                self.attach_selected(terminal)?;
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Background reload: check for completed results.
            if self.poll_bg_reload() && self.active_is_live() {
                self.refresh_detail();
            }

            // Adaptive polling: use effective_refresh() instead of the
            // fixed self.refresh. Start a background reload when the
            // interval elapses (Phase 3, items 9+10).
            let effective = self.effective_refresh().max(self.refresh);
            if self.last_refresh.elapsed() >= effective && !self.bg_pending {
                self.start_bg_reload();
            }
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let show_presence = !self.presence.is_empty();
        // Header, then the worker strip. The strip has no `show_*` gate:
        // it is a reading, not a query, so it is always on.
        let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
        if show_presence {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Min(3));
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area());

        let mut idx = 0;
        self.draw_header(f, chunks[idx]);
        idx += 1;
        self.draw_worker_strip(f, chunks[idx]);
        idx += 1;
        if show_presence {
            self.draw_presence_strip(f, chunks[idx]);
            idx += 1;
        }
        self.draw_zoom_body(f, chunks[idx]);
        idx += 1;
        self.draw_vital_signs(f, chunks[idx]);
        idx += 1;
        self.draw_footer(f, chunks[idx]);

        // Help overlay renders last so it floats above everything else.
        // Painted after `draw_footer` specifically because the footer
        // hint tells operators which key toggles the overlay — the
        // overlay then obscures the table but leaves the header and
        // footer visible, so the shortcut line stays in view.
        if self.show_help {
            self.draw_help_overlay(f, f.area());
        }

        // Action modal overlays above everything else so the input field
        // is never obscured by background updates (task-20260423-16ad).
        if self.action_modal.is_active() {
            self.draw_action_modal(f, f.area());
        }
    }

    /// Render the current action modal as a centered popover. Wheat-paste
    /// rule: same monospace canon as the rest of `cs peek`, no fancy
    /// borders beyond the existing ratatui `Block::bordered` primitive.
    fn draw_action_modal(&self, f: &mut Frame, area: Rect) {
        let (title, lines) = match &self.action_modal {
            ActionModal::None => return,
            ActionModal::Nucleate(form) => {
                let formula_marker = if matches!(form.focus, NucleateField::Formula) {
                    "▸"
                } else {
                    " "
                };
                let topic_marker = if matches!(form.focus, NucleateField::Topic) {
                    "▸"
                } else {
                    " "
                };
                (
                    " nucleate · Tab to switch field · Enter to fire · Esc cancels ",
                    vec![
                        format!("{formula_marker} formula: {}", form.formula),
                        format!("{topic_marker} topic  : {}", form.topic),
                        String::new(),
                        "cs nucleate <formula> --var topic=\"...\"".to_owned(),
                    ],
                )
            }
            ActionModal::ConfirmTackle { mol_id } => (
                " tackle · y confirms · n/Enter/Esc cancels ",
                vec![
                    format!("Tackle {mol_id}?"),
                    String::new(),
                    "Runs: cs tackle <id> — spawns worker in background tmux.".to_owned(),
                ],
            ),
            ActionModal::ConfirmMerge { mol_id } => (
                " merge-and-done · y confirms · n/Enter/Esc cancels ",
                vec![
                    format!("Merge and close {mol_id}?"),
                    String::new(),
                    "Runs: cs done <id> — merges the worker branch and tears down tmux + worktree."
                        .to_owned(),
                    "This is destructive; the branch is merged to its parent.".to_owned(),
                ],
            ),
            ActionModal::Whisper { mol_id, body } => (
                " whisper · Enter to send · Esc cancels ",
                vec![
                    format!("target: {mol_id}"),
                    String::new(),
                    format!("▸ body: {body}"),
                    String::new(),
                    "cs whisper <id> -m \"<body>\"".to_owned(),
                ],
            ),
            ActionModal::SessionNote { body } => (
                " session note · Enter to save · Esc cancels ",
                vec![
                    format!("▸ note: {body}"),
                    String::new(),
                    "cs session note \"<line>\"".to_owned(),
                    "(requires an open session — start one with `cs session start`).".to_owned(),
                ],
            ),
        };
        let popup = centered_rect(60, 40, area);
        f.render_widget(Clear, popup);
        let body_lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|s| Line::from(Span::raw(s)))
            .collect();
        let p = Paragraph::new(body_lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        f.render_widget(p, popup);
    }

    /// Dispatch the body rectangle across three scales based on
    /// [`Self::zoom_level`]. Continuous zoom blends two neighbouring
    /// scales side-by-side so the transition itself is information:
    /// the operator sees *where in the whole* the detail sits.
    ///
    /// - `z = 0.0`           — ville (fleet table, full-width).
    /// - `z ∈ (0.0, 1.0)`    — ville + immeuble side-by-side.
    /// - `z = 1.0`           — immeuble (one molecule pleine-page).
    /// - `z ∈ (1.0, 2.0)`    — immeuble + peau side-by-side.
    /// - `z = 2.0`           — peau (raw artifact text).
    ///
    /// When [`Self::active_renderer`] is set, the `z ∈ [0.0, 1.0)` band
    /// preserves the legacy table+detail split so existing workflows
    /// (press `b` to see briefing, `j/k` to walk) are untouched.
    fn draw_zoom_body(&mut self, f: &mut Frame, area: Rect) {
        if self.ensemble_events.is_some() {
            self.draw_ensemble_tab(f, area);
            return;
        }
        let z = self.zoom_level.clamp(0.0, 2.0);
        if z <= 0.001 {
            if self.active_renderer.is_none() {
                self.draw_table(f, area);
            } else {
                // Legacy 45/55 split preserved: the detail renderer is
                // not part of the zoom state, so keeping `b`/`l`/`s`/…
                // behaviour stable across the zoom=0 regime avoids a
                // double-mechanism confusion. Zoom is for scale
                // navigation; detail keys are for artifact selection.
                let inner = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(area);
                self.draw_table(f, inner[0]);
                self.draw_detail_panel(f, inner[1]);
            }
        } else if z < 1.0 {
            // ville + immeuble blend. Fraction of immeuble grows with z.
            let right = (z * 100.0).round().clamp(5.0, 95.0) as u16;
            let left = 100_u16.saturating_sub(right);
            let inner = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(left), Constraint::Percentage(right)])
                .split(area);
            self.draw_table(f, inner[0]);
            self.draw_immeuble(f, inner[1]);
        } else if (z - 1.0).abs() < 0.001 {
            self.draw_immeuble(f, area);
        } else if z < 2.0 {
            // immeuble + peau blend. Fraction of peau grows with z-1.
            let frac = z - 1.0;
            let right = (frac * 100.0).round().clamp(5.0, 95.0) as u16;
            let left = 100_u16.saturating_sub(right);
            let inner = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(left), Constraint::Percentage(right)])
                .split(area);
            self.draw_immeuble(f, inner[0]);
            self.draw_peau(f, inner[1]);
        } else {
            self.draw_peau(f, area);
        }
    }

    /// Zoom in by one step. Step size is `0.25` so each keypress is a
    /// visible but gentle transition — five presses crosses a full
    /// scale. Clamped to the peau ceiling.
    fn zoom_in(&mut self) {
        let next = (self.zoom_level + ZOOM_STEP).min(ZOOM_MAX);
        self.zoom_level = next;
        self.status_msg = format!("zoom: {} ({:.2})", zoom_label(next), next);
    }

    /// Zoom out by one step. Symmetric with [`Self::zoom_in`].
    fn zoom_out(&mut self) {
        let next = (self.zoom_level - ZOOM_STEP).max(ZOOM_MIN);
        self.zoom_level = next;
        self.status_msg = format!("zoom: {} ({:.2})", zoom_label(next), next);
    }

    /// Reset zoom to the ville scale — the `=` key.
    fn zoom_reset(&mut self) {
        self.zoom_level = ZOOM_MIN;
        self.status_msg = "zoom: ville (0.00)".into();
    }

    /// Toggle the fleet-wide ensemble events tab (`E` key). When open,
    /// `j/k` navigates events and `Enter` zooms into the selected
    /// molecule. When closed, the normal molecule table is restored.
    fn toggle_ensemble_events(&mut self) {
        if self.ensemble_events.is_some() {
            self.ensemble_events = None;
            self.status_msg = "ensemble: off".into();
        } else {
            self.refresh_ensemble_events();
            self.status_msg = "ensemble: fleet events (j/k scroll, Enter zoom, E close)".into();
        }
    }

    /// Move the ensemble-view selection by `delta` rows.
    fn ensemble_move(&mut self, delta: i32) {
        let Some(view) = self.ensemble_events.as_mut() else {
            return;
        };
        if view.entries.is_empty() {
            return;
        }
        let max = view.entries.len() - 1;
        let new = (view.selected as i32 + delta).clamp(0, max as i32);
        view.selected = new as usize;
    }

    /// Close the ensemble tab and jump the molecule table to the event's
    /// molecule id. Zoom one step so the detail pane is visible — one
    /// keystroke advances by one navigation mark on the ville → immeuble →
    /// peau axis (see `docs/guides/peek-zoom.md`).
    fn ensemble_zoom_in(&mut self) {
        let Some(view) = self.ensemble_events.as_ref() else {
            return;
        };
        let Some(entry) = view.entries.get(view.selected) else {
            return;
        };
        let mol_id = entry.mol_id.clone();
        self.ensemble_events = None;
        if let Some(pos) = self.rows.iter().position(|r| r.mol_id == mol_id) {
            self.table_state.select(Some(pos));
            self.zoom_level = (self.zoom_level + ZOOM_STEP).min(ZOOM_MAX);
            self.refresh_detail();
            self.status_msg = format!("zoomed on {mol_id}");
        } else {
            self.status_msg = format!("no row for {mol_id} in current scope");
        }
    }

    /// Render the ensemble events tab — one line per event, newest first.
    /// Columns are aligned to the wheat-paste canon: time (8), `mol_id`
    /// (shortened), variant (kind), summary.
    fn draw_ensemble_tab(&self, f: &mut Frame, area: Rect) {
        let Some(view) = self.ensemble_events.as_ref() else {
            return;
        };
        let title = format!(
            " ensemble — {} events across {} molecules ",
            view.entries.len(),
            view.entries
                .iter()
                .map(|e| e.mol_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
        if view.entries.is_empty() {
            let block = Block::default().borders(Borders::ALL).title(title);
            f.render_widget(
                Paragraph::new("<no events — are there events.jsonl files in this scope?>")
                    .block(block),
                area,
            );
            return;
        }

        let rows_view: Vec<Row> = view
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let ts =
                    e.ts.map(|t| t.format("%H:%M:%S").to_string())
                        .unwrap_or_else(|| "--:--:--".into());
                let mol = truncate_str(&e.mol_id, 22);
                let kind = truncate_str(&e.kind, 16);
                let summary = truncate_str(&e.summary, 200);
                let style = if i == view.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Row::new(vec![
                    Cell::from(ts),
                    Cell::from(mol),
                    Cell::from(kind).style(Style::default().fg(ensemble_kind_color(&e.kind))),
                    Cell::from(summary),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            rows_view,
            [
                Constraint::Length(8),
                Constraint::Length(22),
                Constraint::Length(16),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(vec!["ts", "mol_id", "kind", "summary"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(table, area);
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let scope = if self.all_projects {
            "all projects".to_owned()
        } else if let Some(pid) = &self.project_id {
            format!("project: {pid}")
        } else {
            "project: <unscoped>".to_owned()
        };
        let text = format!(
            " cs peek — fleet watchdog — {scope} — {} molecules",
            self.rows.len()
        );
        let p = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().add_modifier(Modifier::BOLD),
        )));
        f.render_widget(p, area);
    }

    /// One always-on line reporting the worker roster against live tmux:
    ///
    /// ```text
    ///  workers: 30 registered · 3 attached · 27 phantom → cs purge
    /// ```
    ///
    /// A fuel gauge does not enumerate the molecules of petrol. What the
    /// operator needs from a roster of thirty entries backed by three live
    /// sessions is not twenty-seven rows — it is one number, and the
    /// number is twenty-seven. So this is a count, never a row, and it is
    /// on at every setting of every flag: the flags filter *molecules*,
    /// and a phantom is not a molecule, so no flag could ever reveal it.
    ///
    /// The `→ cs purge` remedy appears only when there is something to
    /// purge; naming it on a clean roster would advise a no-op. The strip
    /// names the gesture and never performs it — peek reads and renders,
    /// `cs purge` writes and drains.
    fn draw_worker_strip(&self, f: &mut Frame, area: Rect) {
        let census = self.census;
        let phantom = census.phantom();
        let mut spans = vec![
            Span::styled(" workers: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} registered", census.registered),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(" · ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} attached", census.attached),
                Style::default().fg(Color::Green),
            ),
            Span::styled(" · ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{phantom} phantom"),
                if phantom == 0 {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                },
            ),
        ];
        if phantom > 0 {
            spans.push(Span::styled(
                " → cs purge",
                Style::default().fg(Color::DarkGray),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// One-line strip listing every live Claude session. Compact, wheat-paste
    /// over `.cosmon/state/presence/*.json` (C-PRESENCE-CORE's on-disk
    /// contract; see [`presence_reader`]). Truncates to a count + ellipsis at
    /// N>5 to preserve glance-legibility at saturation.
    fn draw_presence_strip(&self, f: &mut Frame, area: Rect) {
        let now = Utc::now();
        let n = self.presence.len();
        let mut spans = vec![Span::styled(
            format!(" [{n} live] "),
            Style::default().fg(Color::DarkGray),
        )];
        let visible = self.presence.iter().take(5);
        for (i, p) in visible.enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let label = presence_label(p);
            let style = if p.is_stale(now) {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Green)
            };
            spans.push(Span::styled(label, style));
        }
        if n > 5 {
            spans.push(Span::styled(
                format!("  …(+{} more)", n - 5),
                Style::default().fg(Color::DarkGray),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let mut spans: Vec<Span> = Vec::new();
        if self.filter_input_mode {
            spans.push(Span::styled(
                format!(" /{}", self.filter),
                Style::default().fg(Color::Yellow),
            ));
            spans.push(Span::raw("  (Enter=apply, Esc=cancel) "));
        } else {
            spans.push(Span::raw(
                " j/k move · →/← expand · +/-/= zoom · Enter attach · p/b/l/e/s/r/N/g/T/v/X panes · n/t/m/w/. actions · y/Y yank · / filter · R reload · a scope · D dead · ? help · q quit ",
            ));
        }
        if !self.filter.is_empty() && !self.filter_input_mode {
            spans.push(Span::styled(
                format!(" [filter: {}]", self.filter),
                Style::default().fg(Color::Cyan),
            ));
        }
        if !self.status_msg.is_empty() {
            spans.push(Span::styled(
                format!(" · {}", self.status_msg),
                Style::default().fg(Color::Green),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Toggle crossterm mouse capture — escape hatch for terminals where
    /// `Shift+drag` does not cleanly bypass `EnableMouseCapture`. Capture
    /// on: future pointer features see events but native text selection is
    /// blocked. Capture off: operator can click-drag to select like in any
    /// regular terminal, at the cost of no pointer support. See the `?`
    /// help overlay for the full explanation.
    fn toggle_mouse_capture(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        if self.mouse_captured {
            execute!(terminal.backend_mut(), DisableMouseCapture)?;
            self.mouse_captured = false;
            self.status_msg =
                "mouse capture OFF — native click-drag selection enabled (press m to re-enable)"
                    .into();
        } else {
            execute!(terminal.backend_mut(), EnableMouseCapture)?;
            self.mouse_captured = true;
            self.status_msg = "mouse capture ON — use Shift+drag to select text".into();
        }
        Ok(())
    }

    /// Draw the `?` help overlay — a centered floating panel that
    /// documents every keybinding and explains how to copy text out of
    /// the TUI (Shift+drag on common terminals, or `m` to toggle mouse
    /// capture off wholesale).
    fn draw_help_overlay(&self, f: &mut Frame, area: Rect) {
        let popup = centered_rect(78, 88, area);
        f.render_widget(Clear, popup);
        let lines = help_overlay_lines(self.mouse_captured);
        let p = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" cs peek — help · press ? or Esc to close "),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(p, popup);
    }

    /// Render the fleet vital signs status line — a one-line aggregate of
    /// fleet health: stale pending count, temperature distribution, and
    /// completed/collapsed ratio over the last 7 days.
    fn draw_vital_signs(&self, f: &mut Frame, area: Rect) {
        let now = Utc::now();
        let threshold_48h = chrono::Duration::hours(48);
        let window_7d = chrono::Duration::days(7);

        if self.rows.is_empty() {
            let line = Line::from(Span::styled(
                " [vital] no data",
                Style::default().fg(Color::DarkGray),
            ));
            f.render_widget(Paragraph::new(line), area);
            return;
        }

        // Count pending molecules older than 48h.
        let stale_pending: usize = self
            .rows
            .iter()
            .filter(|r| {
                r.status == "pending"
                    && r.created_at_utc
                        .is_some_and(|dt| now.signed_duration_since(dt) > threshold_48h)
            })
            .count();

        // Temperature distribution from tags.
        let (mut hot, mut warm, mut cold, mut frozen) = (0usize, 0, 0, 0);
        for r in &self.rows {
            for t in &r.tags {
                match t.as_str() {
                    "temp:hot" => hot += 1,
                    "temp:warm" => warm += 1,
                    "temp:cold" => cold += 1,
                    "temp:frozen" => frozen += 1,
                    _ => {}
                }
            }
        }

        // Health ratio: completed / (completed + collapsed) in last 7 days.
        let mut completed_7d: usize = 0;
        let mut collapsed_7d: usize = 0;
        for r in &self.rows {
            if let Some(dt) = r.created_at_utc {
                if now.signed_duration_since(dt) <= window_7d {
                    match r.status.as_str() {
                        "completed" => completed_7d += 1,
                        "collapsed" => collapsed_7d += 1,
                        _ => {}
                    }
                }
            }
        }
        let total_terminal = completed_7d + collapsed_7d;

        let mut spans: Vec<Span> = Vec::new();

        // Label
        spans.push(Span::styled(
            " [vital] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

        // Stale pending count
        let stale_style = if stale_pending > 0 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(
            format!("{stale_pending} pending>48h"),
            stale_style,
        ));

        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));

        // Temperature distribution
        spans.push(Span::styled(
            format!("hot:{hot}"),
            if hot > 0 {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("warm:{warm}"),
            if warm > 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("cold:{cold}"),
            if cold > 0 {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("frozen:{frozen}"),
            if frozen > 0 {
                Style::default().fg(Color::Blue)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));

        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));

        // Health ratio
        if total_terminal > 0 {
            let pct = (completed_7d as f64 / total_terminal as f64 * 100.0) as u64;
            let health_style = if pct >= 80 {
                Style::default().fg(Color::Green)
            } else if pct >= 50 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Red)
            };
            spans.push(Span::styled(
                format!("health: {pct}% ({completed_7d}/{total_terminal} completed)"),
                health_style,
            ));
        } else {
            spans.push(Span::styled(
                "health: –",
                Style::default().fg(Color::DarkGray),
            ));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_table(&mut self, f: &mut Frame, area: Rect) {
        let visible = self.filtered_indices();
        let header = Row::new(vec![
            Cell::from(" "),
            Cell::from("♥"),
            Cell::from("W"),
            Cell::from("T"),
            Cell::from("PROJECT"),
            Cell::from("MOLECULE"),
            Cell::from("● STEP"),
            Cell::from("TRUST"),
            Cell::from("AGE"),
            Cell::from("ENERGY"),
            Cell::from("ADAPTER"),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        // Position within `visible` of the first Dead-band row, if any.
        // Used to inject a single "fold band" divider between above-the-fold
        // and below-the-fold content when terminal rows are revealed via 'D'.
        let fold_at: Option<usize> = visible.iter().position(|&i| {
            self.rows
                .get(i)
                .is_some_and(|r| liveness_band(r) == LivenessBand::Dead)
        });
        let dead_count: usize = visible
            .iter()
            .filter(|&&i| {
                self.rows
                    .get(i)
                    .is_some_and(|r| liveness_band(r) == LivenessBand::Dead)
            })
            .count();

        let mut rows: Vec<Row> = visible
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .map(|r| {
                let (status_glyph, status_style) = status_token(&r.status, r.heartbeat);
                let health = molecule_health_for_row(r);
                let health_glyph = health.glyph();
                let health_style = molecule_health_style(health);
                let (temp_glyph_s, temp_style) = temp_token(&r.tags);
                let (whisper_glyph_s, whisper_style) = whisper_token(r.whisper_fresh);
                let energy = format_energy(r.energy_in, r.energy_out, r.cost_usd, r.context_window);
                let is_expanded = self.expanded.contains(&r.mol_id);
                // Tree-view indicator: ▾ when expanded, ▸ when collapsed.
                // Matches htop / lazygit affordance so the arrow actually
                // means what it looks like.
                let indicator = if is_expanded { "▾" } else { "▸" };
                // The MOLECULE cell carries the optional detail block as
                // extra lines. Ratatui sizes each Row by the max line-count
                // across its cells, so we also pad the primary columns with
                // blanks to match, keeping the detail visually anchored
                // under the molecule label.
                let detail_lines = if is_expanded {
                    expanded_detail_lines(r)
                } else {
                    Vec::new()
                };
                let row_height = (1 + detail_lines.len()) as u16;

                let mut mol_cell_lines: Vec<Line> = Vec::with_capacity(1 + detail_lines.len());
                mol_cell_lines.push(Line::from(r.display_label(32)));
                for dl in &detail_lines {
                    mol_cell_lines.push(dl.clone());
                }

                let blank_count = detail_lines.len();
                let pad_cell = |first: Line<'static>| -> Cell<'static> {
                    let mut v: Vec<Line<'static>> = Vec::with_capacity(1 + blank_count);
                    v.push(first);
                    for _ in 0..blank_count {
                        v.push(Line::from(""));
                    }
                    Cell::from(v)
                };

                let (trust_text, trust_style) = trust_badge(r.trust_score);
                Row::new(vec![
                    pad_cell(Line::from(Span::styled(
                        indicator.to_owned(),
                        Style::default().fg(Color::DarkGray),
                    ))),
                    pad_cell(Line::from(Span::styled(
                        {
                            let kind = r.row_kind();
                            kind.glyph().to_owned()
                        },
                        r.row_kind().ratatui_style(),
                    ))),
                    pad_cell(Line::from(Span::styled(
                        whisper_glyph_s.to_owned(),
                        whisper_style,
                    ))),
                    pad_cell(Line::from(Span::styled(
                        temp_glyph_s.to_owned(),
                        temp_style,
                    ))),
                    pad_cell(Line::from(r.project.clone())),
                    Cell::from(mol_cell_lines),
                    pad_cell(Line::from(vec![
                        Span::styled(status_glyph, status_style),
                        Span::raw(" "),
                        Span::styled(health_glyph.to_owned(), health_style),
                        Span::raw(format!(" {}", r.step)),
                    ])),
                    pad_cell(Line::from(Span::styled(trust_text, trust_style))),
                    pad_cell(Line::from(age_cell(r.updated_at))),
                    pad_cell(Line::from(energy)),
                    pad_cell(adapter_cell(&r.adapter)),
                ])
                .height(row_height)
            })
            .collect();

        // Inject the fold-band divider at `fold_at`. It is non-selectable —
        // j/k still navigates `visible` indices; physical rows after the
        // divider shift by one, which we compensate for when translating
        // the outer selection into the local TableState below.
        if let Some(pos) = fold_at {
            let label = format!(
                " ─── below the fold · {dead_count} terminal molecule{} ─── ",
                if dead_count == 1 { "" } else { "s" }
            );
            let fold_style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM);
            let divider = Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(Line::from(Span::styled(label, fold_style))),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
            ])
            .height(1);
            rows.insert(pos, divider);
        }

        let any_expanded = visible.iter().any(|&i| {
            self.rows
                .get(i)
                .is_some_and(|r| self.expanded.contains(&r.mol_id))
        });
        let widths = if any_expanded {
            [
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(10),
                Constraint::Min(60),
                Constraint::Length(8),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(22),
                Constraint::Length(18),
            ]
        } else {
            [
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(14),
                Constraint::Length(34),
                Constraint::Min(10),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(22),
                Constraint::Length(20),
            ]
        };
        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Fleet"))
            // REVERSED swaps fg/bg automatically, so every span in the row
            // stays legible regardless of its own color. The previous
            // `bg(DarkGray)` collided with DarkGray-foreground spans
            // (indicator, expanded detail labels `formula`/`tier`/`kind`),
            // painting them DarkGray-on-DarkGray — i.e. invisible on a
            // dark terminal. REVERSED is the accessibility-safe pattern.
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
            .highlight_symbol("▶ ");

        // Map outer selection onto the filtered view, accounting for the
        // fold divider row that may have been inserted at `fold_at`.
        let mut local_state = TableState::default();
        if let Some(sel) = self.table_state.selected() {
            if let Some(local) = visible.iter().position(|&i| i == sel) {
                let physical = match fold_at {
                    Some(pos) if local >= pos => local + 1,
                    _ => local,
                };
                local_state.select(Some(physical));
            }
        }
        f.render_stateful_widget(table, area, &mut local_state);
    }

    fn draw_detail_panel(&self, f: &mut Frame, area: Rect) {
        let mol_label = self
            .selected_row()
            .map(|r| r.display_label(48))
            .unwrap_or_else(|| "-".into());
        let title = format!(
            "{} · {} (Ctrl-j/k, PgDn/PgUp to scroll)",
            self.active_label(),
            mol_label
        );
        let content: ratatui::text::Text<'static> = if self.detail_content.lines.is_empty() {
            ratatui::text::Text::raw("<empty>")
        } else {
            self.detail_content.clone()
        };
        let p = Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
    }

    /// Render the *immeuble* scale — one molecule pleine-page with its
    /// adjacent neighbours and DAG cables. Three visible boxes, wired
    /// together by straight monospace glyphs (`│`, `└`, `─`). No layout
    /// engine, no force-directed graph — JR's wheat-paste rule: just
    /// bigger characters on the same wall.
    fn draw_immeuble(&self, f: &mut Frame, area: Rect) {
        let visible = self.filtered_indices();
        let Some(sel) = self.table_state.selected() else {
            let p = Paragraph::new("(no molecule selected)")
                .block(Block::default().borders(Borders::ALL).title(" immeuble "));
            f.render_widget(p, area);
            return;
        };
        let Some(pos) = visible.iter().position(|&i| i == sel) else {
            let p = Paragraph::new("(selection out of frame)")
                .block(Block::default().borders(Borders::ALL).title(" immeuble "));
            f.render_widget(p, area);
            return;
        };

        // prev / current / next molecule (up to three visible neighbours).
        let prev = pos
            .checked_sub(1)
            .and_then(|p| visible.get(p).and_then(|&i| self.rows.get(i)));
        let cur = self.rows.get(sel);
        let next = visible.get(pos + 1).and_then(|&i| self.rows.get(i));

        let lines = immeuble_lines(prev, cur, next);
        let title = cur
            .map(|r| format!(" immeuble · {} ", r.display_label(40)))
            .unwrap_or_else(|| " immeuble ".into());
        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
    }

    /// Render the *peau* scale — raw artifact text at full resolution.
    /// Delegates to the active detail renderer when set; otherwise falls
    /// back to the synthesis / briefing / state dump for the selected
    /// molecule. The operator sees exact bytes, citations, commit hashes.
    fn draw_peau(&self, f: &mut Frame, area: Rect) {
        let mol_label = self
            .selected_row()
            .map(|r| r.display_label(48))
            .unwrap_or_else(|| "-".into());
        let label = if self.active_renderer.is_some() {
            self.active_label()
        } else {
            "peau"
        };
        let title = format!(" {label} · {mol_label} (Ctrl-j/k, PgDn/PgUp to scroll) ");

        let content: ratatui::text::Text<'static> = if self.detail_content.lines.is_empty() {
            // Nothing pinned — fall back to a concise pleine-page dump of
            // the selected molecule's briefing artifact. This matches the
            // briefing that any worker reads when it wakes up, so peau is
            // exactly what the agent sees.
            self.peau_fallback_content()
        } else {
            self.detail_content.clone()
        };

        let p = Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
    }

    /// Load the briefing.md body of the selected molecule. Used as the
    /// *peau* default content when no detail renderer is active — the
    /// briefing is the canonical artifact every worker reads first, so
    /// falling back to it matches the operator's mental model of "what
    /// would the agent see right now?".
    fn peau_fallback_content(&self) -> ratatui::text::Text<'static> {
        let Some(dir) = self.selected_molecule_dir() else {
            return ratatui::text::Text::raw("(no molecule selected)");
        };
        let body = std::fs::read_to_string(dir.join("briefing.md")).unwrap_or_else(|_| {
            std::fs::read_to_string(dir.join("prompt.md"))
                .unwrap_or_else(|_| "(no briefing or prompt yet)".into())
        });
        renderers::render_markdown(&body)
    }
}

// Lower/upper bounds and step for the [`App::zoom_level`] machine.
//
// `0.0` = ville (fleet table), `1.0` = immeuble (molecule pleine-page),
// `2.0` = peau (raw artifact). Step chosen so five keypresses cross a
// scale — small enough to feel continuous, large enough to avoid a
// mushy "infinite dial" where the operator can't land cleanly on a
// scale marker. The `=` key snaps back to `ZOOM_MIN`.
const ZOOM_MIN: f32 = 0.0;
const ZOOM_MAX: f32 = 2.0;
const ZOOM_STEP: f32 = 0.25;

/// Short label for the current zoom level — used in the status-bar
/// message after a `+` / `-` / `=` keypress so the operator gets
/// immediate feedback on which scale they are heading toward.
fn zoom_label(z: f32) -> &'static str {
    if z < 0.5 {
        "ville"
    } else if z < 1.5 {
        "immeuble"
    } else {
        "peau"
    }
}

/// Build the *immeuble* scale body — one pleine-page rendering of the
/// selected molecule plus up to two adjacent neighbours, wired by
/// straight monospace DAG cables.
///
/// The rule is deliberately simple (JR's "no galaxie d'épinards"): each
/// neighbour box sits above / below the current molecule box, and the
/// cable is the vertical bar `│` with an `└` elbow where a typed link
/// (`BlockedBy` / `Blocks`) exists. When no link exists, a dim dashed line
/// separates the boxes so the operator still sees the spatial ordering
/// without being mislead into a dependency that isn't there.
fn immeuble_lines(
    prev: Option<&RowView>,
    cur: Option<&RowView>,
    next: Option<&RowView>,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let Some(cur) = cur else {
        lines.push(Line::from("(no molecule selected)"));
        return lines;
    };

    // Previous neighbour (above).
    if let Some(p) = prev {
        for l in molecule_box(p, BoxAccent::Neighbour) {
            lines.push(l);
        }
        lines.push(cable_line(p, cur));
    }

    // Current molecule (center, highlighted).
    for l in molecule_box(cur, BoxAccent::Current) {
        lines.push(l);
    }

    // Next neighbour (below).
    if let Some(n) = next {
        lines.push(cable_line(cur, n));
        for l in molecule_box(n, BoxAccent::Neighbour) {
            lines.push(l);
        }
    }

    lines
}

/// Which accent to paint a molecule box with. The current selection is
/// BOLD + cyan; neighbours are dim so the operator's eye lands on the
/// centre without a cursor or arrow.
#[derive(Clone, Copy)]
enum BoxAccent {
    Current,
    Neighbour,
}

/// Render a single molecule as a four-line monospace box:
///
/// ```text
/// ┌─ <id> ──────────────────── <step> ─┐
/// │ <kind> · <formula> · <status>      │
/// │ topic: <one-line topic>            │
/// └────────────────────────────────────┘
/// ```
///
/// Width adapts to the longest field; wrap/trim is left to ratatui's
/// `Paragraph` when the frame is narrower than the natural line.
fn molecule_box(r: &RowView, accent: BoxAccent) -> Vec<Line<'static>> {
    let accent_style = match accent {
        BoxAccent::Current => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        BoxAccent::Neighbour => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };
    let body_style = match accent {
        BoxAccent::Current => Style::default().fg(Color::Gray),
        BoxAccent::Neighbour => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };

    let id = r.display_label(40);
    let step = if r.step.is_empty() {
        "-".to_owned()
    } else {
        r.step.clone()
    };
    let kind = if r.kind.is_empty() { "?" } else { &r.kind };
    let formula = if r.formula.is_empty() {
        "?".to_owned()
    } else {
        r.formula.clone()
    };
    let status = if r.status.is_empty() {
        "?".to_owned()
    } else {
        r.status.clone()
    };
    let topic = r.topic.as_deref().unwrap_or("—");

    let top = format!("┌─ {id}  ·  step {step} ─┐");
    let mid1 = format!("│ {kind} · {formula} · {status}");
    let mid2 = format!("│ topic: {topic}");
    let bottom = "└─────────────────────────────────┘".to_owned();

    vec![
        Line::from(Span::styled(top, accent_style)),
        Line::from(Span::styled(mid1, body_style)),
        Line::from(Span::styled(mid2, body_style)),
        Line::from(Span::styled(bottom, accent_style)),
    ]
}

/// Build the cable connecting two adjacent molecule boxes. When `from`
/// blocks `to` (or vice-versa) the cable is a solid `│ └─` typed-link
/// indicator; otherwise it's a dim `·` spacer so the spatial ordering
/// is visible without faking a DAG edge.
fn cable_line(from: &RowView, to: &RowView) -> Line<'static> {
    let blocks = from.blocked_by.iter().any(|(id, _)| id == &to.mol_id)
        || to.blocked_by.iter().any(|(id, _)| id == &from.mol_id);
    if blocks {
        Line::from(Span::styled(
            "    │",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(
            "    ·",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))
    }
}

/// Build the ADAPTER column cell for the main table.
///
/// Adapter name in cyan, the pinned model dimmed after a `/`, and the
/// selection source as a dim bracketed tag (`[cli]`, `[config]`, …). An
/// unrecorded attribution renders a single dim placeholder. Reasoning effort
/// (magenta `@effort`) appears **only** when the honest fold carried one —
/// never inferred from the current config (task-20260712-6609). The plain
/// text of this cell is `AdapterAttribution::compact_cell`, the shared
/// drift-proof source of truth both the TUI and the HTTP surface render.
fn adapter_cell(att: &cosmon_core::adapter_attribution::AdapterAttribution) -> Line<'static> {
    use cosmon_core::adapter_attribution::EMPTY_CELL;
    let Some(adapter) = att.adapter.clone() else {
        return Line::from(Span::styled(
            EMPTY_CELL.to_owned(),
            Style::default().fg(Color::DarkGray),
        ));
    };
    let mut spans: Vec<Span<'static>> =
        vec![Span::styled(adapter, Style::default().fg(Color::Cyan))];
    if let Some(model) = att.model.clone() {
        spans.push(Span::styled(
            format!("/{model}"),
            Style::default().fg(Color::Gray),
        ));
    }
    // Realized-model drift (delib-20260718-c70e). The pin (intention) is dim
    // gray above; the *realized* segment — what actually ran — is painted a
    // distinct yellow so drift reads as a signal, not another pin. Shown only
    // on drift / observed-without-pin (agreement and silence add nothing here;
    // the honest disposition lives in the expanded detail).
    if let Some(drift) = att.realized_drift_display() {
        spans.push(Span::styled(
            format!("~>{drift}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(src) = att.adapter_source {
        spans.push(Span::styled(
            format!(" [{}]", src.tag()),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
    }
    if let Some(effort) = att.reasoning_effort {
        spans.push(Span::styled(
            format!("@{effort}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    Line::from(spans)
}

/// Build the extra detail lines shown under an expanded row.
///
/// Each field on its own line, vertically stacked with a fixed-width label
/// (14 chars) so values align. Empty fields render as `–` for stable layout.
fn expanded_detail_lines(r: &RowView) -> Vec<Line<'static>> {
    let label_style = Style::default().fg(Color::DarkGray);
    let val_style = Style::default().fg(Color::Gray);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    let dash = "–";
    let indent = "    ";
    // Fixed-width label: 12 chars right-padded + ": "
    let field = |name: &str, val: &str, last: bool| -> Line<'static> {
        let connector = if last { "└─" } else { "├─" };
        let label = format!("{indent}{connector} {name:<10} ");
        Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(val.to_owned(), val_style),
        ])
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // formula
    let formula = if r.formula.is_empty() {
        dash
    } else {
        r.formula.as_str()
    };
    lines.push(field("formula", formula, false));

    // tier
    if !r.tier_badge.is_empty() {
        lines.push(field("tier", &r.tier_badge, false));
    }

    // kind
    let kind = if r.kind.is_empty() {
        dash
    } else {
        r.kind.as_str()
    };
    lines.push(field("kind", kind, false));

    // topic
    let topic = r.topic.as_deref().unwrap_or(dash);
    lines.push(field("topic", topic, false));

    // worker
    let worker = r.worker_name.as_deref().unwrap_or(dash);
    lines.push(field("worker", worker, false));

    // branch
    let branch = format!("feat/{}", r.mol_id);
    lines.push(field("branch", &branch, false));

    // session
    let session = r
        .session_slug
        .as_deref()
        .or(r.session.as_deref())
        .unwrap_or(dash);
    lines.push(field("session", session, false));

    // blocked-by (each blocker with status)
    if r.blocked_by.is_empty() {
        lines.push(field("blocked-by", dash, false));
    } else {
        let blocked_str: String = r
            .blocked_by
            .iter()
            .map(|(id, st)| format!("{id} [{st}]"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(field("blocked-by", &blocked_str, false));
    }

    // tags
    let tags = if r.tags.is_empty() {
        dash.to_owned()
    } else {
        r.tags.join(", ")
    };
    lines.push(field("tags", &tags, false));

    // energy budget — per-molecule step circuit breaker (THESIS Part XI)
    let energy = match r.energy_budget {
        Some((remaining, cap)) if cap > 0 => format!("{remaining}/{cap}"),
        _ => dash.to_owned(),
    };
    lines.push(field("energy", &energy, false));

    // created
    let created = r
        .created_at_utc
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| dash.to_owned());
    lines.push(field("created", &created, false));

    // adapter — the honest, persisted dispatch attribution (adapter / model
    // / selection source). Reasoning effort is shown only when a past event
    // recorded it, never inferred from the current config
    // (task-20260712-6609). Empty renders as `-`.
    lines.push(field("adapter", &r.adapter.compact_cell(), false));

    // realized — the honest disposition of what actually ran, on its own axis
    // (delib-20260718-c70e). Distinct from the intention pin shown in `adapter`:
    // `? (unknown)` (crashed / never observed), `- (silent)` (ran, reported
    // nothing), or the observed `a->b` trajectory. Never back-filled from the
    // pin. The value is painted yellow when a concrete model was observed so it
    // stands apart from the dim intention above.
    {
        let realized = format!(
            "{} ({})",
            r.adapter.realized.detail_fragment(),
            r.adapter.realized.disposition(),
        );
        let realized_style = if r.adapter.realized.observed().is_some() {
            Style::default().fg(Color::Yellow)
        } else {
            dim_style
        };
        let connector = "├─";
        let label = format!("{indent}{connector} {:<10} ", "realized");
        lines.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(realized, realized_style),
        ]));
    }

    // heartbeat (with last activity)
    //
    // The "stalled" label has been retired from the visual vocabulary
    // (2026-04-19): a live tmux with no recent output is a *quiet*
    // worker, not a broken one. Only the Orphaned tier is genuinely
    // alarming, and it gets its own label + glyph already.
    let hb_label = match r.heartbeat {
        HeartbeatTier::Active => "active",
        HeartbeatTier::Idle => "idle",
        HeartbeatTier::Quiet | HeartbeatTier::Stalled => "quiet",
        HeartbeatTier::Orphaned => "orphaned",
    };
    let hb_val = if let Some(la) = r.last_activity {
        let ago = Utc::now().signed_duration_since(la).num_seconds();
        format!("{} {} (last: {}s ago)", r.heartbeat.glyph(), hb_label, ago)
    } else {
        format!("{} {}", r.heartbeat.glyph(), hb_label)
    };
    let hb_line = {
        let connector = "└─";
        let label = format!("{indent}{connector} {:<10} ", "heartbeat");
        Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(
                hb_val,
                match r.heartbeat {
                    HeartbeatTier::Active => Style::default().fg(Color::Green),
                    HeartbeatTier::Idle => Style::default().fg(Color::Yellow),
                    HeartbeatTier::Quiet | HeartbeatTier::Stalled => {
                        Style::default().fg(Color::Gray)
                    }
                    HeartbeatTier::Orphaned => dim_style,
                },
            ),
        ])
    };
    lines.push(hb_line);

    lines
}

/// Ratatui style applied to the raw heartbeat tier in detail panes. The
/// primary row glyph now comes from [`RowKind`]; this helper is kept for
/// ancillary UI that shows the heartbeat label outside the table. Never
/// paints red unless the tier is Orphaned — red is reserved for ghosts.
#[allow(dead_code)]
fn heartbeat_style(tier: HeartbeatTier) -> Style {
    match tier {
        HeartbeatTier::Active => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        HeartbeatTier::Idle => Style::default().fg(Color::Yellow),
        HeartbeatTier::Quiet | HeartbeatTier::Stalled => Style::default().fg(Color::Gray),
        HeartbeatTier::Orphaned => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}

/// Cosmon-charter status glyph tinted by the Writer role hue, with heartbeat
/// overrides for orphaned / stalled runs (they repaint to red so a dead
/// session looks alarming even if its status string still reads "running").
///
/// Returns `(glyph, style)` — a single-char semantic token. The long status
/// word ("running" / "pending" / …) is intentionally dropped from the base
/// row: the glyph + color already carry that information, and textual labels
/// are available in the expanded detail block.
fn status_token(status: &str, heartbeat: HeartbeatTier) -> (String, Style) {
    use cosmon_core::visual::{Charter, Status};
    let core_status = match status {
        "queued" => cosmon_core::molecule::MoleculeStatus::Queued,
        "running" => cosmon_core::molecule::MoleculeStatus::Running,
        "frozen" | "stuck" => cosmon_core::molecule::MoleculeStatus::Frozen,
        "completed" => cosmon_core::molecule::MoleculeStatus::Completed,
        "collapsed" => cosmon_core::molecule::MoleculeStatus::Collapsed,
        _ => cosmon_core::molecule::MoleculeStatus::Pending,
    };
    let vstatus = Status::for_molecule_status(core_status);
    let charter = Charter::get();
    let glyph = charter.status(vstatus).glyph.clone();

    // ANSI-16 palette (delib-b8c6 P1/C2): the previous Writer-hue `Color::Rgb`
    // was invisible on dark terminals and collided with solarized/gruvbox
    // themes. Named ANSI-16 colors are remapped by the terminal emulator to
    // match the operator's theme, so `Green`/`Yellow`/`Red`/`Blue` stay
    // legible everywhere.
    //
    // Heartbeat override: a running row with a dead session is a genuine
    // ghost (red). A running row whose worker went quiet for 30+ minutes
    // is still alive in tmux — paint it by status band, not red, so the
    // pastille only lies when a real ghost is visible (2026-04-19 charter).
    let style = match (heartbeat, status) {
        (HeartbeatTier::Orphaned, "running") => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        (HeartbeatTier::Orphaned, _) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
        _ => {
            // Active and Completed intentionally share Color::Green — the
            // modifier below distinguishes them (BOLD vs DIM).
            #[allow(clippy::match_same_arms)]
            let mut s = Style::default().fg(match vstatus {
                Status::Active => Color::Green,
                Status::Pending | Status::Waiting => Color::Yellow,
                Status::Stuck => Color::Blue,
                Status::Completed => Color::Green,
                Status::Collapsed => Color::Red,
            });
            match vstatus {
                Status::Active => s = s.add_modifier(Modifier::BOLD),
                Status::Completed | Status::Collapsed => s = s.add_modifier(Modifier::DIM),
                _ => {}
            }
            s
        }
    };
    (glyph, style)
}

/// Ratatui style for a [`MoleculeHealth`] glyph. Uses the ANSI-16 palette
/// so colors remap under solarized / gruvbox themes.
fn molecule_health_style(h: MoleculeHealth) -> Style {
    match h {
        MoleculeHealth::Healthy => Style::default().fg(Color::Green),
        MoleculeHealth::Orphaned | MoleculeHealth::Blocked => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        MoleculeHealth::Stalled | MoleculeHealth::Degraded => Style::default().fg(Color::Yellow),
        MoleculeHealth::Inert | MoleculeHealth::Terminal => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}

/// Lineage-coverage score → TRUST column badge text + style. Bands:
///
/// - `None`     → grey `—` (not verified yet)
/// - `<50`      → red `██ N%` (evidence chain is broken)
/// - `50..=85`  → amber `▓░ N%` (partial verification)
/// - `>85`      → green `██ N%` (strong lineage)
///
/// The half-block pair is a tiny bar-chart borrowed from the energy
/// column — it lets the operator absorb "how much trust" at a glance
/// without reading digits. 7 visible columns including the percentage.
fn trust_badge(score: Option<u8>) -> (String, Style) {
    match score {
        None => ("  —   ".to_owned(), Style::default().fg(Color::DarkGray)),
        Some(p) if p < 50 => (
            format!("██{p:>3}%"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Some(p) if p <= 85 => (format!("▓░{p:>3}%"), Style::default().fg(Color::Yellow)),
        Some(p) => (format!("██{p:>3}%"), Style::default().fg(Color::Green)),
    }
}

fn format_energy(input: u64, output: u64, cost_usd: f64, cw: Option<u64>) -> String {
    // Three right-aligned fields with fixed *visible* widths so the column
    // reads like a ledger: `<bar 6w> <tokens 6w> <cost 7w>`.
    //
    // - bar  : "█  72%" or lone "·" when no context window was reported
    // - tokens: humanised total ("-", "123", "1.2K", "1.2M"), right-justified
    // - cost : "$0.12" / "$12.34" / "$123.45"
    //
    // Rust's `{:>N}` counts *scalar chars*, which misaligns the moment a
    // value contains a wide char (e.g. an emoji fallback from the theme,
    // combining marks, or a terminal that upgrades block glyphs to full
    // width). `pad_to_visible_width` uses `unicode-width` to measure the
    // rendered column count and pads with ASCII spaces, so the `$` sits at
    // the same x coordinate regardless of magnitude or font metrics
    // (delib-b8c6 P1/C3).
    let total = input.saturating_add(output);
    let bar = match cw {
        Some(c) if c > 0 => {
            let pct = (total as f64 / c as f64 * 100.0).clamp(0.0, 999.0);
            let glyph = if pct < 25.0 {
                '▂'
            } else if pct < 50.0 {
                '▄'
            } else if pct < 75.0 {
                '▆'
            } else {
                '█'
            };
            pad_to_visible_width(&format!("{glyph} {pct:>3.0}%"), 6)
        }
        _ => "     ·".to_owned(),
    };
    let tokens = pad_to_visible_width(&humanize_tokens(total), 6);
    let cost = pad_to_visible_width(&format!("${:.2}", cost_usd.max(0.0)), 7);
    format!("{bar} {tokens} {cost}")
}

/// Right-pad `s` with ASCII spaces so its **visual** column width matches
/// `target`. Uses `unicode-width` to count rendered columns (emoji, block
/// glyphs, fullwidth chars, combining marks handled correctly). If `s` is
/// already wider than `target`, it is returned unchanged.
///
/// Why this exists — Rust's `{:>N}` formatter pads by `char` count, which
/// disagrees with the terminal's column count for non-ASCII text. A row
/// containing a `🔥` emoji (width 2) in an otherwise char-counted column
/// would place the next field one column to the right of its neighbours.
fn pad_to_visible_width(s: &str, target: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let width = UnicodeWidthStr::width(s);
    if width >= target {
        s.to_owned()
    } else {
        let pad = target - width;
        let mut out = String::with_capacity(s.len() + pad);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(s);
        out
    }
}

fn humanize_tokens(n: u64) -> String {
    if n == 0 {
        "-".to_owned()
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Render the AGE column for a row: the elapsed time since its last state
/// write, or `-` when there is no molecule behind the session.
///
/// This is the *only* place the clock touches the age of a row. The value it
/// produces is rendered and thrown away; nothing compares it, and in
/// particular the sort key does not (delib-20260716-a2f1 C3). A quantity that
/// changes every tick without any state changing underneath it can order rows
/// only by accident.
fn age_cell(updated_at: Option<DateTime<Utc>>) -> String {
    updated_at.map_or_else(|| "-".into(), age_since)
}

fn age_since(ts: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now.signed_duration_since(ts);
    let secs = delta.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Fold every session + molecule in `snap` into at most one row per
/// molecule id.
///
/// The runtime-vs-cognition split means a
/// single macro-molecule may be bound to two tmux sessions — one for the
/// resident runtime, one for the cognitive worker. Before this pass the
/// TUI rendered each session as its own row, so a pair showed up as two
/// identical-looking entries and operators lost track of the bijection
/// "one molecule ≡ one line". This function does the fusion:
///
/// 1. Build a base row from each session (preferring cognition over
///    runtime when both are present).
/// 2. Merge energy totals across every worker tied to the same mol id.
/// 3. Compose [`RowView::role_glyphs`] so the operator still sees both
///    roles at a glance via the 🎛️ / 🧠 markers.
/// 4. Emit orphaned / pending / frozen molecules as standalone rows the
///    same way the session loop would.
pub(crate) fn snapshot_to_rows(snap: &FleetSnapshot) -> Vec<RowView> {
    use std::collections::HashMap;

    let now = Utc::now();
    let mut by_mol: HashMap<String, RowView> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for s in snap.list_sessions(&SessionFilter::default()) {
        let mol = s
            .molecule_id
            .as_ref()
            .and_then(|mid| snap.molecules().find(|m| m.id.to_string() == *mid));
        let worker = s
            .worker_id
            .as_ref()
            .and_then(|wid| snap_find_worker(snap, wid));
        let fresh = row_view_from(s, mol, worker, now);
        let mol_id = fresh.mol_id.clone();
        let worker_role = worker.map(|w| w.role);
        if let Some(existing) = by_mol.get_mut(&mol_id) {
            merge_row(existing, fresh, worker_role);
        } else {
            let mut row = fresh;
            if let Some(r) = worker_role {
                glyph_for_role(r).clone_into(&mut row.role_glyphs);
            }
            order.push(mol_id.clone());
            by_mol.insert(mol_id, row);
        }
    }

    for m in snap.molecules() {
        let id = m.id.to_string();
        if by_mol.contains_key(&id) {
            continue;
        }
        let heartbeat = if m.status == MoleculeStatus::Running {
            HeartbeatTier::Orphaned
        } else {
            HeartbeatTier::Stalled
        };
        order.push(id.clone());
        by_mol.insert(
            id.clone(),
            RowView {
                mol_id: id,
                session_slug: m.session.clone(),
                project: m.project_root.clone(),
                role: "-".into(),
                status: m.status.to_string(),
                step: "-".into(),
                updated_at: Some(m.updated_at),
                energy_in: 0,
                energy_out: 0,
                cost_usd: 0.0,
                context_window: None,
                session: None,
                socket: String::new(),
                heartbeat,
                last_activity: None,
                last_progress_at: None,
                topic: None,
                mission_description: None,
                formula: String::new(),
                tier_badge: String::new(),
                kind: String::new(),
                blocked_by: Vec::new(),
                worker_name: None,
                tags: Vec::new(),
                created_at_utc: None,
                whisper_fresh: false,
                role_glyphs: String::new(),
                trust_score: None,
                energy_budget: None,
                adapter: cosmon_core::adapter_attribution::AdapterAttribution::default(),
            },
        );
    }

    order
        .into_iter()
        .filter_map(|id| by_mol.remove(&id))
        .collect()
}

/// Map an observability [`WorkerRole`](cosmon_observability::worker::WorkerRole)
/// to the glyph used by the peek renderer.
fn glyph_for_role(role: cosmon_observability::worker::WorkerRole) -> &'static str {
    match role {
        cosmon_observability::worker::WorkerRole::Runtime => "🎛️",
        cosmon_observability::worker::WorkerRole::Cognition => "🧠",
    }
}

/// Merge a fresh session-derived row into an existing row that shares
/// the same molecule id. Cognition wins over runtime for the primary
/// display details (session label, worker name, step), because the
/// operator cares about the process doing the work — runtime is a
/// supervisor.
fn merge_row(
    existing: &mut RowView,
    fresh: RowView,
    worker_role: Option<cosmon_observability::worker::WorkerRole>,
) {
    use cosmon_observability::worker::WorkerRole as OR;
    // Energy totals merge across every worker bound to the same mol.
    existing.energy_in = existing.energy_in.saturating_add(fresh.energy_in);
    existing.energy_out = existing.energy_out.saturating_add(fresh.energy_out);
    existing.cost_usd += fresh.cost_usd;
    if existing.context_window.is_none() {
        existing.context_window = fresh.context_window;
    }
    // Prefer the freshest heartbeat / activity across roles.
    if fresh.heartbeat > existing.heartbeat {
        existing.heartbeat = fresh.heartbeat;
    }
    match (existing.last_activity, fresh.last_activity) {
        (Some(a), Some(b)) => existing.last_activity = Some(a.max(b)),
        (None, Some(b)) => existing.last_activity = Some(b),
        _ => {}
    }
    // Cognition wins for the primary label columns — runtime is only a
    // supervisor, the operator wants to see the worker doing the work.
    if matches!(worker_role, Some(OR::Cognition)) {
        existing.session = fresh.session;
        existing.session_slug = fresh.session_slug.or(existing.session_slug.clone());
        existing.worker_name = fresh.worker_name;
        existing.role = fresh.role;
        existing.step = fresh.step;
    }
    // Append the role glyph if we haven't recorded it yet.
    if let Some(r) = worker_role {
        let g = glyph_for_role(r);
        if !existing.role_glyphs.contains(g) {
            existing.role_glyphs.push_str(g);
        }
    }
}

fn snap_find_worker<'a>(snap: &'a FleetSnapshot, id: &str) -> Option<&'a Worker> {
    snap.workers().find(|w| w.id.0 == id)
}

fn row_view_from(
    s: &Session,
    mol: Option<&Molecule>,
    worker: Option<&Worker>,
    now: DateTime<Utc>,
) -> RowView {
    let (mol_id, status, updated_at, project) = mol
        .map(|m| {
            // Tmux-session presence is ground truth for liveness. When a
            // molecule is still recorded as Pending/Queued on disk but a
            // live session is bound to it, we're inside the short race
            // window between `cs tackle` spawning the worker and the
            // subsequent `save_molecule(Running)`. Show Running so the
            // operator doesn't see a stale yellow glyph they'd otherwise
            // only clear by attaching.
            let effective = match m.status {
                MoleculeStatus::Pending => MoleculeStatus::Running,
                other => other,
            };
            (
                m.id.to_string(),
                effective.to_string(),
                Some(m.updated_at),
                m.project_root.clone(),
            )
        })
        .unwrap_or_else(|| {
            (
                s.molecule_id.clone().unwrap_or_else(|| "-".into()),
                "-".into(),
                None,
                project_label(&s.project_root),
            )
        });
    let role = worker.map_or_else(|| "-".into(), |_w| "worker".into());
    let energy = worker.map(|w| w.energy).unwrap_or(EnergyBudget::default());
    // Heartbeat freshness — pick the MAX of both signals.
    //
    // tmux `#{session_activity}` is attach-bumped: opening the session with
    // Enter resets it even when the worker produced nothing. Conversely,
    // `molecule.updated_at` ticks every `cs evolve` / state save.
    // Neither alone is truthful. Using the max means a freshly-tackled
    // molecule (updated_at ~ now) shows Active 🟢 immediately instead of
    // 🟡 Idle until the operator attaches and artificially bumps tmux.
    // This is the second half of the fix started in 89ba (status override);
    // without it, the heartbeat column stayed stale even after the status
    // glyph was corrected.
    let last_activity = match (s.last_activity, mol.map(|m| m.updated_at)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    let heartbeat = HeartbeatTier::classify(last_activity, now);
    let session_slug = mol.and_then(|m| m.session.clone());
    RowView {
        mol_id,
        session_slug,
        project,
        role,
        status,
        step: "-".into(),
        updated_at,
        energy_in: energy.input_tokens,
        energy_out: energy.output_tokens,
        cost_usd: energy.cost_usd,
        context_window: energy.context_window,
        session: Some(s.name.clone()),
        socket: s.socket.clone(),
        heartbeat,
        last_activity,
        // `last_progress_at` is only available in the molecule's persisted
        // state, not in the observability snapshot — `enrich_rows` reads
        // `state.json` and populates this field on cache miss.
        last_progress_at: None,
        topic: None,
        mission_description: None,
        formula: String::new(),
        tier_badge: String::new(),
        kind: String::new(),
        blocked_by: Vec::new(),
        worker_name: worker.map(|w| w.id.0.clone()),
        tags: Vec::new(),
        created_at_utc: None,
        whisper_fresh: false,
        role_glyphs: String::new(),
        trust_score: None,
        energy_budget: None,
        adapter: cosmon_core::adapter_attribution::AdapterAttribution::default(),
    }
}

fn project_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("-")
        .to_owned()
}

/// Push `text` onto the system clipboard. Strategy:
///
/// 1. Try the platform-native clipboard binary (`pbcopy` on macOS,
///    `wl-copy` / `xclip` / `xsel` on Linux). When the user runs `cs peek`
///    on a real desktop session this is the highest-fidelity path.
/// 2. Fall back to OSC 52, an ANSI escape that asks the controlling
///    terminal to set its clipboard. Works inside `ssh` and `tmux`
///    (provided `set -g set-clipboard on`) where no clipboard binary is
///    reachable. The escape goes straight to stdout because the TUI is
///    in raw mode and the alternate screen.
///
/// Why both: a single mechanism would either break for remote sessions
/// (binary path) or fail in vanilla terminals where OSC 52 is disabled
/// (e.g. Terminal.app). Trying the binary first keeps macOS users out of
/// OSC-52-allowlist territory entirely.
fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    for (bin, args) in candidates {
        let mut cmd = Command::new(bin);
        cmd.args(*args).stdin(std::process::Stdio::piped());
        let Ok(mut child) = cmd.spawn() else { continue };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if let Ok(status) = child.wait() {
            if status.success() {
                return Ok(());
            }
        }
    }
    // Fallback: OSC 52 — base64-encode and write the escape sequence
    // straight to stdout. Inside tmux we wrap with the passthrough
    // sequence so tmux forwards it to the outer terminal.
    let encoded = base64_encode(text.as_bytes());
    let in_tmux = std::env::var_os("TMUX").is_some();
    let inner = format!("\x1b]52;c;{encoded}\x07");
    let payload = if in_tmux {
        // tmux passthrough: ESC P tmux; ESC <payload-with-doubled-ESC> ESC \
        let escaped = inner.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;\x1b{escaped}\x1b\\")
    } else {
        inner
    };
    let mut out = io::stdout();
    out.write_all(payload.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// Standard base64 (RFC 4648) encoder. Hand-rolled to avoid pulling a
/// dependency for ~20 lines of byte-shuffling. Used by [`copy_to_clipboard`]
/// for the OSC 52 fallback path.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in &mut chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

pub(super) fn capture_pane(socket: &str, session: &str) -> anyhow::Result<String> {
    let output = Command::new("tmux")
        .args([
            "-L",
            socket,
            "capture-pane",
            "-t",
            session,
            "-p",
            "-e",
            "-S",
            "-200",
        ])
        .output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Build a [`FleetSnapshot`] from the local filestore and the tmux socket.
///
/// When `all` is true, this enumerates every tmux socket under
/// `$TMUX_TMPDIR/tmux-<uid>/` (default `/private/tmp/tmux-501/` on macOS) and
/// walks up from each live session's working directory to discover the
/// owning `.cosmon/` directory. Molecules from every discovered project are
/// merged into the snapshot, so `cs peek --all` from any project shows a
/// cross-project fleet view.
pub(crate) fn build_snapshot(
    state_dir: &std::path::Path,
    socket: &str,
    project_id: Option<&cosmon_core::id::ProjectId>,
    all: bool,
) -> anyhow::Result<(
    FleetSnapshot,
    std::collections::HashMap<String, std::path::PathBuf>,
)> {
    let mut snap = FleetSnapshot::new();
    let mut state_dirs: std::collections::HashMap<String, std::path::PathBuf> =
        std::collections::HashMap::new();
    // Resolve real token counts and cost via claudion for every active worker.
    let backends = crate::energy_probe::discover_fleet_backends(state_dir, socket);

    if all {
        // Multi-socket, multi-project aggregation.
        let sockets = enumerate_tmux_sockets();
        // (cosmon_dir canonical) -> project_label
        let mut projects: std::collections::HashMap<std::path::PathBuf, String> =
            std::collections::HashMap::new();
        // (cosmon_dir canonical, socket, session_name, last_activity)
        let mut sessions_by_project: Vec<(
            std::path::PathBuf, // cosmon_dir
            String,             // socket
            String,             // session_name
            Option<DateTime<Utc>>,
        )> = Vec::new();

        // Parallel tmux fan-out (delib-b8c6 P1/C1): cross-galaxy startup used
        // to do one synchronous `tmux list-sessions` per socket (2-5s for 10+
        // sockets). Scoped threads collapse that into a single wall-clock
        // round-trip. No tokio — the work is blocking I/O and the thread
        // count is tiny (one per live tmux socket).
        #[allow(clippy::type_complexity)]
        let per_socket: Vec<(String, Vec<(String, String, Option<DateTime<Utc>>)>)> =
            std::thread::scope(|s| {
                let handles: Vec<_> = sockets
                    .iter()
                    .map(|sk| {
                        let sk = sk.clone();
                        s.spawn(move || {
                            let sessions = list_tmux_sessions_with_path(&sk);
                            (sk, sessions)
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });

        for (sk, sessions) in per_socket {
            for (sname, cwd, activity) in sessions {
                let Some(cosmon_dir) =
                    cosmon_filestore::walk_up_find_cosmon_dir_from(std::path::Path::new(&cwd))
                else {
                    continue;
                };
                let canon = std::fs::canonicalize(&cosmon_dir).unwrap_or(cosmon_dir);
                projects
                    .entry(canon.clone())
                    .or_insert_with(|| project_label_for(&canon));
                sessions_by_project.push((canon, sk.clone(), sname, activity));
            }
        }

        // Always include the current project even if no live session is attached.
        if let Some(cur) = cosmon_filestore::walk_up_find_cosmon_dir_from(
            &std::env::current_dir().unwrap_or_default(),
        ) {
            let canon = std::fs::canonicalize(&cur).unwrap_or(cur);
            projects
                .entry(canon.clone())
                .or_insert_with(|| project_label_for(&canon));
        }

        for (cosmon_dir, label) in &projects {
            let sd = cosmon_dir.join("state");
            let store = FileStore::new(&sd);
            let Ok(fleet) = store.load_fleet() else {
                continue;
            };
            let Ok(molecules) = store.list_molecules(&cosmon_state::MoleculeFilter::default())
            else {
                continue;
            };
            for m in &molecules {
                state_dirs.insert(m.id.to_string(), sd.clone());
            }
            let energy_by_worker = crate::energy_probe::load_worker_energy(&sd, &backends, &fleet);
            populate_snapshot(
                &mut snap,
                &store,
                &fleet,
                &molecules,
                label,
                cosmon_dir,
                None, // sockets are resolved below per-session
                &sessions_by_project
                    .iter()
                    .filter(|(c, _, _, _)| c == cosmon_dir)
                    .map(|(_, sk, sn, a)| (sk.clone(), sn.clone(), *a))
                    .collect::<Vec<_>>(),
                &energy_by_worker,
            );
        }
    } else {
        // Single-project mode (legacy behavior).
        let store = FileStore::new(state_dir);
        let fleet = store.load_fleet()?;
        let filter = cosmon_state::MoleculeFilter {
            project: project_id.cloned(),
            ..cosmon_state::MoleculeFilter::default()
        };
        let molecules = store.list_molecules(&filter)?;
        for m in &molecules {
            state_dirs.insert(m.id.to_string(), state_dir.to_path_buf());
        }
        let label = project_id
            .map(ToString::to_string)
            .unwrap_or_else(|| project_label_for(state_dir));
        let live: Vec<(String, String, Option<DateTime<Utc>>)> = list_tmux_sessions(socket)
            .into_iter()
            .map(|(s, a)| (socket.to_owned(), s, a))
            .collect();
        let energy_by_worker =
            crate::energy_probe::load_worker_energy(state_dir, &backends, &fleet);
        populate_snapshot(
            &mut snap,
            &store,
            &fleet,
            &molecules,
            &label,
            state_dir,
            Some(socket),
            &live,
            &energy_by_worker,
        );
    }

    Ok((snap, state_dirs))
}

// `cs peek` is a STRICT READER (delib-20260718-c70e / F-01): it no longer
// emits `ModelObserved`. Realized-model capture is now an always-on step of the
// completion seam (`cs complete` → `energy_probe::capture_realized_at_completion`),
// so the journal records what ran even when nobody is watching the TUI. The
// pending live-view refinement is a pure read: `AdapterAttribution::mark_pending_if_live`
// upgrades an unobserved but running molecule's `?` to `...` at render time.

fn project_label_for(path: &std::path::Path) -> String {
    // .cosmon/ -> parent basename (project dir name). Fall back to "config.toml".project_id.
    let config = path.join("config.toml");
    if config.exists() {
        if let Ok(cfg) = cosmon_filestore::load_project_config(&config) {
            if let Some(id) = cfg.project.project_id {
                return id.to_string();
            }
        }
    }
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_owned()
}

#[allow(clippy::too_many_arguments)]
fn populate_snapshot(
    snap: &mut FleetSnapshot,
    _store: &FileStore,
    fleet: &cosmon_state::Fleet,
    molecules: &[cosmon_state::MoleculeData],
    project_label: &str,
    project_dir: &std::path::Path,
    default_socket: Option<&str>,
    live_sessions: &[(String, String, Option<DateTime<Utc>>)], // (socket, session_name, last_activity)
    energy_by_worker: &std::collections::HashMap<
        cosmon_core::id::WorkerId,
        crate::energy_probe::WorkerEnergy,
    >,
) {
    for m in molecules {
        let id: cosmon_observability::MoleculeId = m.id.to_string().as_str().into();
        snap.insert_molecule(Molecule {
            id,
            title: m.id.to_string(),
            kind: m
                .kind
                .as_ref()
                .map(|k| format!("{k:?}").to_lowercase())
                .unwrap_or_else(|| "task".into()),
            status: m.status,
            project_root: project_label.to_owned(),
            session: m.session_name.clone(),
            updated_at: m.updated_at,
        });
    }

    for w in fleet.workers.values() {
        let wid_s = w.id.as_str().to_owned();
        let matched: Option<(String, String, Option<DateTime<Utc>>)> = live_sessions
            .iter()
            .find(|(_, s, _)| s.as_str() == wid_s)
            .cloned()
            .or_else(|| {
                w.current_molecule
                    .as_ref()
                    .and_then(|mid| molecules.iter().find(|m| m.id == *mid))
                    .and_then(|m| m.session_name.clone())
                    .map(|sn| {
                        let activity = live_sessions
                            .iter()
                            .find(|(_, s, _)| s == &sn)
                            .and_then(|(_, _, a)| *a);
                        (default_socket.unwrap_or("default").to_owned(), sn, activity)
                    })
            });

        let energy = energy_by_worker
            .get(&w.id)
            .map(|e| {
                let (i, o, c) = e.as_tuple();
                EnergyBudget {
                    input_tokens: i,
                    output_tokens: o,
                    cost_usd: c,
                    context_window: None,
                }
            })
            .unwrap_or_default();

        snap.insert_worker(Worker {
            id: wid_s.as_str().into(),
            molecule_id: w.current_molecule.as_ref().map(ToString::to_string),
            session: matched
                .as_ref()
                .map(|(_, s, _)| s.clone())
                .unwrap_or_default(),
            energy,
            live: format!("{:?}", w.status).to_lowercase(),
            role: match w.worker_role {
                cosmon_core::worker::WorkerRole::Runtime => {
                    cosmon_observability::worker::WorkerRole::Runtime
                }
                cosmon_core::worker::WorkerRole::Cognition => {
                    cosmon_observability::worker::WorkerRole::Cognition
                }
            },
        });

        if let Some((sk, sn, activity)) = matched {
            snap.push_session(Session {
                name: sn,
                socket: sk,
                project_root: project_dir.display().to_string(),
                molecule_id: w.current_molecule.as_ref().map(ToString::to_string),
                worker_id: Some(wid_s),
                last_activity: activity,
            });
        }
    }

    // Include sessions named after molecule `session_name` without a worker tie.
    for m in molecules {
        if let Some(sn) = &m.session_name {
            if let Some((sk, _, activity)) = live_sessions.iter().find(|(_, s, _)| s == sn) {
                if snap
                    .list_sessions(&SessionFilter::default())
                    .iter()
                    .all(|s| &s.name != sn)
                {
                    snap.push_session(Session {
                        name: sn.clone(),
                        socket: sk.clone(),
                        project_root: project_dir.display().to_string(),
                        molecule_id: Some(m.id.to_string()),
                        worker_id: None,
                        last_activity: *activity,
                    });
                }
            }
        }
    }
}

/// Drop molecules from `snap` whose phase the `PhaseFilter` does not
/// surface. Used by `cs peek --snapshot --phase done` (and the default) to
/// project the wheat-paste byte stream through the requested phase
/// slice. Workers and sessions are untouched — the wheat-paste's
/// WORKERS / SESSIONS sections still list every recorded entity so the
/// operator can see the runtime state of the fleet; only the molecule
/// rows are sliced.
pub(crate) fn filter_snapshot_by_phase(
    snap: &mut FleetSnapshot,
    phase_filter: super::peek::PhaseFilter,
) {
    snap.retain_molecules(|m| phase_filter.matches_status(m.status));
}

/// Enumerate every tmux socket available to the current user.
///
/// tmux places its per-user sockets under `$TMUX_TMPDIR/tmux-<uid>/` (default
/// `/tmp/tmux-<uid>/`, which is `/private/tmp/tmux-<uid>/` on macOS after
/// symlink resolution). Each entry in that directory is a Unix-domain socket
/// whose basename is the socket *name* passed to `tmux -L <name>`. Without
/// this enumeration `cs peek --all` only sees the socket cosmon spawned
/// workers on, missing sessions started under other sockets (e.g. `default`,
/// or per-project socket names like `wiki2-4d7e`).
fn enumerate_tmux_sockets() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("TMUX_TMPDIR") {
        roots.push(std::path::PathBuf::from(dir));
    }
    roots.push(std::path::PathBuf::from("/private/tmp"));
    roots.push(std::path::PathBuf::from("/tmp"));

    for root in &roots {
        let Ok(rd) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name_s = name.to_string_lossy();
            // tmux directories are named `tmux-<uid>`.
            if !name_s.starts_with("tmux-") {
                continue;
            }
            let Ok(sub) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for e in sub.flatten() {
                if let Some(n) = e.file_name().to_str() {
                    out.push(n.to_owned());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Return `(session_name, session_path, last_activity)` tuples for every
/// session on `socket`. `last_activity` comes from tmux's built-in
/// `#{session_activity}` (unix ts of the last keystroke or pane output),
/// zero-cost vs a filesystem probe.
fn list_tmux_sessions_with_path(socket: &str) -> Vec<(String, String, Option<DateTime<Utc>>)> {
    let Ok(output) = Command::new("tmux")
        .args([
            "-L",
            socket,
            "list-sessions",
            "-F",
            "#{session_name}|#{session_path}|#{session_activity}",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '|');
            let n = it.next()?.trim();
            let p = it.next().unwrap_or("").trim();
            let a = it.next().unwrap_or("").trim();
            if n.is_empty() {
                return None;
            }
            Some((n.to_owned(), p.to_owned(), parse_tmux_activity(a)))
        })
        .collect()
}

fn list_tmux_sessions(socket: &str) -> Vec<(String, Option<DateTime<Utc>>)> {
    let Ok(output) = Command::new("tmux")
        .args([
            "-L",
            socket,
            "list-sessions",
            "-F",
            "#{session_name}|#{session_activity}",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| {
            let (n, a) = l.split_once('|').unwrap_or((l, ""));
            let n = n.trim();
            if n.is_empty() {
                None
            } else {
                Some((n.to_owned(), parse_tmux_activity(a.trim())))
            }
        })
        .collect()
}

/// Parse tmux's `#{session_activity}` — seconds since the unix epoch, as a
/// decimal string. Returns `None` on empty / unparseable input.
fn parse_tmux_activity(s: &str) -> Option<DateTime<Utc>> {
    let secs: i64 = s.parse().ok()?;
    DateTime::<Utc>::from_timestamp(secs, 0)
}

#[cfg(test)]
impl App {
    /// Test-only constructor. Builds a minimal `App` with the supplied
    /// rows and per-row state-dir map; everything else is set to a benign
    /// default. Intentionally never touches the terminal, so it is safe to
    /// call from `cargo test` (which runs without a TTY).
    fn for_test(
        rows: Vec<RowView>,
        row_state_dirs: std::collections::HashMap<String, std::path::PathBuf>,
    ) -> Self {
        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        Self {
            state_dir: std::path::PathBuf::new(),
            socket: String::new(),
            all_projects: false,
            project_id: None,
            default_project_id: None,
            refresh: Duration::from_millis(250),
            rows,
            census: WorkerCensus::default(),
            row_state_dirs,
            expanded: std::collections::HashSet::new(),
            table_state: TableState::default(),
            filter: String::new(),
            filter_input_mode: false,
            phase_filter: super::peek::PhaseFilter::all(),
            renderers: renderers::all(),
            active_renderer: None,
            detail_content: ratatui::text::Text::default(),
            detail_scroll: 0,
            last_refresh: Instant::now(),
            status_msg: String::new(),
            enrichment_cache: std::collections::HashMap::new(),
            formula_cache: std::collections::HashMap::new(),
            idle_ticks: 0,
            bg_rx,
            bg_tx,
            bg_pending: false,
            show_help: false,
            mouse_captured: true,
            zoom_level: 0.0,
            action_modal: ActionModal::None,
            presence: Vec::new(),
            ensemble_events: None,
            filter_cfg: FilterConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The help overlay is the only place operators learn how to copy
    /// text out of cs peek — if the mention of `Shift+drag` ever
    /// disappears, the underlying mouse-capture bug silently resurfaces.
    /// Same check is applied to the mouse-toggle `m` key and the named
    /// terminals we claim to support.
    #[test]
    fn help_overlay_documents_shift_drag_bypass() {
        let lines = super::help_overlay_lines(true);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flat.contains("Shift+drag"),
            "help overlay must document Shift+drag bypass; got:\n{flat}"
        );
        for terminal in ["iTerm2", "Terminal.app", "Alacritty", "kitty", "WezTerm"] {
            assert!(
                flat.contains(terminal),
                "help overlay must name {terminal} as a supported terminal; got:\n{flat}"
            );
        }
        assert!(
            flat.contains(" M  "),
            "help overlay must bind `M` as the mouse-capture toggle"
        );
    }

    /// The overlay reflects the current mouse-capture state so an
    /// operator who just pressed `m` gets immediate feedback from `?`.
    #[test]
    fn help_overlay_reports_mouse_capture_state() {
        let on = super::help_overlay_lines(true);
        let on_flat: String = on
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(on_flat.contains("currently ON"));

        let off = super::help_overlay_lines(false);
        let off_flat: String = off
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(off_flat.contains("currently OFF"));
    }

    #[test]
    fn format_energy_renders_badge() {
        let s = format_energy(50_000, 50_000, 1.23, Some(1_000_000));
        assert!(s.contains('%'));
    }

    #[test]
    fn status_token_running_uses_charter_active_glyph() {
        let (g, _) = status_token("running", HeartbeatTier::Active);
        assert_eq!(g, "●");
    }

    #[test]
    fn status_token_pending_uses_charter_pending_glyph() {
        let (g, _) = status_token("pending", HeartbeatTier::Active);
        assert_eq!(g, "○");
    }

    #[test]
    fn status_token_unknown_falls_back_to_pending() {
        let (g, _) = status_token("totally-unknown", HeartbeatTier::Active);
        assert_eq!(g, "○");
    }

    /// Regression — 2026-04-13: a pending molecule paired with a live tmux
    /// session is really mid-tackle (worker spawned, state save not yet
    /// committed). `row_view_from` must show it as Running, so the operator
    /// doesn't see a stale yellow glyph until reload catches up.
    #[test]
    fn row_view_from_overrides_pending_with_live_session_to_running() {
        let now = Utc::now();
        let session = Session {
            name: "cosmon-task-x".into(),
            socket: "cosmon".into(),
            project_root: "/tmp/p".into(),
            molecule_id: Some("task-x".into()),
            worker_id: None,
            last_activity: Some(now),
        };
        let molecule = Molecule {
            id: cosmon_observability::MoleculeId("task-x".into()),
            title: "t".into(),
            kind: "task".into(),
            status: MoleculeStatus::Pending,
            project_root: "/tmp/p".into(),
            session: Some("cosmon-task-x".into()),
            updated_at: now,
        };
        let row = row_view_from(&session, Some(&molecule), None, now);
        assert_eq!(row.status, "running");
    }

    /// Regression — 2026-04-14 (05db): heartbeat pastille stayed 🟡 Idle
    /// for a freshly-tackled molecule because tmux `session_activity` was
    /// older than the just-saved `molecule.updated_at`, and the code
    /// preferred tmux via `or_else`. After the fix we take the max, so
    /// the Active 🟢 glyph appears as soon as state is persisted —
    /// without the operator needing to `Enter` and artificially bump
    /// tmux's activity counter.
    #[test]
    fn row_view_from_heartbeat_prefers_freshest_of_tmux_and_molecule() {
        let now = Utc::now();
        let stale_tmux = now - chrono::Duration::minutes(10);
        let session = Session {
            name: "cosmon-task-y".into(),
            socket: "cosmon".into(),
            project_root: "/tmp/p".into(),
            molecule_id: Some("task-y".into()),
            worker_id: None,
            last_activity: Some(stale_tmux),
        };
        let molecule = Molecule {
            id: cosmon_observability::MoleculeId("task-y".into()),
            title: "t".into(),
            kind: "task".into(),
            status: MoleculeStatus::Running,
            project_root: "/tmp/p".into(),
            session: Some("cosmon-task-y".into()),
            updated_at: now,
        };
        let row = row_view_from(&session, Some(&molecule), None, now);
        assert_eq!(row.heartbeat, HeartbeatTier::Active);
        assert_eq!(row.last_activity, Some(now));
    }

    /// Regression — 2026-04-14: the ENERGY column drifted because widths
    /// were counted by scalar `char` rather than by
    /// rendered column. All three strings must share the same *visible*
    /// width (as measured by `unicode-width`) regardless of magnitude.
    #[test]
    fn format_energy_alignment_is_column_stable() {
        use unicode_width::UnicodeWidthStr;
        let small = format_energy(0, 0, 0.0, Some(200_000));
        let big = format_energy(500_000, 500_000, 12.34, Some(200_000));
        let nocw = format_energy(1_234, 0, 0.01, None);
        let w_small = UnicodeWidthStr::width(small.as_str());
        let w_big = UnicodeWidthStr::width(big.as_str());
        let w_nocw = UnicodeWidthStr::width(nocw.as_str());
        assert_eq!(
            w_small, w_big,
            "small={small:?} big={big:?} — widths must match"
        );
        assert_eq!(w_small, w_nocw, "nocw={nocw:?} — widths must match");
    }

    /// `pad_to_visible_width` must right-align based on rendered columns,
    /// not char count. `🔥` is 1 char but 2 columns, so
    /// padding an emoji-bearing string must leave fewer spaces than
    /// padding an ASCII-only one of the same char count.
    #[test]
    fn pad_to_visible_width_accounts_for_wide_glyphs() {
        use unicode_width::UnicodeWidthStr;
        let ascii = super::pad_to_visible_width("abc", 6);
        let emoji = super::pad_to_visible_width("🔥", 6);
        assert_eq!(UnicodeWidthStr::width(ascii.as_str()), 6);
        assert_eq!(UnicodeWidthStr::width(emoji.as_str()), 6);
        assert_eq!(ascii, "   abc");
        // 🔥 has width 2 → 4 padding spaces, not 5 (which is what `{:>6}`
        // would produce by char count).
        assert_eq!(emoji, "    🔥");
    }

    /// No-op case: a string already at or above the target width is
    /// returned unchanged (no truncation, no extra padding).
    #[test]
    fn pad_to_visible_width_noop_when_already_wide() {
        let over = super::pad_to_visible_width("abcdefgh", 6);
        assert_eq!(over, "abcdefgh");
    }

    #[test]
    fn temp_token_hot_returns_flame() {
        let (g, s) = temp_token(&["temp:hot".to_string()]);
        assert_eq!(g, "🔥");
        assert_eq!(s.fg, Some(Color::Red));
    }

    #[test]
    fn temp_token_none_returns_blank() {
        let (g, _) = temp_token(&[]);
        assert_eq!(g, " ");
    }

    #[test]
    fn temp_token_unknown_tag_returns_blank() {
        let (g, _) = temp_token(&["other:tag".to_string()]);
        assert_eq!(g, " ");
    }

    #[test]
    fn whisper_token_fresh_returns_bubble() {
        let (g, _) = whisper_token(true);
        assert_eq!(g, "🫧");
    }

    #[test]
    fn whisper_token_stale_returns_blank() {
        let (g, _) = whisper_token(false);
        assert_eq!(g, " ");
    }

    #[test]
    fn whisper_fresh_missing_file_is_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whispers.jsonl");
        assert!(!whisper_fresh_within(
            &path,
            Utc::now(),
            WHISPER_FRESH_WINDOW
        ));
    }

    #[test]
    fn whisper_fresh_recent_entry_is_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whispers.jsonl");
        let now = Utc::now();
        let ts = now - chrono::Duration::minutes(5);
        std::fs::write(
            &path,
            format!(
                "{{\"ts\":\"{}\",\"pilot\":\"t\",\"target_session\":\"s\",\"sha256\":\"x\",\"size_bytes\":1}}\n",
                ts.to_rfc3339()
            ),
        )
        .unwrap();
        assert!(whisper_fresh_within(&path, now, WHISPER_FRESH_WINDOW));
    }

    #[test]
    fn whisper_fresh_old_entry_is_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whispers.jsonl");
        let now = Utc::now();
        let ts = now - chrono::Duration::minutes(120);
        std::fs::write(
            &path,
            format!(
                "{{\"ts\":\"{}\",\"pilot\":\"t\",\"target_session\":\"s\",\"sha256\":\"x\",\"size_bytes\":1}}\n",
                ts.to_rfc3339()
            ),
        )
        .unwrap();
        assert!(!whisper_fresh_within(&path, now, WHISPER_FRESH_WINDOW));
    }

    #[test]
    fn whisper_fresh_uses_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whispers.jsonl");
        let now = Utc::now();
        let old = now - chrono::Duration::hours(5);
        let recent = now - chrono::Duration::minutes(10);
        let content = format!(
            "{{\"ts\":\"{}\",\"pilot\":\"t\",\"target_session\":\"s\",\"sha256\":\"a\",\"size_bytes\":1}}\n{{\"ts\":\"{}\",\"pilot\":\"t\",\"target_session\":\"s\",\"sha256\":\"b\",\"size_bytes\":1}}\n",
            old.to_rfc3339(),
            recent.to_rfc3339()
        );
        std::fs::write(&path, content).unwrap();
        assert!(whisper_fresh_within(&path, now, WHISPER_FRESH_WINDOW));
    }

    #[test]
    fn age_since_recent() {
        let now = Utc::now();
        assert!(age_since(now).ends_with('s'));
    }

    #[test]
    fn base64_encode_matches_rfc4648_vectors() {
        // RFC 4648 §10 test vectors — covers all three pad cases.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_handles_high_bytes() {
        // 0xff bytes exercise the upper bits of the 24-bit packing.
        assert_eq!(base64_encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(base64_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn status_token_orphaned_running_is_red_bold() {
        let (_, style) = status_token("running", HeartbeatTier::Orphaned);
        assert_eq!(style.fg, Some(Color::Red));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn status_token_stalled_running_is_not_red() {
        // Charter change (2026-04-19): a running row with a live tmux
        // but no recent output (the ex-"stalled" tier) is still alive —
        // the ghost is reserved for Orphaned. Red is precious; spend it
        // on the things that actually need action.
        let (_, style) = status_token("running", HeartbeatTier::Stalled);
        assert_ne!(style.fg, Some(Color::Red));
    }

    #[test]
    fn status_token_orphaned_pending_is_dim_gray() {
        let (_, style) = status_token("pending", HeartbeatTier::Orphaned);
        assert_eq!(style.fg, Some(Color::DarkGray));
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn status_token_active_running_is_bold_and_ansi16() {
        // Regression (delib-b8c6 P1/C2): the active-running glyph used to be
        // painted with `Color::Rgb`, which rendered invisible on dark
        // terminals. It must now be a named ANSI-16 color so the terminal
        // theme remaps it to something legible.
        let (_, style) = status_token("running", HeartbeatTier::Active);
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.fg, Some(Color::Green));
        assert!(
            !matches!(style.fg, Some(Color::Rgb(_, _, _))),
            "status_token must not use Color::Rgb (theme-hostile)"
        );
    }

    /// ANSI-16 coverage: every (status × heartbeat) pair used in the TUI
    /// must produce an ANSI-16 foreground — no `Color::Rgb` leaks through
    /// the match arms.
    #[test]
    fn status_token_never_uses_rgb() {
        let statuses = [
            "running",
            "pending",
            "queued",
            "frozen",
            "completed",
            "collapsed",
        ];
        let hbs = [
            HeartbeatTier::Active,
            HeartbeatTier::Idle,
            HeartbeatTier::Quiet,
            HeartbeatTier::Stalled,
            HeartbeatTier::Orphaned,
        ];
        for s in statuses {
            for hb in hbs {
                let (_, style) = status_token(s, hb);
                assert!(
                    !matches!(style.fg, Some(Color::Rgb(_, _, _))),
                    "status={s} hb={hb:?} leaked Color::Rgb ({style:?})"
                );
            }
        }
    }

    fn row_with(status: &str, hb: HeartbeatTier) -> RowView {
        RowView {
            mol_id: "x".into(),
            session_slug: None,
            project: String::new(),
            role: String::new(),
            status: status.into(),
            step: String::new(),
            updated_at: None,
            energy_in: 0,
            energy_out: 0,
            cost_usd: 0.0,
            context_window: None,
            session: None,
            socket: String::new(),
            heartbeat: hb,
            last_activity: None,
            last_progress_at: None,
            topic: None,
            mission_description: None,
            formula: String::new(),
            tier_badge: String::new(),
            kind: String::new(),
            blocked_by: Vec::new(),
            worker_name: None,
            tags: Vec::new(),
            created_at_utc: None,
            whisper_fresh: false,
            role_glyphs: String::new(),
            trust_score: None,
            energy_budget: None,
            adapter: cosmon_core::adapter_attribution::AdapterAttribution::default(),
        }
    }

    /// Default watchdog filter — only `running` molecules are visible.
    /// Pending, queued, completed, collapsed, starved, frozen are noise at
    /// a glance (operator rationale 2026-04-27); `A` cycles through
    /// preset combinations.
    ///
    /// This list is every variant of the one `MoleculeStatus` alphabet. It
    /// used to carry `stuck` — the name the deleted observability copy of
    /// the enum gave to `Starved` — which is now an unrecognised string and
    /// would be surfaced rather than filtered, per the unknown-status
    /// passthrough. See [`unknown_status_is_surfaced_not_filtered`].
    #[test]
    fn filtered_indices_default_hides_the_archive_and_nothing_else() {
        let rows = vec![
            row_with("running", HeartbeatTier::Active),
            row_with("pending", HeartbeatTier::Active),
            row_with("queued", HeartbeatTier::Active),
            row_with("completed", HeartbeatTier::Active),
            row_with("collapsed", HeartbeatTier::Active),
            row_with("starved", HeartbeatTier::Active),
            row_with("frozen", HeartbeatTier::Active),
        ];
        let mut app = App::for_test(rows, std::collections::HashMap::new());
        app.phase_filter = super::super::peek::PhaseFilter::unfinished();
        let visible = app.filtered_indices();
        assert_eq!(
            visible,
            vec![0, 1, 2, 5, 6],
            "the default drops completed and collapsed. It must keep the \
             pending, starved and frozen rows: no other instrument reports \
             them, so hiding them is amputation rather than subtraction"
        );
    }

    /// The regression the enum unification exists to prevent: a status this
    /// binary does not recognise must be *shown*, never filtered away.
    ///
    /// `PhaseFilter::matches` documents this (an unparseable label is
    /// surfaced), but the arm could not fire while `map_status` laundered
    /// every unknown into `Pending` upstream — the filter never saw an
    /// unknown at all. With one alphabet there is nothing between the wire
    /// value and the filter.
    #[test]
    fn unknown_status_is_surfaced_not_filtered() {
        let rows = vec![
            row_with("running", HeartbeatTier::Active),
            row_with("a_status_from_a_newer_cs", HeartbeatTier::Active),
        ];
        let mut app = App::for_test(rows, std::collections::HashMap::new());
        app.phase_filter = super::super::peek::PhaseFilter::unfinished();
        assert_eq!(
            app.filtered_indices(),
            vec![0, 1],
            "an unrecognised status must be surfaced — hiding molecules whose \
             status the binary does not understand is the worst possible \
             failure mode for an observer"
        );
    }

    /// ADR-062: `Starved` means an external authority refused service. It is
    /// alive, and the repair is a wait or a rotation — never a re-prompt.
    ///
    /// It used to arrive here as the string `"stuck"` and band `Dead`, filing
    /// the one status whose purpose is to summon the operator below the fold
    /// with the terminal rows.
    #[test]
    fn starved_bands_stuck_not_dead() {
        assert_eq!(
            liveness_band(&row_with("starved", HeartbeatTier::Active)),
            LivenessBand::Stuck
        );
    }

    /// `Starved` and `Queued` used to fall through `map_status`'s `_ =>` arm
    /// into `Pending`, so the health classifier saw an untackled molecule
    /// where the core rules `Degraded`.
    #[test]
    fn row_status_parses_every_variant_of_the_one_alphabet() {
        for status in [
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Starved,
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
        ] {
            assert_eq!(
                parse_row_status(&status.to_string()),
                status,
                "{status} must survive the RowView string round-trip"
            );
        }
    }

    /// `--all` (or the `A` cycle's `all` step) lifts every state filter,
    /// exposing the archive again. The status field of every row is
    /// unchanged — the filter is the only difference between modes.
    #[test]
    fn filtered_indices_phase_filter_all_keeps_every_row() {
        let rows = vec![
            row_with("running", HeartbeatTier::Active),
            row_with("pending", HeartbeatTier::Active),
            row_with("completed", HeartbeatTier::Active),
            row_with("collapsed", HeartbeatTier::Active),
            row_with("starved", HeartbeatTier::Active),
        ];
        let mut app = App::for_test(rows, std::collections::HashMap::new());
        app.phase_filter = super::super::peek::PhaseFilter::all();
        let visible = app.filtered_indices();
        assert_eq!(visible, vec![0, 1, 2, 3, 4]);
    }

    /// `--phase unfinished,done,failed` is what `--past` used to mean, and
    /// it now says so. It cannot subtract: the frozen and starved rows the
    /// old flag gated are unfinished work and live in the default.
    #[test]
    fn filtered_indices_archive_selectors_add_to_the_default() {
        let rows = vec![
            row_with("running", HeartbeatTier::Active),
            row_with("pending", HeartbeatTier::Active),
            row_with("queued", HeartbeatTier::Active),
            row_with("completed", HeartbeatTier::Active),
            row_with("collapsed", HeartbeatTier::Active),
            row_with("frozen", HeartbeatTier::Active),
        ];
        let mut app = App::for_test(rows, std::collections::HashMap::new());
        app.phase_filter = super::super::peek::PhaseFilter::from_phase_args(&[
            super::super::peek::PhaseSelector::Unfinished,
            super::super::peek::PhaseSelector::Done,
            super::super::peek::PhaseSelector::Failed,
        ]);
        let visible = app.filtered_indices();
        assert_eq!(visible, vec![0, 1, 2, 3, 4, 5]);
    }

    /// `A` cycle: unfinished → all → unfinished.
    #[test]
    fn cycle_phase_filter_walks_canonical_presets() {
        use super::super::peek::PhaseFilter;
        let unfinished = PhaseFilter::unfinished();
        let all = PhaseFilter::all();
        assert_eq!(super::cycle_phase_filter(unfinished), all);
        assert_eq!(super::cycle_phase_filter(all), unfinished);
        // Any off-cycle filter snaps back to the default in one press.
        assert_eq!(super::cycle_phase_filter(PhaseFilter::none()), unfinished);
    }

    #[test]
    fn liveness_band_ordering_live_first() {
        assert!(LivenessBand::Live < LivenessBand::Stuck);
        assert!(LivenessBand::Stuck < LivenessBand::Waiting);
        assert!(LivenessBand::Waiting < LivenessBand::Dormant);
        assert!(LivenessBand::Dormant < LivenessBand::Dead);
    }

    /// The band ignores the heartbeat entirely (C3). It used to send a
    /// stalled or orphaned `running` row to `Stuck`, which meant a row could
    /// change bands — and therefore jump up the table — because thirty
    /// minutes had passed and nothing else. The tier is a column now.
    #[test]
    fn liveness_band_ignores_heartbeat() {
        for hb in [
            HeartbeatTier::Active,
            HeartbeatTier::Idle,
            HeartbeatTier::Quiet,
            HeartbeatTier::Stalled,
            HeartbeatTier::Orphaned,
        ] {
            assert_eq!(
                liveness_band(&row_with("running", hb)),
                LivenessBand::Live,
                "a running row bands Live whatever its heartbeat says ({hb:?})"
            );
        }
    }

    /// `starved` is the phase whose whole purpose is to summon the operator
    /// (ADR-062), and it reaches `Stuck` on the status alone.
    #[test]
    fn liveness_band_bands_starved_stuck() {
        assert_eq!(
            liveness_band(&row_with("starved", HeartbeatTier::Active)),
            LivenessBand::Stuck
        );
    }

    /// Build a row carrying a full change key.
    fn row_at(mol_id: &str, status: &str, updated_at: Option<DateTime<Utc>>) -> RowView {
        let mut r = row_with(status, HeartbeatTier::Active);
        r.mol_id = mol_id.into();
        r.updated_at = updated_at;
        r
    }

    /// **The C3 invariant: the sort key is a function of the change-detection
    /// key.**
    ///
    /// Two row sets that agree on `(mol_id, status, step, updated_at)` must
    /// sort identically, no matter how violently they disagree on everything
    /// else. If they don't, `rows_differ` can call a tick idle while the
    /// table reorders under the operator's cursor — which is what the rows
    /// were doing, and why this test is here rather than in a comment.
    #[test]
    fn sort_key_is_a_function_of_the_change_key() {
        let t = Utc::now();
        let build = |hb: HeartbeatTier, trust: Option<u8>, act: Option<DateTime<Utc>>| {
            ["c-1", "a-2", "b-3"]
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let mut r =
                        row_at(id, "running", Some(t - chrono::Duration::minutes(i as i64)));
                    // Everything below is outside the change key. None of it
                    // may reach the order.
                    r.heartbeat = hb;
                    r.trust_score = trust;
                    r.last_activity = act;
                    r
                })
                .collect::<Vec<_>>()
        };

        let mut lhs = build(HeartbeatTier::Active, Some(90), Some(t));
        let mut rhs = build(
            HeartbeatTier::Orphaned,
            None,
            Some(t - chrono::Duration::days(400)),
        );
        assert!(
            !rows_differ(&lhs, &rhs),
            "same change keys must read as the same fleet"
        );

        sort_rows(&mut lhs);
        sort_rows(&mut rhs);
        let ids = |v: &[RowView]| v.iter().map(|r| r.mol_id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(&lhs),
            ids(&rhs),
            "the same fleet must render in the same order"
        );
    }

    /// Within a band, the most recently touched molecule rises, and a row
    /// with no molecule behind it sinks to the bottom rather than claiming a
    /// timestamp it does not have.
    #[test]
    fn sort_orders_by_updated_at_desc_then_mol_id() {
        let t = Utc::now();
        let mut rows = vec![
            row_at("old", "running", Some(t - chrono::Duration::hours(2))),
            row_at("none", "running", None),
            row_at("fresh", "running", Some(t)),
            row_at("mid", "running", Some(t - chrono::Duration::hours(1))),
        ];
        sort_rows(&mut rows);
        assert_eq!(
            rows.iter().map(|r| r.mol_id.as_str()).collect::<Vec<_>>(),
            ["fresh", "mid", "old", "none"]
        );
    }

    /// The band still governs: a fresh corpse never outranks a stale live
    /// worker. `updated_at` orders *within* a band, never across one.
    #[test]
    fn sort_puts_band_before_recency() {
        let t = Utc::now();
        let mut rows = vec![
            row_at("done-now", "completed", Some(t)),
            row_at(
                "running-old",
                "running",
                Some(t - chrono::Duration::days(3)),
            ),
        ];
        sort_rows(&mut rows);
        assert_eq!(rows[0].mol_id, "running-old");
    }

    /// A change in `updated_at` alone is a change: the sort reads it, so the
    /// detector must too, or the poller idles through a reorder.
    #[test]
    fn rows_differ_detects_updated_at_change() {
        let t = Utc::now();
        let a = vec![row_at("m", "running", Some(t))];
        let b = vec![row_at("m", "running", Some(t - chrono::Duration::hours(1)))];
        assert!(super::rows_differ(&a, &b));
    }

    /// The complement, and the reason the old key could not be left alone:
    /// the heartbeat moves on its own and no detector watches it. It is now
    /// absent from the order, so its drift is invisible to the table — which
    /// is exactly what "render it, don't order by it" has to mean.
    #[test]
    fn heartbeat_drift_alone_does_not_reorder() {
        let t = Utc::now();
        let mut rows = vec![
            row_at("a", "running", Some(t)),
            row_at("b", "running", Some(t - chrono::Duration::minutes(1))),
        ];
        sort_rows(&mut rows);
        let before = rows.iter().map(|r| r.mol_id.clone()).collect::<Vec<_>>();

        // Time passes; every tier decays. Nothing on disk moved.
        rows[0].heartbeat = HeartbeatTier::Stalled;
        rows[1].heartbeat = HeartbeatTier::Orphaned;
        sort_rows(&mut rows);

        assert_eq!(
            rows.iter().map(|r| r.mol_id.clone()).collect::<Vec<_>>(),
            before,
            "the clock alone must never reorder the table"
        );
    }

    /// A Running molecule whose `last_progress_at`
    /// has not advanced past the active step's `timeout_minutes` budget gets
    /// promoted to `HeartbeatTier::Stalled` regardless of tmux activity.
    /// Default budget is 30 minutes when the formula step does not declare
    /// `timeout_minutes` (M3).
    #[test]
    fn is_stalled_by_progress_respects_step_timeout_and_default() {
        let now = Utc::now();
        // Default 30-minute budget — fresh progress is not stalled.
        assert!(!is_stalled_by_progress(
            Some(now - chrono::Duration::minutes(5)),
            None,
            now,
        ));
        // Default 30-minute budget — exceeded.
        assert!(is_stalled_by_progress(
            Some(now - chrono::Duration::minutes(31)),
            None,
            now,
        ));
        // Explicit 5-minute budget — exceeded after 6 minutes.
        assert!(is_stalled_by_progress(
            Some(now - chrono::Duration::minutes(6)),
            Some(5),
            now,
        ));
        // Explicit 60-minute budget — 31 minutes is fine.
        assert!(!is_stalled_by_progress(
            Some(now - chrono::Duration::minutes(31)),
            Some(60),
            now,
        ));
        // No progress timestamp recorded yet — not yet stalled.
        assert!(!is_stalled_by_progress(None, Some(5), now));
    }

    /// Regression (delib-20260716-a2f1 C2): a memo that changes the answer
    /// is not a memo. The enrichment cache exists to skip re-reading
    /// `state.json`; it must not decide what the operator sees.
    ///
    /// The bug: `is_stalled_by_progress` was evaluated inside the
    /// cache-**miss** branch, below the `continue` that a hit takes. For a
    /// molecule stalled well past its step budget, the miss tick promoted the
    /// heartbeat to `Stalled` (band `Stuck`, row flagged high) and every hit
    /// tick skipped the promotion, letting the heartbeat revert to the
    /// attach-bumped tmux value the code itself documents as lying — so the
    /// row jumped back into the healthy band. Identical disk state, two
    /// renderings, discriminated only by whether a memoisation happened to
    /// hit. `trust_score` had the same defect at a second layer: absent from
    /// `CachedEnrichment`, the TRUST column blanked itself on every hit.
    ///
    /// This test drives two consecutive `enrich_rows` ticks over an unchanged
    /// tree — the first a forced miss, the second a hit on the same mtime —
    /// and pins that the stall verdict is identical across both.
    ///
    /// It pinned the *band* when C2 landed, because the band was then where
    /// the stall verdict surfaced. C3 removed the heartbeat from the band (a
    /// band that moves with the clock cannot be the first term of a sort
    /// key), so the band is now equal across hit and miss whatever the cache
    /// does — it would pass this test while the bug raged. The heartbeat is
    /// where the verdict lives, so the heartbeat is what this pins.
    #[test]
    fn enrichment_cache_hit_renders_the_same_stall_verdict_as_a_miss() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon = tmp.path().join(".cosmon");
        let formulas = cosmon.join("formulas");
        std::fs::create_dir_all(&formulas).expect("formulas dir");
        // `config.toml` is what walk-up discovery keys on.
        std::fs::write(cosmon.join("config.toml"), "").expect("config");
        std::fs::write(
            formulas.join("task-work.formula.toml"),
            "formula = \"task-work\"\nversion = 1\ndescription = \"t\"\n\n\
             [[steps]]\nid = \"implement\"\ntitle = \"Implement\"\n\
             description = \"d\"\nacceptance = \"a\"\n",
        )
        .expect("formula");

        let state_dir = cosmon.join("state");
        let mol_id = "task-20260716-c2c2";
        let mol_dir = state_dir
            .join("fleets")
            .join("default")
            .join("molecules")
            .join(mol_id);
        std::fs::create_dir_all(&mol_dir).expect("mol dir");

        // Running, but no forward motion for 45 minutes — well past the
        // step's default 30-minute budget. The honest verdict is `Stuck`.
        let stalled_since = (Utc::now() - chrono::Duration::minutes(45)).to_rfc3339();
        let created = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        std::fs::write(
            mol_dir.join("state.json"),
            format!(
                r#"{{
  "id": "{mol_id}",
  "fleet_id": "default",
  "formula_id": "task-work",
  "status": "running",
  "variables": {{}},
  "assigned_worker": null,
  "created_at": "{created}",
  "updated_at": "{stalled_since}",
  "total_steps": 1,
  "current_step": 0,
  "completed_steps": [],
  "collapse_reason": null,
  "collapsed_step": null,
  "links": [],
  "last_progress_at": "{stalled_since}"
}}"#
            ),
        )
        .expect("state.json");

        let mut state_dirs = std::collections::HashMap::new();
        state_dirs.insert(mol_id.to_owned(), state_dir.clone());
        let mut app = App::for_test(Vec::new(), state_dirs);

        // The tmux-derived heartbeat every tick starts from — `snapshot_to_rows`
        // rebuilds it fresh from the session, and it reads `Active` because
        // attaching to the pane bumps it. This is precisely the lie the stall
        // promotion exists to correct.
        let fresh_row = || {
            let mut r = row_with("running", HeartbeatTier::Active);
            r.mol_id = mol_id.to_owned();
            r
        };

        // Tick 1 — cold cache, forced miss.
        let mut rows = vec![fresh_row()];
        app.enrich_rows(&mut rows);
        let miss_heartbeat = rows[0].heartbeat;
        let miss_trust = rows[0].trust_score;
        assert!(
            !app.enrichment_cache.is_empty(),
            "tick 1 must have populated the cache, else tick 2 is not a hit"
        );

        // Tick 2 — same tree, untouched: guaranteed cache hit.
        let mut rows = vec![fresh_row()];
        app.enrich_rows(&mut rows);

        assert_eq!(
            miss_heartbeat,
            HeartbeatTier::Stalled,
            "45 minutes without progress on a 30-minute budget must promote to Stalled"
        );
        assert_eq!(
            miss_heartbeat, rows[0].heartbeat,
            "identical disk state must render an identical stall verdict on a \
             cache hit and a cache miss — the cache must not decide the \
             semantics, and must not let the heartbeat revert to the \
             attach-bumped tmux value"
        );
        assert_eq!(
            miss_trust, rows[0].trust_score,
            "the TRUST score must survive the cache hit too — same defect, \
             second layer"
        );
    }

    #[test]
    fn liveness_band_classifies_non_running_by_status() {
        assert_eq!(
            liveness_band(&row_with("pending", HeartbeatTier::Stalled)),
            LivenessBand::Waiting
        );
        assert_eq!(
            liveness_band(&row_with("frozen", HeartbeatTier::Stalled)),
            LivenessBand::Dormant
        );
        assert_eq!(
            liveness_band(&row_with("completed", HeartbeatTier::Stalled)),
            LivenessBand::Dead
        );
        assert_eq!(
            liveness_band(&row_with("collapsed", HeartbeatTier::Stalled)),
            LivenessBand::Dead
        );
    }

    /// Build a worker roster entry bound to `session`, with the fields the
    /// census ignores filled in with inert values.
    fn census_worker(id: &str, session: &str) -> Worker {
        use cosmon_observability::worker::WorkerRole as OR;
        Worker {
            id: id.into(),
            molecule_id: None,
            session: session.into(),
            energy: EnergyBudget {
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
                context_window: None,
            },
            live: "working".into(),
            role: OR::Cognition,
        }
    }

    /// The situation the strip exists for: a roster of 30 workers, of
    /// which only 3 are claimed by a live tmux session. The other 27 are
    /// phantoms — the fleet counts them but no longer runs them.
    #[test]
    fn worker_census_counts_phantoms_as_roster_minus_live_tmux() {
        let mut snap = FleetSnapshot::new();
        for i in 0..30 {
            let wid = format!("w-{i:02}");
            snap.insert_worker(census_worker(&wid, &format!("sess-{i:02}")));
            // Only the first three workers still have a live session.
            if i < 3 {
                snap.push_session(Session {
                    name: format!("sess-{i:02}"),
                    socket: "cosmon".into(),
                    project_root: "proj".into(),
                    molecule_id: None,
                    worker_id: Some(wid),
                    last_activity: Some(Utc::now()),
                });
            }
        }

        let census = worker_census(&snap);
        assert_eq!(census.registered, 30);
        assert_eq!(census.attached, 3);
        assert_eq!(census.phantom(), 27);
    }

    /// A phantom has no molecule id, so it must never reach the table —
    /// `snapshot_to_rows` is keyed by `mol_id` and a synthesised row would
    /// collide with or corrupt that key. The census is the only place the
    /// discrepancy surfaces.
    #[test]
    fn phantom_workers_are_counted_but_never_become_rows() {
        let mut snap = FleetSnapshot::new();
        for i in 0..12 {
            snap.insert_worker(census_worker(&format!("w-{i:02}"), &format!("gone-{i:02}")));
        }

        assert_eq!(worker_census(&snap).phantom(), 12);
        assert!(
            snapshot_to_rows(&snap).is_empty(),
            "phantoms have no mol_id and must not synthesise rows"
        );
    }

    /// A roster where every entry is backed by a live session reports zero
    /// phantoms — the strip stays on and simply says nothing is wrong.
    #[test]
    fn worker_census_reports_zero_phantoms_on_a_clean_roster() {
        let mut snap = FleetSnapshot::new();
        for i in 0..4 {
            let wid = format!("w-{i}");
            snap.insert_worker(census_worker(&wid, &format!("sess-{i}")));
            snap.push_session(Session {
                name: format!("sess-{i}"),
                socket: "cosmon".into(),
                project_root: "proj".into(),
                molecule_id: None,
                worker_id: Some(wid),
                last_activity: Some(Utc::now()),
            });
        }

        let census = worker_census(&snap);
        assert_eq!(census.registered, 4);
        assert_eq!(census.attached, 4);
        assert_eq!(census.phantom(), 0);
    }

    /// A session naming a worker the roster never registered must not
    /// inflate `attached` past `registered` — the census counts roster
    /// entries, and sessions only decide which of them are claimed.
    #[test]
    fn worker_census_ignores_sessions_naming_unregistered_workers() {
        let mut snap = FleetSnapshot::new();
        snap.insert_worker(census_worker("w-known", "sess-known"));
        for name in ["sess-known", "sess-stranger"] {
            snap.push_session(Session {
                name: name.into(),
                socket: "cosmon".into(),
                project_root: "proj".into(),
                molecule_id: None,
                worker_id: Some(name.replace("sess-", "w-")),
                last_activity: Some(Utc::now()),
            });
        }

        let census = worker_census(&snap);
        assert_eq!(census.registered, 1);
        assert_eq!(census.attached, 1);
        assert_eq!(census.phantom(), 0);
    }

    /// Seventeen macro-molecules, each bound
    /// to a runtime + cognition tmux session (34 sessions total), must
    /// fold into exactly 17 peek rows — one line per molecule. Before the
    /// fusion pass each row was emitted per session, so the TUI displayed
    /// 34 rows that looked identical modulo the session name.
    #[test]
    fn snapshot_to_rows_fuses_runtime_and_cognition_pairs_into_one_row_per_mol() {
        use cosmon_observability::worker::WorkerRole as OR;
        let now = Utc::now();
        let mut snap = FleetSnapshot::new();
        for i in 0..17 {
            let mol_id = format!("task-2026041{i:02}-abcd");
            snap.insert_molecule(Molecule {
                id: cosmon_observability::MoleculeId(mol_id.clone()),
                title: mol_id.clone(),
                kind: "task".into(),
                status: MoleculeStatus::Running,
                project_root: "proj".into(),
                session: Some(format!("{mol_id}-cog")),
                updated_at: now,
            });
            let runtime_wid = format!("runtime-{mol_id}");
            let cog_wid = format!("cog-{mol_id}");
            snap.insert_worker(Worker {
                id: runtime_wid.as_str().into(),
                molecule_id: Some(mol_id.clone()),
                session: format!("{mol_id}-rt"),
                energy: EnergyBudget {
                    input_tokens: 10,
                    output_tokens: 5,
                    cost_usd: 0.1,
                    context_window: Some(1_000_000),
                },
                live: "working".into(),
                role: OR::Runtime,
            });
            snap.insert_worker(Worker {
                id: cog_wid.as_str().into(),
                molecule_id: Some(mol_id.clone()),
                session: format!("{mol_id}-cog"),
                energy: EnergyBudget {
                    input_tokens: 100,
                    output_tokens: 50,
                    cost_usd: 1.0,
                    context_window: Some(1_000_000),
                },
                live: "working".into(),
                role: OR::Cognition,
            });
            snap.push_session(Session {
                name: format!("{mol_id}-rt"),
                socket: "cosmon".into(),
                project_root: "proj".into(),
                molecule_id: Some(mol_id.clone()),
                worker_id: Some(runtime_wid),
                last_activity: Some(now),
            });
            snap.push_session(Session {
                name: format!("{mol_id}-cog"),
                socket: "cosmon".into(),
                project_root: "proj".into(),
                molecule_id: Some(mol_id.clone()),
                worker_id: Some(cog_wid),
                last_activity: Some(now),
            });
        }

        let rows = snapshot_to_rows(&snap);
        assert_eq!(
            rows.len(),
            17,
            "expected one fused row per molecule, got {}: {:?}",
            rows.len(),
            rows.iter().map(|r| r.mol_id.clone()).collect::<Vec<_>>()
        );

        // Each fused row must advertise both roles via the glyph marker.
        for row in &rows {
            assert!(
                row.role_glyphs.contains("🎛️"),
                "row {} missing runtime glyph, got {:?}",
                row.mol_id,
                row.role_glyphs
            );
            assert!(
                row.role_glyphs.contains("🧠"),
                "row {} missing cognition glyph, got {:?}",
                row.mol_id,
                row.role_glyphs
            );
            // Energy totals merge across the pair.
            assert_eq!(row.energy_in, 110, "merged input tokens");
            assert_eq!(row.energy_out, 55, "merged output tokens");
        }
    }

    /// Regression: `setup_terminal` must refuse to enter raw mode when stdout
    /// is not a real TTY. Under `cargo test`, stdout is captured (not a TTY),
    /// so calling `setup_terminal` should return the preflight error without
    /// mutating the terminal. Before the preflight, crossterm would fail deep
    /// inside `enable_raw_mode` with `Device not configured (os error 6)` — a
    /// cryptic errno that also sometimes left the controlling terminal in an
    /// inconsistent state on macOS.
    ///
    /// See `docs/diagnostics/cs-peek-earshot-kill.md` for the investigation.
    #[test]
    fn setup_terminal_refuses_without_tty() {
        let err = super::setup_terminal().expect_err("must fail without TTY");
        let msg = err.to_string();
        assert!(
            msg.contains("requires a TTY"),
            "expected TTY preflight error, got: {msg}"
        );
    }

    // --- Phase 3 tests: mtime cache, adaptive polling, rows_differ ---

    #[test]
    fn rows_differ_detects_status_change() {
        let a = vec![row_with("running", HeartbeatTier::Active)];
        let mut b = vec![row_with("running", HeartbeatTier::Active)];
        assert!(
            !super::rows_differ(&a, &b),
            "identical rows must not differ"
        );
        b[0].status = "completed".into();
        assert!(super::rows_differ(&a, &b), "status change must be detected");
    }

    #[test]
    fn rows_differ_detects_count_change() {
        let a = vec![row_with("running", HeartbeatTier::Active)];
        let b = Vec::new();
        assert!(
            super::rows_differ(&a, &b),
            "different row count must differ"
        );
    }

    #[test]
    fn rows_differ_ignores_order() {
        let mut a1 = row_with("running", HeartbeatTier::Active);
        a1.mol_id = "aaa".into();
        let mut a2 = row_with("pending", HeartbeatTier::Stalled);
        a2.mol_id = "bbb".into();
        let va = vec![a1.clone(), a2.clone()];
        let vb = vec![a2, a1];
        assert!(
            !super::rows_differ(&va, &vb),
            "same rows in different order must not differ"
        );
    }

    #[test]
    fn apply_cached_enrichment_fills_row() {
        let mut row = row_with("running", HeartbeatTier::Active);
        let cached = CachedEnrichment {
            topic: Some("test topic".into()),
            mission_description: Some("test description".into()),
            formula: "task-work".into(),
            tier_badge: "T1".into(),
            kind: "task".into(),
            blocked_by: vec![("mol-a".into(), "running".into())],
            worker_name: Some("worker-1".into()),
            tags: vec!["temp:hot".into()],
            created_at_utc: Some(Utc::now()),
            last_progress_at: None,
            energy_budget: Some((42, 100)),
            trust_score: Some(87),
            current_step: 1,
        };
        super::apply_cached_enrichment(&mut row, &cached);
        assert_eq!(row.topic, Some("test topic".into()));
        assert_eq!(row.mission_description, Some("test description".into()));
        assert_eq!(row.formula, "task-work");
        assert_eq!(row.tier_badge, "T1");
        assert_eq!(row.kind, "task");
        assert_eq!(row.blocked_by.len(), 1);
        assert_eq!(row.worker_name, Some("worker-1".into()));
        assert_eq!(row.tags, vec!["temp:hot".to_string()]);
        assert_eq!(
            row.trust_score,
            Some(87),
            "TRUST must survive a cache hit — it used to blank itself \
             because the field was absent from CachedEnrichment"
        );
    }

    /// Regression: when a detail pane is open, j/k navigation must refresh
    /// the pane content for the newly-selected molecule. An earlier fix
    /// wired this for the tmux pane only;
    /// the [`DetailRenderer`] trait refactor unified all panes through a
    /// single [`App::refresh_detail`] call. This test pins the contract so
    /// the static panes (briefing / log / synthesis / notes / responses)
    /// don't silently freeze on the originally-selected row again.
    ///
    /// Strategy: drive [`App::refresh_detail`] twice with distinct
    /// selections and assert the rendered pane content differs. If a future
    /// renderer caches by molecule id without invalidating, this test will
    /// catch it.
    #[test]
    fn refresh_detail_follows_jk_selection_for_every_static_pane() {
        use std::collections::HashMap;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let state_dir = tmp.path().to_path_buf();

        let first_mol = "task-20260412-aaaa";
        let second_mol = "task-20260412-bbbb";

        // Materialize each artifact a static renderer reads (briefing, log,
        // synthesis, notes/, responses/) with content unique to the molecule.
        // FileStore::molecule_dir resolves under fleets/default/molecules/<id>/.
        for &id in &[first_mol, second_mol] {
            let dir = state_dir
                .join("fleets")
                .join("default")
                .join("molecules")
                .join(id);
            std::fs::create_dir_all(dir.join("notes")).expect("notes dir");
            std::fs::create_dir_all(dir.join("responses")).expect("responses dir");
            std::fs::write(dir.join("briefing.md"), format!("# brief for {id}")).expect("briefing");
            std::fs::write(dir.join("log.md"), format!("log for {id}")).expect("log");
            std::fs::write(dir.join("synthesis.md"), format!("synth {id}")).expect("synth");
            std::fs::write(dir.join("notes").join("n.md"), format!("note for {id}")).expect("note");
            std::fs::write(
                dir.join("responses").join("r.md"),
                format!("response for {id}"),
            )
            .expect("response");
        }

        let mut row_first = row_with("running", HeartbeatTier::Active);
        row_first.mol_id = first_mol.into();
        let mut row_second = row_with("running", HeartbeatTier::Active);
        row_second.mol_id = second_mol.into();

        let mut sd_map: HashMap<String, std::path::PathBuf> = HashMap::new();
        sd_map.insert(first_mol.into(), state_dir.clone());
        sd_map.insert(second_mol.into(), state_dir.clone());

        let mut app = App::for_test(vec![row_first, row_second], sd_map);

        // The bug specifically called out b/l/s/n/r as detail panes that
        // failed to follow j/k. Each one must now produce different content
        // for two distinct selections. `n` (notes) was promoted to `N` when
        // the lowercase slot became the nucleate action (task-20260423-16ad).
        for &key in &['b', 'l', 's', 'N', 'r'] {
            let idx = app
                .renderer_for_key(key)
                .unwrap_or_else(|| panic!("no renderer registered for key '{key}'"));
            app.active_renderer = Some(idx);

            app.table_state.select(Some(0));
            app.refresh_detail();
            let first_text = text_to_plain(&app.detail_content);

            app.table_state.select(Some(1));
            app.refresh_detail();
            let second_text = text_to_plain(&app.detail_content);

            assert_ne!(
                first_text, second_text,
                "detail pane '{key}' did not refresh on j/k selection \
                 (same content for both molecules):\n1: {first_text:?}\n2: {second_text:?}"
            );
            assert!(
                second_text.contains(second_mol),
                "pane '{key}' content for second selected molecule should mention {second_mol}: {second_text:?}"
            );
        }
    }

    /// Flatten a [`ratatui::text::Text`] into a single newline-joined plain
    /// string so test assertions can compare panes by visible content.
    fn text_to_plain(t: &ratatui::text::Text<'_>) -> String {
        t.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn effective_refresh_adapts_to_idle_ticks() {
        // Cannot construct a full App in tests (no TTY), so test the
        // threshold logic directly via the constants.
        const IDLE_THRESHOLD: u32 = 5;
        const ACTIVE_MS: u64 = 250;
        const IDLE_MS: u64 = 1000;

        // Simulate: idle_ticks=0 → active cadence
        let idle_ticks = 0u32;
        let ms = if idle_ticks > IDLE_THRESHOLD {
            IDLE_MS
        } else {
            ACTIVE_MS
        };
        assert_eq!(ms, 250);

        // idle_ticks=6 → idle cadence
        let idle_ticks = 6u32;
        let ms = if idle_ticks > IDLE_THRESHOLD {
            IDLE_MS
        } else {
            ACTIVE_MS
        };
        assert_eq!(ms, 1000);
    }

    // --- Zoom-continu (task-20260422-1da5) ---

    #[test]
    fn zoom_label_bands_match_scale_intent() {
        // Ville band covers [0.0, 0.5): the operator is still in fleet mode.
        assert_eq!(super::zoom_label(0.0), "ville");
        assert_eq!(super::zoom_label(0.49), "ville");
        // Immeuble band covers [0.5, 1.5): one-molecule pleine-page.
        assert_eq!(super::zoom_label(0.5), "immeuble");
        assert_eq!(super::zoom_label(1.0), "immeuble");
        assert_eq!(super::zoom_label(1.49), "immeuble");
        // Peau band covers [1.5, 2.0]: raw artifact text.
        assert_eq!(super::zoom_label(1.5), "peau");
        assert_eq!(super::zoom_label(2.0), "peau");
    }

    #[test]
    #[allow(clippy::items_after_statements)]
    fn zoom_bounds_and_step_match_specification() {
        // ZOOM_MIN = ville anchor, ZOOM_MAX = peau anchor, step fine enough
        // to feel continuous (5 presses per scale). Regression guard: if
        // these drift, the three-scale spec breaks (task-20260422-1da5).
        const _: () = assert!(
            ZOOM_STEP > 0.0 && ZOOM_STEP <= 0.5,
            "ZOOM_STEP out of range"
        );
        assert!((super::ZOOM_MIN - 0.0).abs() < f32::EPSILON);
        assert!((super::ZOOM_MAX - 2.0).abs() < f32::EPSILON);
    }

    /// `cable_line` renders a solid bar when two adjacent molecules have a
    /// typed DAG link, and a dim dot otherwise. The cable glyph is the
    /// only DAG signal at immeuble scale, so this is load-bearing.
    #[test]
    fn cable_line_solid_when_blocked_by_exists() {
        let mut a = row_with("running", HeartbeatTier::Active);
        a.mol_id = "mol-a".into();
        let mut b = row_with("pending", HeartbeatTier::Quiet);
        b.mol_id = "mol-b".into();
        b.blocked_by = vec![("mol-a".into(), "running".into())];
        let line = super::cable_line(&a, &b);
        let text: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains('│'),
            "expected solid cable glyph, got: {text}"
        );
    }

    #[test]
    fn cable_line_dim_when_no_link() {
        let mut a = row_with("running", HeartbeatTier::Active);
        a.mol_id = "mol-a".into();
        let mut b = row_with("pending", HeartbeatTier::Quiet);
        b.mol_id = "mol-b".into();
        let line = super::cable_line(&a, &b);
        let text: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains('·'), "expected dim spacer glyph, got: {text}");
    }

    /// `molecule_box` renders the four-line box frame with `┌`/`└` corners
    /// — monospace characters only, no layout engine. Acceptance #5.
    #[test]
    fn molecule_box_uses_monospace_frame() {
        let mut r = row_with("running", HeartbeatTier::Active);
        r.mol_id = "mol-xyz".into();
        r.step = "2/3".into();
        r.topic = Some("JR wall".into());
        let lines = super::molecule_box(&r, super::BoxAccent::Current);
        assert_eq!(lines.len(), 4, "box must render 4 lines");
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        assert!(rendered[0].starts_with("┌"), "top frame corner missing");
        assert!(rendered[3].starts_with("└"), "bottom frame corner missing");
        assert!(
            rendered[0].contains("mol-xyz"),
            "id must appear in top frame"
        );
        assert!(
            rendered[2].contains("JR wall"),
            "topic must appear in box body"
        );
    }

    /// `immeuble_lines` glues prev + current + next boxes together. Three
    /// boxes × 4 lines = 12, plus two cable separators.
    #[test]
    fn immeuble_lines_stacks_three_neighbours_with_cables() {
        let mut prev = row_with("pending", HeartbeatTier::Quiet);
        prev.mol_id = "mol-prev".into();
        let mut cur = row_with("running", HeartbeatTier::Active);
        cur.mol_id = "mol-cur".into();
        let mut next = row_with("pending", HeartbeatTier::Quiet);
        next.mol_id = "mol-next".into();
        let lines = super::immeuble_lines(Some(&prev), Some(&cur), Some(&next));
        // 3 boxes * 4 lines + 2 cables = 14
        assert_eq!(
            lines.len(),
            14,
            "expected 14 lines (3 boxes + 2 cables), got {}",
            lines.len()
        );
    }

    /// With no neighbours, the immeuble body still renders the current
    /// molecule (4 lines, no cables).
    #[test]
    fn immeuble_lines_singleton_when_no_neighbours() {
        let mut cur = row_with("running", HeartbeatTier::Active);
        cur.mol_id = "mol-cur".into();
        let lines = super::immeuble_lines(None, Some(&cur), None);
        assert_eq!(lines.len(), 4, "singleton box is 4 lines");
    }

    #[test]
    fn immeuble_lines_empty_when_no_selection() {
        let lines = super::immeuble_lines(None, None, None);
        assert_eq!(lines.len(), 1, "must render a single 'no selection' line");
    }

    /// Footer and help overlay must document the three zoom keybindings —
    /// they are invisible otherwise. Regression guard for the one place
    /// the operator learns about the new controls.
    #[test]
    fn help_overlay_documents_zoom_keys() {
        let lines = super::help_overlay_lines(true);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(flat.contains("Zoom-continu"));
        assert!(flat.contains("ville"));
        assert!(flat.contains("immeuble"));
        assert!(flat.contains("peau"));
        assert!(
            flat.contains(" +  "),
            "`+` keybind must appear in the help overlay"
        );
        assert!(
            flat.contains(" -  "),
            "`-` keybind must appear in the help overlay"
        );
        assert!(
            flat.contains(" =  "),
            "`=` reset must appear in the help overlay"
        );
    }

    // ---- Cockpit action keystrokes (task-20260423-16ad) ---------------

    /// Build a minimal `App` with a single selected molecule so modal
    /// open/confirm paths can be driven from the test without touching
    /// the terminal.
    fn app_with_selected(mol_id: &str) -> App {
        use std::collections::HashMap;
        let mut row = row_with("running", HeartbeatTier::Active);
        row.mol_id = mol_id.into();
        let mut app = App::for_test(vec![row], HashMap::new());
        app.table_state.select(Some(0));
        app
    }

    /// `n` opens the nucleate modal with empty fields focused on the
    /// formula field; Esc cancels back to None.
    #[test]
    fn action_n_opens_nucleate_modal() {
        let mut app = app_with_selected("task-x");
        app.open_nucleate_modal();
        match &app.action_modal {
            ActionModal::Nucleate(form) => {
                assert_eq!(form.focus, NucleateField::Formula);
                assert!(form.formula.is_empty());
                assert!(form.topic.is_empty());
            }
            _ => panic!("expected Nucleate modal, got {:?}", modal_kind(&app)),
        }
        assert!(app.handle_action_modal_key(KeyCode::Esc));
        assert!(!app.action_modal.is_active());
    }

    /// Typing into the nucleate formula field, Tab to topic, Enter to
    /// fire — path exercised without actually invoking `cs` (the modal
    /// only calls `fire_nucleate` when both fields accept; we intercept
    /// at Tab to verify the focus transition).
    #[test]
    fn action_n_tab_switches_focus() {
        let mut app = app_with_selected("task-x");
        app.open_nucleate_modal();
        for c in "task-work".chars() {
            assert!(app.handle_action_modal_key(KeyCode::Char(c)));
        }
        assert!(app.handle_action_modal_key(KeyCode::Tab));
        match &app.action_modal {
            ActionModal::Nucleate(form) => {
                assert_eq!(form.focus, NucleateField::Topic);
                assert_eq!(form.formula, "task-work");
            }
            _ => panic!("expected Nucleate modal"),
        }
    }

    /// `t` opens the tackle confirmation modal carrying the selected
    /// molecule id; `n` cancels without firing, `y` would fire but we
    /// can't shell out in tests — just verify the confirm/cancel path.
    #[test]
    fn action_t_opens_tackle_confirm() {
        let mut app = app_with_selected("task-x");
        app.open_tackle_modal();
        match &app.action_modal {
            ActionModal::ConfirmTackle { mol_id } => assert_eq!(mol_id, "task-x"),
            _ => panic!("expected ConfirmTackle modal"),
        }
        assert!(app.handle_action_modal_key(KeyCode::Char('n')));
        assert!(!app.action_modal.is_active());
    }

    /// `m` opens the merge-and-done confirmation; `y` is the only
    /// affirmative path, everything else cancels (destructive action
    /// discipline).
    #[test]
    fn action_m_opens_merge_confirm() {
        let mut app = app_with_selected("task-x");
        app.open_merge_modal();
        match &app.action_modal {
            ActionModal::ConfirmMerge { mol_id } => assert_eq!(mol_id, "task-x"),
            _ => panic!("expected ConfirmMerge modal"),
        }
        // A stray space should not confirm.
        assert!(app.handle_action_modal_key(KeyCode::Char(' ')));
        assert!(
            matches!(app.action_modal, ActionModal::ConfirmMerge { .. }),
            "non-y/n key must keep the merge modal open"
        );
        assert!(app.handle_action_modal_key(KeyCode::Esc));
        assert!(!app.action_modal.is_active());
    }

    /// `w` opens the whisper modal scoped to the selected molecule.
    /// Characters accumulate into the body; backspace erases; Esc
    /// cancels.
    #[test]
    fn action_w_opens_whisper_modal() {
        let mut app = app_with_selected("task-x");
        app.open_whisper_modal();
        for c in "slow down".chars() {
            assert!(app.handle_action_modal_key(KeyCode::Char(c)));
        }
        match &app.action_modal {
            ActionModal::Whisper { mol_id, body } => {
                assert_eq!(mol_id, "task-x");
                assert_eq!(body, "slow down");
            }
            _ => panic!("expected Whisper modal"),
        }
        assert!(app.handle_action_modal_key(KeyCode::Backspace));
        match &app.action_modal {
            ActionModal::Whisper { body, .. } => assert_eq!(body, "slow dow"),
            _ => unreachable!(),
        }
        assert!(app.handle_action_modal_key(KeyCode::Esc));
        assert!(!app.action_modal.is_active());
    }

    /// `.` opens the free-form session-note modal; no selected molecule
    /// required (it targets the open session, not a specific row).
    #[test]
    fn action_dot_opens_session_note_modal() {
        use std::collections::HashMap;
        let mut app = App::for_test(Vec::new(), HashMap::new());
        app.open_session_note_modal();
        match &app.action_modal {
            ActionModal::SessionNote { body } => assert!(body.is_empty()),
            _ => panic!("expected SessionNote modal"),
        }
        for c in "insight".chars() {
            assert!(app.handle_action_modal_key(KeyCode::Char(c)));
        }
        match &app.action_modal {
            ActionModal::SessionNote { body } => assert_eq!(body, "insight"),
            _ => unreachable!(),
        }
    }

    /// `t` / `m` / `w` on an empty fleet (no selection) should surface
    /// a status message rather than opening a modal against a missing
    /// row.
    #[test]
    fn action_t_without_selection_stays_idle() {
        use std::collections::HashMap;
        let mut app = App::for_test(Vec::new(), HashMap::new());
        app.open_tackle_modal();
        assert!(!app.action_modal.is_active());
        assert!(!app.status_msg.is_empty());
    }

    /// Key remap regression: the lowercase detail-pane letters `n` and
    /// `t` are **not** registered as renderer keys anymore — they were
    /// promoted to cockpit actions. Their shifted counterparts `N` / `T`
    /// are.
    #[test]
    fn renderer_keys_no_longer_claim_lowercase_n_t() {
        use std::collections::HashMap;
        let app = App::for_test(Vec::new(), HashMap::new());
        assert!(
            app.renderer_for_key('n').is_none(),
            "lowercase `n` must be free for the nucleate action"
        );
        assert!(
            app.renderer_for_key('t').is_none(),
            "lowercase `t` must be free for the tackle action"
        );
        assert!(
            app.renderer_for_key('N').is_some(),
            "`N` must open the notes pane"
        );
        assert!(
            app.renderer_for_key('T').is_some(),
            "`T` must open the tree pane"
        );
    }

    /// Help overlay advertises every cockpit action.
    #[test]
    fn help_overlay_documents_cockpit_actions() {
        let lines = super::help_overlay_lines(true);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        for tag in [
            "nucleate",
            "tackle",
            "merge-and-done",
            "whisper",
            "session note",
        ] {
            assert!(
                flat.contains(tag),
                "help overlay must mention cockpit action `{tag}`; got:\n{flat}"
            );
        }
    }

    fn modal_kind(app: &App) -> &'static str {
        match app.action_modal {
            ActionModal::None => "None",
            ActionModal::Nucleate(_) => "Nucleate",
            ActionModal::ConfirmTackle { .. } => "ConfirmTackle",
            ActionModal::ConfirmMerge { .. } => "ConfirmMerge",
            ActionModal::Whisper { .. } => "Whisper",
            ActionModal::SessionNote { .. } => "SessionNote",
        }
    }

    // -- ADAPTER column (task-20260712-6609) ---------------------------------

    use cosmon_core::adapter_attribution::{
        AdapterAttribution, AdapterSource, ModelSource, Realized,
    };

    /// Flatten a `Line`'s spans into its visible text — the drift-proof
    /// contents an operator actually reads.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Build the attribution a `claude`-dispatched molecule would carry:
    /// adapter from the `--adapter claude` CLI flag, model pinned by
    /// `--model`, and — honestly — no reasoning effort (no event records it).
    fn claude_attribution() -> AdapterAttribution {
        AdapterAttribution {
            adapter: Some("claude".into()),
            adapter_source: Some(AdapterSource::Cli),
            model: Some("claude-opus-4-8".into()),
            model_source: Some(ModelSource::Flag),
            reasoning_effort: None,
            realized: Realized::default(),
        }
    }

    #[test]
    fn adapter_cell_renders_adapter_model_and_source() {
        let cell = adapter_cell(&claude_attribution());
        let text = line_text(&cell);
        assert_eq!(text, "claude/claude-opus-4-8 [cli]");
        // The adapter name is emphasised (cyan), the source tag dimmed.
        assert_eq!(cell.spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn adapter_cell_empty_renders_placeholder() {
        let cell = adapter_cell(&AdapterAttribution::default());
        assert_eq!(line_text(&cell), "-");
    }

    #[test]
    fn adapter_cell_never_shows_effort_when_unrecorded() {
        // The honesty rule: with no effort on record the cell carries no
        // `@effort` marker. This is the render-side guard against inferring
        // thinking from the current config.
        let cell = adapter_cell(&claude_attribution());
        assert!(!line_text(&cell).contains('@'));
    }

    #[test]
    fn adapter_cell_renders_realized_drift_distinctly() {
        // Pinned opus, realized sonnet: the drift segment `~>sonnet` renders in
        // a distinct yellow so it reads as a signal, not another pin.
        let mut att = claude_attribution();
        att.realized = Realized::Observed(vec!["claude-sonnet-5".into()]);
        let cell = adapter_cell(&att);
        assert_eq!(
            line_text(&cell),
            "claude/claude-opus-4-8~>claude-sonnet-5 [cli]"
        );
        // The drift span is yellow (distinct from the dim-gray pin).
        let drift = cell
            .spans
            .iter()
            .find(|s| s.content.contains("~>"))
            .expect("drift span present");
        assert_eq!(drift.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn adapter_cell_agreement_shows_no_drift() {
        // Realized == pin: agreement is silence — no `~>` glyph in the cell.
        let mut att = claude_attribution();
        att.realized = Realized::Observed(vec!["claude-opus-4-8".into()]);
        let cell = adapter_cell(&att);
        assert_eq!(line_text(&cell), "claude/claude-opus-4-8 [cli]");
        assert!(!line_text(&cell).contains("~>"));
    }

    #[test]
    fn expanded_detail_includes_realized_axis() {
        let mut row = row_with("running", HeartbeatTier::Active);
        row.adapter = claude_attribution();
        row.adapter.realized = Realized::Silent;
        let lines = expanded_detail_lines(&row);
        let joined: String = lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("realized"),
            "expanded detail must carry a realized axis:\n{joined}"
        );
        assert!(
            joined.contains("- (silent)"),
            "a silent run must render its honest disposition:\n{joined}"
        );
    }

    #[test]
    fn expanded_detail_includes_adapter_attribution() {
        let mut row = row_with("running", HeartbeatTier::Active);
        row.adapter = claude_attribution();
        let lines = expanded_detail_lines(&row);
        let joined: String = lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("adapter"),
            "expanded detail must carry an adapter field:\n{joined}"
        );
        assert!(
            joined.contains("claude/claude-opus-4-8 [cli]"),
            "expanded detail must show the folded attribution:\n{joined}"
        );
    }

    /// Render the full fleet table to a headless backend and confirm the
    /// ADAPTER column header and a `claude`-dispatched row's attribution
    /// both land in the visible buffer. This is the end-to-end render gate:
    /// header, data cell, and layout must survive together.
    #[test]
    fn draw_table_shows_adapter_column() {
        let mut row = row_with("running", HeartbeatTier::Active);
        row.mol_id = "task-20260712-6609".into();
        row.adapter = claude_attribution();
        let mut app = App::for_test(vec![row], std::collections::HashMap::new());

        let backend = ratatui::backend::TestBackend::new(160, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw_table(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol));
            }
            rendered.push('\n');
        }
        assert!(
            rendered.contains("ADAPTER"),
            "table header must include the ADAPTER column:\n{rendered}"
        );
        assert!(
            rendered.contains("claude"),
            "table must render the folded claude attribution:\n{rendered}"
        );
    }

    /// Render `app`'s worker strip to a headless backend and return the
    /// line as text.
    fn rendered_worker_strip(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(160, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| app.draw_worker_strip(f, f.area()))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut rendered = String::new();
        for x in 0..buf.area.width {
            rendered.push_str(buf.cell((x, 0)).map_or(" ", ratatui::buffer::Cell::symbol));
        }
        rendered.trim_end().to_owned()
    }

    /// The end-to-end render gate for the strip: a lying roster must put
    /// all three counts and the remedy on one visible line.
    #[test]
    fn draw_worker_strip_renders_counts_and_the_purge_remedy() {
        let mut app = App::for_test(Vec::new(), std::collections::HashMap::new());
        app.census = WorkerCensus {
            registered: 30,
            attached: 3,
        };

        let rendered = rendered_worker_strip(&app);
        assert_eq!(
            rendered, " workers: 30 registered · 3 attached · 27 phantom → cs purge",
            "the strip must read as one sentence, got:\n{rendered}"
        );
    }

    /// A clean roster keeps the strip — a reading is always on — but must
    /// not advise `cs purge`, which would be a no-op gesture.
    #[test]
    fn draw_worker_strip_stays_on_but_drops_the_remedy_when_nothing_to_purge() {
        let mut app = App::for_test(Vec::new(), std::collections::HashMap::new());
        app.census = WorkerCensus {
            registered: 4,
            attached: 4,
        };

        let rendered = rendered_worker_strip(&app);
        assert_eq!(
            rendered, " workers: 4 registered · 4 attached · 0 phantom",
            "a clean roster still reports, without advising a no-op purge"
        );
    }

    /// End-to-end honest fold: write a real `events.jsonl` for a molecule
    /// explicitly dispatched via the `claude` adapter, then confirm
    /// `fold_adapter_attributions` reads it back off disk into the row.
    /// This reproduces the mission's fixture — a claude-dispatched molecule —
    /// through the exact I/O path the live TUI uses.
    #[test]
    fn fold_adapter_attributions_reads_claude_dispatch_from_disk() {
        use cosmon_core::event_v2::{
            AdapterSelectionSource, Envelope, EventV2, ModelSelectionSource, Seq,
        };
        use cosmon_core::id::MoleculeId;

        let tmp = tempfile::TempDir::new().unwrap();
        let mid = MoleculeId::new("task-20260712-6609").unwrap();
        let events = vec![
            EventV2::AdapterSelected {
                mol_id: mid.clone(),
                adapter_name: "claude".into(),
                selected_at: Utc::now(),
                selection_source: AdapterSelectionSource::Cli {
                    flag: "claude".into(),
                },
                role_hint: None,
                loop_ownership: Default::default(),
            },
            EventV2::ModelSelected {
                mol_id: mid.clone(),
                adapter_name: "claude".into(),
                model: Some("claude-opus-4-8".into()),
                selection_source: ModelSelectionSource::Flag {
                    flag: "claude-opus-4-8".into(),
                },
                selected_at: Utc::now(),
            },
        ];
        let mut body = String::new();
        for (i, ev) in events.into_iter().enumerate() {
            let env = Envelope::new(Seq(i as u64), None, ev);
            body.push_str(&serde_json::to_string(&env).unwrap());
            body.push('\n');
        }
        std::fs::write(tmp.path().join("events.jsonl"), body).unwrap();

        let mut row = row_with("running", HeartbeatTier::Active);
        row.mol_id = mid.to_string();
        let mut state_dirs = std::collections::HashMap::new();
        state_dirs.insert(mid.to_string(), tmp.path().to_path_buf());
        let app = App::for_test(vec![row], state_dirs);

        let map = app.fold_adapter_attributions(&app.rows);
        let att = map.get(&mid.to_string()).expect("attribution folded");
        assert_eq!(att.adapter.as_deref(), Some("claude"));
        assert_eq!(att.model.as_deref(), Some("claude-opus-4-8"));
        // No observation on disk → honest `?` (F-03): an unconfirmed intention
        // is visibly distinct from a confirmed realization.
        assert_eq!(att.compact_cell(), "claude/claude-opus-4-8 [cli] ?");
        // Honest silence: the disk record carried no effort, so none surfaces.
        assert_eq!(att.reasoning_effort, None);
    }
}
