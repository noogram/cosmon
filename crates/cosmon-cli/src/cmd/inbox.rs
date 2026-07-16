// SPDX-License-Identifier: AGPL-3.0-only

//! `cs inbox` — pile verticale d'actions atomiques (Jobs' cockpit-as-mailbox).
//!
//! One panel, one stack. Every line = one molecule demanding an operator
//! decision *right now*. Decision taken, line disappears, next line rises.
//! No graph viewer, no chat, no multi-pane layout, no editor, no dashboard,
//! no search bar — those are the five deliberate non-features plus the
//! "piège existentiel" of the search bar.
//!
//! # Buckets (top to bottom)
//!
//! 1. **COMPLETED** — `status == Completed` and not yet merged; awaits
//!    `cs done`. Most urgent: the worker has finished, the branch is
//!    on the shelf, the merge window is open.
//! 2. **STUCK** — `status ∈ {Frozen, Collapsed}` or molecules the worker
//!    flagged as needing operator input. A question from a worker.
//! 3. **HOT** — `status == Pending` with `temp:hot` tag (actionable
//!    backlog the operator has already promoted).
//! 4. **SIGNAL** — `kind == Signal` not yet terminal; patrol-surfaced
//!    observations requiring a decision (see ADR-013 §Signal).
//!
//! Within each bucket, sort by age DESC (oldest first — the most overdue
//! rises to the top). A sticky top line surfaces any currently-open
//! session.
//!
//! # Keybindings
//!
//! - `j` / `k` or `↓` / `↑` — move selection
//! - `Enter` / `b` — toggle briefing+synthesis detail overlay
//! - `d` — `cs done <id>` (leaves TUI, runs, returns)
//! - `t` — `cs tackle <id>` (leaves TUI, runs, returns)
//! - `w` — whisper to the worker (prompts for text inline)
//! - `c` — collapse (prompts for reason inline)
//! - `r` / `R` — reload the stack
//! - `q` / `Esc` — quit
//!
//! # Success metric
//!
//! Jobs' binary: **5 days out of 7 without opening Claude Code to pilot**.
//! Niel's companion: **token ratio pilote/worker below 10:1**. The
//! 7-day trial protocol is documented in [`docs/guides/inbox-trial.md`].

#![allow(clippy::too_many_lines)]

use std::io::{self, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};

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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use super::peek_tui::renderers::{all as all_renderers, DetailCtx, DetailRenderer};
use super::peek_tui::RowView;
use super::Context;

/// Arguments for the `inbox` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Refresh cadence in milliseconds. Inbox reloads the stack on each
    /// tick or on `r`/`R`. Default: 2000ms (gentler than peek's 250ms —
    /// inbox is a decision surface, not a watchdog).
    #[arg(long, default_value_t = 2000)]
    pub refresh_ms: u64,

    /// Print the inbox contents as NDJSON and exit (agent-first). Skips
    /// the TUI entirely. One JSON object per actionable row plus one
    /// session envelope when an unsealed session exists.
    #[arg(long)]
    pub json: bool,
}

/// Entry point for `cs inbox`.
///
/// # Errors
/// Propagates project-identity, TTY, and filestore errors.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    super::require_project_identity(ctx)?;
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);

    if args.json || ctx.json {
        return run_json(&state_dir);
    }

    let mut app = App::new(ctx, state_dir, args.refresh_ms)?;
    let mut terminal = setup_terminal()?;

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        eprintln!("cs inbox panicked: {info}");
    }));

    let res = app.event_loop(&mut terminal);
    std::panic::set_hook(prev_hook);
    restore_terminal(&mut terminal)?;
    res
}

/// JSON / NDJSON rendering — skips the TUI entirely and streams one line
/// per actionable row plus a leading `session` envelope when one is open.
fn run_json(state_dir: &Path) -> anyhow::Result<()> {
    let store = FileStore::new(state_dir);
    let snap = Snapshot::build(&store, state_dir)?;
    let mut stdout = io::stdout().lock();
    if let Some(s) = &snap.session {
        let obj = serde_json::json!({
            "kind": "session",
            "session_id": s.session_id,
            "path": s.path.to_string_lossy(),
            "started_at": s.started_at.to_rfc3339(),
            "note_count": s.note_count,
        });
        writeln!(stdout, "{obj}").ok();
    }
    for row in &snap.rows {
        let obj = serde_json::json!({
            "kind": "row",
            "bucket": row.bucket.as_str(),
            "mol_id": row.mol_id,
            "molecule_kind": row.mol_kind.map(|k| format!("{k:?}").to_lowercase()),
            "status": format!("{:?}", row.status).to_lowercase(),
            "topic": row.topic,
            "age_seconds": row.age_seconds,
            "tags": row.tags,
        });
        writeln!(stdout, "{obj}").ok();
    }
    if snap.session.is_none() && snap.rows.is_empty() {
        writeln!(stdout, "{}", serde_json::json!({"kind":"empty"})).ok();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Terminal setup — mirrors `cs peek` so panic behaviour and alt-screen are
// handled identically. Deliberately duplicated rather than extracted into a
// shared helper: inbox and peek are independent surfaces and a shared
// helper would be a one-site abstraction for two callers (feynman: don't).
// ---------------------------------------------------------------------------

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    if !io::stdout().is_terminal() {
        return Err(anyhow::anyhow!(
            "cs inbox requires a TTY on stdout; got a pipe or redirected stream. \
             Run `cs inbox --json` for a non-interactive view."
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

// ---------------------------------------------------------------------------
// Bucket classification — the heart of inbox. Pure function over
// `MoleculeData` so it is trivially testable and does not require a
// FleetSnapshot. Each bucket carries its glyph, its color, and its
// ranking weight (lower = higher in the stack).
// ---------------------------------------------------------------------------

/// The four buckets of actionable molecules. Ordering in the enum matches
/// stack ordering (top to bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    /// Completed but not yet merged — awaits `cs done`.
    Completed,
    /// Frozen or the operator explicitly marked it as stuck — a question
    /// from the worker.
    Stuck,
    /// `temp:hot` pending — actionable backlog the operator has promoted.
    Hot,
    /// Signal molecules (⚡) that have not yet been resolved.
    Signal,
}

impl Bucket {
    /// Glyph for the pile line — one char + variant-selector, rendered
    /// left-aligned as the "priority" column.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Completed => "✓",
            Self::Stuck => "❓",
            Self::Hot => "🔥",
            Self::Signal => "⚡",
        }
    }

    /// Color paired with the glyph — green for completed (the finish line),
    /// yellow for stuck, red for hot, cyan for signal.
    #[must_use]
    pub fn color(self) -> Color {
        match self {
            Self::Completed => Color::Green,
            Self::Stuck => Color::Yellow,
            Self::Hot => Color::Red,
            Self::Signal => Color::Cyan,
        }
    }

    /// Serializable short string for the JSON mode.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Stuck => "stuck",
            Self::Hot => "hot",
            Self::Signal => "signal",
        }
    }

    /// Hint shown next to the pending bracket marker.
    #[must_use]
    pub fn state_badge(self) -> &'static str {
        match self {
            Self::Completed => "DONE?",
            Self::Stuck => "ASK",
            Self::Hot => "TACKLE?",
            Self::Signal => "ACK?",
        }
    }
}

/// Classify a molecule into its inbox bucket, or `None` when it is not
/// actionable (terminal-merged, running, queued with no tag, …).
#[must_use]
pub fn classify(m: &MoleculeData) -> Option<Bucket> {
    let tags: Vec<&str> = m.tags.iter().map(cosmon_core::tag::Tag::as_str).collect();

    // Bucket 1 — completed but not merged. `merged_at` distinguishes
    // "done + merged + cleaned" from "done but branch still sitting".
    if m.status == MoleculeStatus::Completed && m.merged_at.is_none() {
        return Some(Bucket::Completed);
    }

    // Bucket 2 — frozen or stuck (operator input required).
    // `Collapsed` molecules are terminal *failures*, not questions — the
    // operator has already recorded the collapse reason, so they do not
    // belong in the pile of decisions-to-make. Only frozen (thaw or
    // continue?) surfaces here.
    if m.status == MoleculeStatus::Frozen {
        return Some(Bucket::Stuck);
    }

    // Bucket 3 — pending + temp:hot.
    if m.status == MoleculeStatus::Pending && tags.contains(&"temp:hot") {
        return Some(Bucket::Hot);
    }

    // Bucket 4 — fresh signals. Signals are ephemeral and often auto-
    // resolve, so we include them only while still Pending / Running.
    if matches!(m.kind, Some(MoleculeKind::Signal))
        && !matches!(
            m.status,
            MoleculeStatus::Completed | MoleculeStatus::Collapsed
        )
    {
        return Some(Bucket::Signal);
    }

    None
}

/// One pile row — one line = one molecule = one atomic action. Kept as a
/// plain data struct (no borrows) so the renderer can mutate the stack
/// and keep the selection pointed at the same row after a reload.
#[derive(Debug, Clone)]
pub struct InboxRow {
    /// Bucket classification (Completed / Stuck / Hot / Signal).
    pub bucket: Bucket,
    /// Molecule ID as a display string (never empty — we skip rows that
    /// fail to classify upstream).
    pub mol_id: String,
    /// Optional molecule kind glyph source.
    pub mol_kind: Option<MoleculeKind>,
    /// Molecule status at classification time — preserved so the JSON
    /// mode can emit the authoritative truth.
    pub status: MoleculeStatus,
    /// Short topic string, pulled from `variables["topic"]` when set.
    pub topic: String,
    /// Tags attached to the molecule (stringified).
    pub tags: Vec<String>,
    /// Age at classification time in seconds. Used for sort stability and
    /// the "N[s|m|h|d]" age column.
    pub age_seconds: i64,
    /// When the molecule was created. Preserved so the renderer can
    /// re-compute the age string across refresh ticks without reloading
    /// the whole molecule from disk.
    pub created_at: DateTime<Utc>,
    /// Whether the molecule has an attached worker — used to decide
    /// whether `w` (whisper) is valid.
    pub has_worker: bool,
}

/// Open-session envelope surfaced as the sticky top line.
#[derive(Debug, Clone)]
pub struct SessionSticky {
    /// Session id (stem of `session-<ts>.md`).
    pub session_id: String,
    /// Absolute path to the session file.
    pub path: PathBuf,
    /// When the session opened.
    pub started_at: DateTime<Utc>,
    /// Number of `## ` note blocks already written.
    pub note_count: usize,
}

/// A full-world snapshot for one inbox render: the open session (if any)
/// plus the actionable rows, already sorted.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Open session line, shown stickily above the pile.
    pub session: Option<SessionSticky>,
    /// Atomic-action rows.
    pub rows: Vec<InboxRow>,
}

impl Snapshot {
    /// Build a snapshot from the filestore rooted at `state_dir`.
    ///
    /// # Errors
    /// Returns any filestore list failure.
    pub fn build(store: &FileStore, state_dir: &Path) -> anyhow::Result<Self> {
        let now = Utc::now();
        let molecules = store.list_molecules(&MoleculeFilter::default())?;
        let mut rows: Vec<InboxRow> = molecules
            .into_iter()
            .filter_map(|m| {
                let bucket = classify(&m)?;
                let topic = m
                    .display_topic()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let age_seconds = now.signed_duration_since(m.created_at).num_seconds().max(0);
                let tags = m.tags.iter().map(ToString::to_string).collect::<Vec<_>>();
                Some(InboxRow {
                    bucket,
                    mol_id: m.id.to_string(),
                    mol_kind: m.kind,
                    status: m.status,
                    topic,
                    tags,
                    age_seconds,
                    created_at: m.created_at,
                    has_worker: m.assigned_worker.is_some(),
                })
            })
            .collect();

        // Sort: bucket ASC (Completed first), then age DESC (oldest first),
        // then id ASC for deterministic output. Oldest rising to the top
        // matches the "most overdue first" intuition of an inbox.
        rows.sort_by(|a, b| {
            a.bucket
                .cmp(&b.bucket)
                .then_with(|| b.age_seconds.cmp(&a.age_seconds))
                .then_with(|| a.mol_id.cmp(&b.mol_id))
        });

        let session = find_open_session(state_dir);

        Ok(Self { session, rows })
    }
}

/// Scan `<state_dir>/sessions/` for the single unsealed session, if any.
/// Silently drops read/parse errors — inbox is read-only on this path and
/// `cs session` is the authoritative tool for sealing.
fn find_open_session(state_dir: &Path) -> Option<SessionSticky> {
    let dir = state_dir.join("sessions");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("session-") || !path_is_md(&path) {
            continue;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        let marker_count = content.lines().filter(|l| *l == "---").count();
        // Sealed files have ≥4 `---` markers (open + close frontmatter,
        // open + close footer). Unsealed have 2.
        if marker_count >= 4 {
            continue;
        }
        candidates.push((path, content));
    }
    if candidates.len() != 1 {
        return None;
    }
    let (path, content) = candidates.into_iter().next()?;
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session-unknown")
        .to_owned();
    let started_at = extract_started_at(&content).unwrap_or_else(Utc::now);
    let note_count = content.lines().filter(|l| l.starts_with("## ")).count();
    Some(SessionSticky {
        session_id,
        path,
        started_at,
        note_count,
    })
}

fn path_is_md(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

fn extract_started_at(content: &str) -> Option<DateTime<Utc>> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("started_at: ") {
            if let Ok(dt) = DateTime::parse_from_rfc3339(rest.trim()) {
                return Some(dt.with_timezone(&Utc));
            }
        }
        if line == "---" && !content.starts_with(line) {
            break;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// TUI application state and event loop.
// ---------------------------------------------------------------------------

struct App {
    state_dir: PathBuf,
    snapshot: Snapshot,
    list_state: ListState,
    refresh: Duration,
    last_refresh: Instant,
    status_msg: String,
    detail_open: bool,
    /// Index into `renderers` of the active pane within the detail
    /// overlay. Defaults to the briefing renderer.
    detail_active: usize,
    detail_scroll: u16,
    renderers: Vec<Box<dyn DetailRenderer>>,
    /// Whether we are in an inline prompt mode (for `c`/`w` payloads).
    prompt_mode: Option<PromptKind>,
    prompt_buffer: String,
    /// True when the last action requested a post-command reload before
    /// the next event tick.
    pending_reload: bool,
    /// Path to the `cs` binary used to shell out for actions — captured
    /// at startup so a subsequent `which` race cannot redirect to a
    /// different binary mid-session.
    cs_exe: PathBuf,
    /// Captured context flags so we can pass `--verbose`/`--json`/`--config`
    /// through on shell-outs.
    verbose: bool,
    config: Option<PathBuf>,
}

enum PromptKind {
    /// `c` — prompting for collapse reason.
    CollapseReason(String),
    /// `w` — prompting for whisper text.
    WhisperText(String),
}

impl App {
    fn new(ctx: &Context, state_dir: PathBuf, refresh_ms: u64) -> anyhow::Result<Self> {
        let store = FileStore::new(&state_dir);
        let snapshot = Snapshot::build(&store, &state_dir)?;
        let mut list_state = ListState::default();
        if !snapshot.rows.is_empty() {
            list_state.select(Some(0));
        }
        let cs_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cs"));
        // Locate the briefing renderer in the registry so Enter defaults
        // to showing briefing.md.
        let renderers = all_renderers();
        let default_idx = renderers
            .iter()
            .position(|r| r.keys().contains(&'b'))
            .unwrap_or(0);
        Ok(Self {
            state_dir,
            snapshot,
            list_state,
            refresh: Duration::from_millis(refresh_ms.max(500)),
            last_refresh: Instant::now(),
            status_msg: String::new(),
            detail_open: false,
            detail_active: default_idx,
            detail_scroll: 0,
            renderers,
            prompt_mode: None,
            prompt_buffer: String::new(),
            pending_reload: false,
            cs_exe,
            verbose: ctx.verbose,
            config: ctx.config.clone(),
        })
    }

    fn selected_row(&self) -> Option<&InboxRow> {
        self.list_state
            .selected()
            .and_then(|i| self.snapshot.rows.get(i))
    }

    fn reload(&mut self) {
        let store = FileStore::new(&self.state_dir);
        match Snapshot::build(&store, &self.state_dir) {
            Ok(new) => {
                let selected_id = self.selected_row().map(|r| r.mol_id.clone());
                self.snapshot = new;
                let new_pos = selected_id
                    .as_ref()
                    .and_then(|id| self.snapshot.rows.iter().position(|r| &r.mol_id == id));
                if let Some(pos) = new_pos {
                    self.list_state.select(Some(pos));
                } else if self.snapshot.rows.is_empty() {
                    self.list_state.select(None);
                } else {
                    self.list_state.select(Some(0));
                }
                self.last_refresh = Instant::now();
            }
            Err(e) => {
                let short: String = e.to_string().chars().take(80).collect();
                self.status_msg = format!("reload failed: {short}");
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.snapshot.rows.is_empty() {
            self.list_state.select(None);
            return;
        }
        let len = self.snapshot.rows.len();
        let cur = self.list_state.selected().unwrap_or(0);
        #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
        let next = {
            let len_i = len as isize;
            let cur_i = cur as isize;
            (cur_i + delta).rem_euclid(len_i) as usize
        };
        self.list_state.select(Some(next));
        self.detail_scroll = 0;
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
                    if self.prompt_mode.is_some() {
                        if self.handle_prompt_key(k.code, terminal)? {
                            return Ok(());
                        }
                        continue;
                    }
                    if self.detail_open {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
                                self.detail_open = false;
                            }
                            KeyCode::Char('j' | 'J')
                                if k.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                self.detail_scroll = self.detail_scroll.saturating_add(1);
                            }
                            KeyCode::Char('k' | 'K')
                                if k.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                self.detail_scroll = self.detail_scroll.saturating_add(1);
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                            }
                            KeyCode::PageDown => {
                                self.detail_scroll = self.detail_scroll.saturating_add(10);
                            }
                            KeyCode::PageUp => {
                                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                            }
                            KeyCode::Char('s') => {
                                if let Some(idx) =
                                    self.renderers.iter().position(|r| r.keys().contains(&'s'))
                                {
                                    self.detail_active = idx;
                                    self.detail_scroll = 0;
                                }
                            }
                            KeyCode::Char('b') | KeyCode::Tab => {
                                if let Some(idx) =
                                    self.renderers.iter().position(|r| r.keys().contains(&'b'))
                                {
                                    self.detail_active = idx;
                                    self.detail_scroll = 0;
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                        KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                        KeyCode::Enter => {
                            if self.selected_row().is_some() {
                                self.detail_open = true;
                                self.detail_scroll = 0;
                            }
                        }
                        KeyCode::Char('r' | 'R') => self.reload(),
                        KeyCode::Char('d') => self.action_done(terminal)?,
                        KeyCode::Char('t') => self.action_tackle(terminal)?,
                        KeyCode::Char('c') => {
                            self.prompt_mode =
                                Some(PromptKind::CollapseReason(self.current_id_or_empty()));
                            self.prompt_buffer.clear();
                        }
                        KeyCode::Char('w') => {
                            if let Some(row) = self.selected_row() {
                                if row.has_worker {
                                    self.prompt_mode =
                                        Some(PromptKind::WhisperText(row.mol_id.clone()));
                                    self.prompt_buffer.clear();
                                } else {
                                    self.status_msg = format!(
                                        "w: no live worker on {} — whisper needs a running molecule",
                                        row.mol_id
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            if self.pending_reload {
                self.pending_reload = false;
                self.reload();
            }
            if self.last_refresh.elapsed() >= self.refresh {
                self.reload();
            }
        }
    }

    fn current_id_or_empty(&self) -> String {
        self.selected_row()
            .map(|r| r.mol_id.clone())
            .unwrap_or_default()
    }

    /// Handle a keypress while in prompt mode. Returns `true` if the
    /// caller should break out of the event loop (never, for now).
    fn handle_prompt_key(
        &mut self,
        code: KeyCode,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<bool> {
        match code {
            KeyCode::Esc => {
                self.prompt_mode = None;
                self.prompt_buffer.clear();
            }
            KeyCode::Enter => {
                let payload = std::mem::take(&mut self.prompt_buffer);
                let Some(kind) = self.prompt_mode.take() else {
                    return Ok(false);
                };
                if payload.trim().is_empty() {
                    self.status_msg = "prompt empty — cancelled".into();
                    return Ok(false);
                }
                match kind {
                    PromptKind::CollapseReason(mol_id) if !mol_id.is_empty() => {
                        self.shell_out(
                            terminal,
                            &["collapse", &mol_id, "--reason", payload.trim()],
                            "collapse",
                        )?;
                    }
                    PromptKind::WhisperText(mol_id) if !mol_id.is_empty() => {
                        self.shell_out(
                            terminal,
                            &["whisper", &mol_id, "-m", payload.trim()],
                            "whisper",
                        )?;
                    }
                    _ => {
                        self.status_msg = "no molecule selected".into();
                    }
                }
            }
            KeyCode::Backspace => {
                self.prompt_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.prompt_buffer.push(c);
            }
            _ => {}
        }
        Ok(false)
    }

    fn action_done(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let mol = row.mol_id.clone();
        let bucket = row.bucket;
        if bucket != Bucket::Completed {
            self.status_msg = format!("d: only valid on ✓ completed rows (this row: {bucket:?})");
            return Ok(());
        }
        self.shell_out(terminal, &["done", &mol], "done")
    }

    fn action_tackle(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        let Some(row) = self.selected_row() else {
            return Ok(());
        };
        let mol = row.mol_id.clone();
        let bucket = row.bucket;
        if !matches!(bucket, Bucket::Hot | Bucket::Signal) {
            self.status_msg =
                format!("t: only valid on 🔥 hot or ⚡ signal rows (this row: {bucket:?})");
            return Ok(());
        }
        self.shell_out(terminal, &["tackle", &mol], "tackle")
    }

    /// Leave raw mode / alternate screen, shell out to `cs <args>` so the
    /// operator sees its output, then re-enter. Mirrors `peek_tui`'s
    /// `attach_selected`. All inbox-triggered side-effects go through
    /// this one path — no direct `StateStore` writes from the TUI.
    fn shell_out(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        tail: &[&str],
        label: &str,
    ) -> anyhow::Result<()> {
        restore_terminal(terminal)?;
        let mut cmd = Command::new(&self.cs_exe);
        if self.verbose {
            cmd.arg("--verbose");
        }
        if let Some(cfg) = &self.config {
            cmd.arg("--config").arg(cfg);
        }
        cmd.args(tail);
        let status = cmd.status();
        // Always pause briefly so the operator can read the command
        // output before the TUI re-enters and hides it.
        println!();
        println!("-- press Enter to return to cs inbox --");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);

        enable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.clear()?;
        match status {
            Ok(s) if s.success() => {
                self.status_msg = format!("{label} succeeded");
            }
            Ok(s) => {
                self.status_msg = format!("{label} exited with {s}");
            }
            Err(e) => {
                self.status_msg = format!("{label} failed: {e}");
            }
        }
        self.pending_reload = true;
        Ok(())
    }

    // -------- rendering ----------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(u16::from(self.snapshot.session.is_some())),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);
        if self.snapshot.session.is_some() {
            self.draw_session_sticky(f, chunks[1]);
        }
        self.draw_pile(f, chunks[2]);
        self.draw_prompt_or_status(f, chunks[3]);
        self.draw_footer(f, chunks[4]);

        if self.detail_open {
            self.draw_detail_overlay(f, f.area());
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let text = format!(
            " cs inbox — {} atomic action{} · r reload · ? help hidden in footer ",
            self.snapshot.rows.len(),
            if self.snapshot.rows.len() == 1 {
                ""
            } else {
                "s"
            },
        );
        let p = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().add_modifier(Modifier::BOLD),
        )));
        f.render_widget(p, area);
    }

    fn draw_session_sticky(&self, f: &mut Frame, area: Rect) {
        let Some(s) = &self.snapshot.session else {
            return;
        };
        let since = s.started_at.with_timezone(&chrono::Local).format("%H:%M");
        let text = format!(
            " 📓 session open since {} — {} note{} ({})",
            since,
            s.note_count,
            if s.note_count == 1 { "" } else { "s" },
            s.session_id
        );
        let p = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        f.render_widget(p, area);
    }

    fn draw_pile(&mut self, f: &mut Frame, area: Rect) {
        if self.snapshot.rows.is_empty() && self.snapshot.session.is_none() {
            let placeholder = Paragraph::new(Line::from(vec![
                Span::styled("  nothing to decide", Style::default().fg(Color::Green)),
                Span::raw(" — sessions: none open. "),
            ]))
            .block(Block::default().borders(Borders::NONE));
            f.render_widget(placeholder, area);
            return;
        }
        if self.snapshot.rows.is_empty() {
            let p = Paragraph::new(Line::from(Span::styled(
                "  nothing to decide — a session is open but no molecule awaits you.",
                Style::default().fg(Color::DarkGray),
            )));
            f.render_widget(p, area);
            return;
        }

        let items: Vec<ListItem> = self
            .snapshot
            .rows
            .iter()
            .map(|r| ListItem::new(row_line(r)))
            .collect();
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_prompt_or_status(&self, f: &mut Frame, area: Rect) {
        if let Some(kind) = &self.prompt_mode {
            let label = match kind {
                PromptKind::CollapseReason(_) => "collapse reason",
                PromptKind::WhisperText(_) => "whisper text",
            };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {label}> "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&self.prompt_buffer),
                Span::styled(
                    "   (Enter=send, Esc=cancel)",
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            f.render_widget(Paragraph::new(line), area);
            return;
        }
        if self.status_msg.is_empty() {
            return;
        }
        let p = Paragraph::new(Line::from(Span::styled(
            format!(" {}", self.status_msg),
            Style::default().fg(Color::Green),
        )));
        f.render_widget(p, area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let has_session = self.snapshot.session.is_some();
        let line = Line::from(vec![
            Span::raw(
                " j/k move · Enter open · d done · t tackle · w whisper · c collapse · r reload · q quit  ",
            ),
            Span::styled(
                format!(
                    "· session: {}",
                    if has_session { "open" } else { "none open" }
                ),
                Style::default().fg(if has_session {
                    Color::Magenta
                } else {
                    Color::DarkGray
                }),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_detail_overlay(&self, f: &mut Frame, area: Rect) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let popup = centered_rect(88, 85, area);
        f.render_widget(Clear, popup);

        let title = format!(
            " {} {} — press b briefing · s synthesis · Esc/q to close ",
            row.bucket.glyph(),
            row.mol_id
        );

        // Build a DetailCtx targeting the selected molecule's directory.
        let mol_dir = self.molecule_dir_for(&row.mol_id);
        let view = RowView {
            mol_id: row.mol_id.clone(),
            session_slug: None,
            project: String::new(),
            role: String::new(),
            status: format!("{:?}", row.status).to_lowercase(),
            step: String::new(),
            updated_at: None,
            energy_in: 0,
            energy_out: 0,
            cost_usd: 0.0,
            context_window: None,
            session: None,
            socket: String::new(),
            heartbeat: cosmon_observability::HeartbeatTier::Orphaned,
            last_activity: None,
            last_progress_at: None,
            topic: Some(row.topic.clone()),
            formula: String::new(),
            tier_badge: String::new(),
            kind: row
                .mol_kind
                .map(|k| format!("{k:?}").to_lowercase())
                .unwrap_or_default(),
            blocked_by: Vec::new(),
            worker_name: None,
            tags: row.tags.clone(),
            created_at_utc: Some(row.created_at),
            whisper_fresh: false,
            role_glyphs: String::new(),
            trust_score: None,
            energy_budget: None,
            adapter: cosmon_core::adapter_attribution::AdapterAttribution::default(),
        };
        let ctx = DetailCtx {
            row: &view,
            molecule_dir: mol_dir.as_deref(),
            state_dir: Some(self.state_dir.as_path()),
        };
        let content = self.renderers[self.detail_active].render(&ctx);

        let para = Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(para, popup);
    }

    fn molecule_dir_for(&self, mol_id: &str) -> Option<PathBuf> {
        let id = cosmon_core::id::MoleculeId::new(mol_id).ok()?;
        Some(FileStore::new(&self.state_dir).molecule_dir(&id))
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Format one pile row as a ratatui `Line`. Kept free of `App` state so the
/// line rendering is testable without a full TUI fixture.
fn row_line(r: &InboxRow) -> Line<'static> {
    let topic = r.topic.trim();
    let topic_display = if topic.is_empty() {
        "(no topic)".to_owned()
    } else {
        topic.to_owned()
    };
    let topic_trunc = truncate(&topic_display, 64);
    let mol_trunc = truncate(&r.mol_id, 28);

    let kind_glyph = r
        .mol_kind
        .map_or("·", cosmon_core::kind::MoleculeKind::emoji);

    Line::from(vec![
        Span::styled(
            format!("{}  ", r.bucket.glyph()),
            Style::default()
                .fg(r.bucket.color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{kind_glyph} ")),
        Span::styled(
            format!("{mol_trunc:<28} "),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(format!("{topic_trunc:<64} ")),
        Span::styled(
            format!("{:>6} ", age_str(r.age_seconds)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("[{}]", r.bucket.state_badge()),
            Style::default().fg(r.bucket.color()),
        ),
    ])
}

/// Compact age string. 0..60s→`Ns`, <60m→`Nm`, <24h→`Nh`, else `Nd`.
#[must_use]
pub fn age_str(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Center a rectangle within `r` at the given percentages. Shared shape
/// with `cs peek`'s help overlay — duplicated here so inbox stays
/// self-contained.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::tag::Tag;
    use tempfile::TempDir;

    use super::*;

    fn mk_mol(id: &str, status: MoleculeStatus, kind: Option<MoleculeKind>) -> MoleculeData {
        MoleculeData {
            fleet_id: FleetId::new("default").unwrap(),
            id: MoleculeId::new(id).unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now() - chrono::Duration::minutes(10),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn classify_completed_awaiting_done() {
        let m = mk_mol("task-20260101-aaaa", MoleculeStatus::Completed, None);
        assert_eq!(classify(&m), Some(Bucket::Completed));
    }

    #[test]
    fn classify_completed_and_merged_is_not_in_inbox() {
        let mut m = mk_mol("task-20260101-aaaa", MoleculeStatus::Completed, None);
        m.merged_at = Some(Utc::now());
        assert_eq!(classify(&m), None);
    }

    #[test]
    fn classify_frozen_is_stuck() {
        let m = mk_mol("task-20260101-aaaa", MoleculeStatus::Frozen, None);
        assert_eq!(classify(&m), Some(Bucket::Stuck));
    }

    #[test]
    fn classify_collapsed_is_not_in_inbox() {
        let m = mk_mol("task-20260101-aaaa", MoleculeStatus::Collapsed, None);
        assert_eq!(classify(&m), None);
    }

    #[test]
    fn classify_pending_with_temp_hot_is_hot() {
        let mut m = mk_mol("task-20260101-aaaa", MoleculeStatus::Pending, None);
        m.tags.insert(Tag::new("temp:hot").unwrap());
        assert_eq!(classify(&m), Some(Bucket::Hot));
    }

    #[test]
    fn classify_pending_without_temp_hot_is_not_in_inbox() {
        let m = mk_mol("task-20260101-aaaa", MoleculeStatus::Pending, None);
        assert_eq!(classify(&m), None);
    }

    #[test]
    fn classify_signal_running_is_signal() {
        let m = mk_mol(
            "signal-20260101-aaaa",
            MoleculeStatus::Running,
            Some(MoleculeKind::Signal),
        );
        assert_eq!(classify(&m), Some(Bucket::Signal));
    }

    #[test]
    fn classify_signal_completed_is_not_in_inbox() {
        let m = mk_mol(
            "signal-20260101-aaaa",
            MoleculeStatus::Completed,
            Some(MoleculeKind::Signal),
        );
        // Completed-not-merged already falls into Bucket::Completed because
        // the Completed gate fires first. Bucket::Completed is acceptable
        // here — signals auto-resolve fast and a completed unmerged one
        // belongs in the done-queue.
        assert_eq!(classify(&m), Some(Bucket::Completed));
    }

    #[test]
    fn bucket_ordering_matches_stack_order() {
        let mut buckets = [
            Bucket::Signal,
            Bucket::Hot,
            Bucket::Completed,
            Bucket::Stuck,
        ];
        buckets.sort();
        assert_eq!(
            buckets,
            [
                Bucket::Completed,
                Bucket::Stuck,
                Bucket::Hot,
                Bucket::Signal
            ]
        );
    }

    #[test]
    fn age_str_formats_each_scale() {
        assert_eq!(age_str(5), "5s");
        assert_eq!(age_str(125), "2m");
        assert_eq!(age_str(7300), "2h");
        assert_eq!(age_str(90_000), "1d");
        assert_eq!(age_str(-12), "0s");
    }

    #[test]
    fn snapshot_sorts_by_bucket_then_age_desc() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mut newer_completed = mk_mol(
            "task-20260101-aaaa",
            MoleculeStatus::Completed,
            Some(MoleculeKind::Task),
        );
        newer_completed.created_at = Utc::now() - chrono::Duration::minutes(5);

        let mut older_completed = mk_mol(
            "task-20260101-bbbb",
            MoleculeStatus::Completed,
            Some(MoleculeKind::Task),
        );
        older_completed.created_at = Utc::now() - chrono::Duration::hours(3);

        let mut hot = mk_mol(
            "task-20260101-cccc",
            MoleculeStatus::Pending,
            Some(MoleculeKind::Task),
        );
        hot.tags.insert(Tag::new("temp:hot").unwrap());

        store
            .save_molecule(&newer_completed.id, &newer_completed)
            .unwrap();
        store
            .save_molecule(&older_completed.id, &older_completed)
            .unwrap();
        store.save_molecule(&hot.id, &hot).unwrap();

        let snap = Snapshot::build(&store, tmp.path()).unwrap();
        assert_eq!(snap.rows.len(), 3);
        // Two Completed first (older → newer), then Hot.
        assert_eq!(snap.rows[0].bucket, Bucket::Completed);
        assert_eq!(snap.rows[0].mol_id, "task-20260101-bbbb");
        assert_eq!(snap.rows[1].bucket, Bucket::Completed);
        assert_eq!(snap.rows[1].mol_id, "task-20260101-aaaa");
        assert_eq!(snap.rows[2].bucket, Bucket::Hot);
    }

    #[test]
    fn snapshot_surfaces_open_session() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        // No molecules — we only care about the session sticky.
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let session_path = sessions_dir.join("session-20260422T140000Z.md");
        std::fs::write(
            &session_path,
            "---\nsession_id: session-20260422T140000Z\n\
             started_at: 2026-04-22T14:00:00Z\noperator: me\ngalaxy: \"\"\n\
             root_molecules: []\n---\n\n## 14:01:02 — \n\nhello\n\n",
        )
        .unwrap();
        let snap = Snapshot::build(&store, tmp.path()).unwrap();
        let s = snap.session.expect("session should be open");
        assert_eq!(s.session_id, "session-20260422T140000Z");
        assert_eq!(s.note_count, 1);
    }

    #[test]
    fn snapshot_ignores_sealed_session() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let session_path = sessions_dir.join("session-20260422T140000Z.md");
        std::fs::write(
            &session_path,
            "---\nsession_id: session-20260422T140000Z\nstarted_at: 2026-04-22T14:00:00Z\n---\n\n\
             ## 14:01:02 — \n\nhello\n\n---\nended_at: 2026-04-22T14:05:00Z\nnote_count: 1\nseal: blake3:abcd\n---\n",
        )
        .unwrap();
        let snap = Snapshot::build(&store, tmp.path()).unwrap();
        assert!(snap.session.is_none());
    }

    #[test]
    fn empty_snapshot_is_empty_and_has_no_session() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let snap = Snapshot::build(&store, tmp.path()).unwrap();
        assert!(snap.rows.is_empty());
        assert!(snap.session.is_none());
    }

    #[test]
    fn classify_pending_with_many_tags_picks_hot_when_hot_present() {
        let mut m = mk_mol("task-20260101-aaaa", MoleculeStatus::Pending, None);
        m.tags.insert(Tag::new("temp:hot").unwrap());
        m.tags.insert(Tag::new("scope:cs").unwrap());
        assert_eq!(classify(&m), Some(Bucket::Hot));
    }

    #[test]
    fn worker_presence_flows_through_snapshot() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut frozen = mk_mol("task-20260101-aaaa", MoleculeStatus::Frozen, None);
        frozen.assigned_worker = Some(WorkerId::new("w-1").unwrap());
        store.save_molecule(&frozen.id, &frozen).unwrap();

        let snap = Snapshot::build(&store, tmp.path()).unwrap();
        assert_eq!(snap.rows.len(), 1);
        assert!(snap.rows[0].has_worker);
    }
}
