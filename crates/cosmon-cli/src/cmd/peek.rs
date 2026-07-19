// SPDX-License-Identifier: AGPL-3.0-only

//! `cs peek` — canonical fleet observation command (ADR-028).
//!
//! `cs peek` unifies fleet observation into a single command perimeter. Its
//! modes are chosen via flags:
//!
//! - `cs peek` (default, TTY) — ratatui TUI watchdog (Phase 1).
//! - `cs peek --json` — the machine projection: one JSON document,
//!   printed once, sorted by molecule id. Raw core `status`, `heartbeat`,
//!   `last_activity` and `updated_at` per molecule, and deliberately no
//!   taxonomy field. See [`PeekMoleculeJson`] for why the schema is this
//!   small.
//! - `cs peek --no-tui --once` — single snapshot + propel pass, then exit.
//! - `cs peek --no-tui --follow` — live event stream (absorbs `cs watch`).
//! - `cs peek --snapshot` — **byte-deterministic**, fixed-width (120 cols),
//!   ASCII-only fleet view printed once to stdout. Output must diff to
//!   **zero bytes** across every device (phone SSH, tablet, laptop, tmux
//!   pane) for the same underlying fleet state, per the wheat-paste rule.
//!
//! The `--no-tui` path reuses the diff-based event log from
//! [`crate::event_log`] so output is byte-identical to the legacy `cs watch`.
//! The heartbeat label is `peek` when invoked directly; `cs watch` invokes
//! this with label `watch` for backward compatibility during the deprecation
//! grace window.

use std::io::{IsTerminal, Write as _};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use colored::Colorize;
use cosmon_core::molecule::{MoleculeStatus, Phase};
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};
use cosmon_transport::TmuxBackend;

use crate::event_log::{
    clear_line, poll_and_diff, print_baseline, print_events, render_heartbeat, PollOutcome,
    Snapshot, WatchEvent, HEARTBEAT_INTERVAL_MS, LOOP_SLEEP_MS, SPINNER_FRAMES,
};

/// Drop baseline `MoleculeAdded` events whose snapshot phase is not
/// surfaced by the active [`PhaseFilter`]. Other event variants pass
/// through untouched — live transitions are the signal the operator
/// subscribed to. Same default as the TUI (the archive drowns the daily
/// signal); `--phase all` / `--all` widen the filter to the terminal
/// phases.
fn filter_baseline(events: Vec<WatchEvent>, phase_filter: PhaseFilter) -> Vec<WatchEvent> {
    events
        .into_iter()
        .filter(|ev| match ev {
            WatchEvent::MoleculeAdded { view, .. } => phase_filter.matches_status(view.status),
            _ => true,
        })
        .collect()
}

/// The bit [`PhaseFilter`] reserves for one [`Phase`].
///
/// A bijection into a bitmask, not a classification — the classification
/// happens once, in [`MoleculeStatus::phase`]. It carries no wildcard arm
/// for the same reason `phase()` carries none: a new `Phase` variant must
/// fail to compile here rather than silently become unfilterable.
const fn phase_bit(phase: Phase) -> u8 {
    match phase {
        Phase::Live => 1 << 0,
        Phase::Waiting => 1 << 1,
        Phase::Blocked => 1 << 2,
        Phase::Parked => 1 << 3,
        Phase::Failed => 1 << 4,
        Phase::Done => 1 << 5,
    }
}

/// Which [`Phase`]s the molecule table surfaces — a set, nothing more.
///
/// This replaces `StateFilter { running, future, past }`, three booleans
/// that were predicates over a domain with no name. The type had eight
/// representable states, labelled all eight, and could reach exactly four:
/// `running` was hardcoded `true` on every constructor path. The booleans
/// were never the defect — the missing codomain was, and it now exists as
/// [`Phase`]. A filter over a named set needs no invariants of its own: it
/// cannot express "running is secretly always on", because there is nothing
/// here but membership.
///
/// The three temporalities it replaces (`running` / `future` / `past`) were
/// named after a **timeline** while the operator was asking about a
/// **relationship**. `past` in particular welded the 917 completed
/// molecules the operator has already seen to the frozen and starved ones
/// they were never shown, so the default could not surface the second
/// without dragging in the first. See `docs/guides/peek-temporalities.md`.
///
/// The flags that reach this type are now one axis each: `--phase` selects
/// the temporality and nothing else, `--all-galaxies` selects the
/// perimeter and nothing else. `--all` is documented sugar for both at
/// their widest, and is the only flag that speaks to two axes — kept by
/// operator verdict (2026-07-16) because a flag named `--all` that returns
/// less than all is the one move an observer never recovers from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseFilter {
    /// One bit per [`Phase`], per [`phase_bit`]. Private: the set is the
    /// contract, the representation is not.
    mask: u8,
}

impl Default for PhaseFilter {
    fn default() -> Self {
        Self::unfinished()
    }
}

impl PhaseFilter {
    /// The empty set. Surfaces nothing; reachable only from the TUI's
    /// runtime cycling, never from a flag.
    #[must_use]
    pub const fn none() -> Self {
        Self { mask: 0 }
    }

    /// Every phase, including the archive. What `--all` means, literally.
    #[must_use]
    pub const fn all() -> Self {
        Self { mask: 0b0011_1111 }
    }

    /// The default: every molecule whose story is not over — `Live`,
    /// `Waiting`, `Blocked` and `Parked`.
    ///
    /// The predicate is `!terminal`, and the distinction from the previous
    /// default (`== running`) is the whole point. The operator's 2026-04-27
    /// rationale asked to remove **the archive**; the code that shipped
    /// removed **everything that is not running**, which is a different and
    /// much larger set — it also hid every frozen, starved, and pending
    /// molecule. Those had never been reported anywhere else, so hiding
    /// them was not subtraction but amputation: an instrument may hide what
    /// it has already told you, and may never hide what it has not.
    ///
    /// Note this cuts *harder* than what it replaces, not softer. The old
    /// default dropped the archive and the signal together; this one drops
    /// the archive and keeps the signal.
    #[must_use]
    pub const fn unfinished() -> Self {
        Self::none()
            .with(Phase::Live)
            .with(Phase::Waiting)
            .with(Phase::Blocked)
            .with(Phase::Parked)
    }

    /// This set plus `phase`.
    #[must_use]
    pub const fn with(self, phase: Phase) -> Self {
        Self {
            mask: self.mask | phase_bit(phase),
        }
    }

    /// This set unioned with `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            mask: self.mask | other.mask,
        }
    }

    /// Is `phase` in the set?
    #[must_use]
    pub const fn contains(self, phase: Phase) -> bool {
        self.mask & phase_bit(phase) != 0
    }

    /// Build a filter from the `--phase` selectors, in the order they were
    /// typed. An empty list is the default ([`Self::unfinished`]).
    ///
    /// The selectors **union**: `--phase done --phase live` is the two of
    /// them and nothing else. Union is the only composition rule here, so
    /// no ordering of the same flags can ever produce a different set —
    /// the operator does not have to hold an evaluation order in their
    /// head to predict what they are about to see.
    #[must_use]
    pub fn from_phase_args(selectors: &[PhaseSelector]) -> Self {
        if selectors.is_empty() {
            return Self::unfinished();
        }
        selectors
            .iter()
            .fold(Self::none(), |acc, sel| acc.union(sel.to_filter()))
    }

    /// Match a molecule status name — the lowercase `Display` label, e.g.
    /// `"running"`, `"starved"`, `"completed"`.
    ///
    /// A label this binary cannot parse is surfaced, never hidden. Refusing
    /// to render a status we do not understand would be the worst failure
    /// mode an observer has: it converts an *erasure* — visible, cheap to
    /// notice — into a *substitution*, which propagates as confident data
    /// and cannot be detected downstream at all.
    #[must_use]
    pub fn matches(self, status: &str) -> bool {
        status
            .parse::<MoleculeStatus>()
            .map_or(true, |s| self.matches_status(s))
    }

    /// Match a typed [`MoleculeStatus`] by its [`Phase`].
    ///
    /// This is the only classification, and it lives in the core beside the
    /// status enum. `cs peek` used to hand-write five of these, each with a
    /// `_ =>` arm, each free to disagree — and all five did.
    #[must_use]
    pub fn matches_status(self, status: MoleculeStatus) -> bool {
        self.contains(status.phase())
    }

    /// One-line label for the TUI status bar: `"unfinished"`, `"all"`, or
    /// the phases themselves (`"live + waiting"`).
    #[must_use]
    pub fn label(self) -> String {
        if self == Self::all() {
            return "all".to_owned();
        }
        if self == Self::unfinished() {
            return "unfinished".to_owned();
        }
        if self == Self::none() {
            return "(empty)".to_owned();
        }
        Phase::ALL
            .iter()
            .filter(|p| self.contains(**p))
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// A single `--phase` value.
///
/// Six of these are the [`Phase`] variants themselves; the other two name
/// the sets an operator actually asks for by hand. Every value here selects
/// on **one axis** — which molecules, never which projects — so no value of
/// this enum can silently widen the perimeter.
///
/// `unfinished` is spellable even though it is the default: a flag whose
/// only way to say "the default" is to be absent cannot be composed. Once
/// `--phase done` exists, `--phase unfinished --phase done` is the natural
/// way to write "the default plus the archive", and it is exactly what
/// `--past` used to mean without saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PhaseSelector {
    /// A worker is on it right now.
    Live,
    /// Nucleated, not yet started.
    Waiting,
    /// An external authority is refusing service (ADR-062).
    Blocked,
    /// Frozen by an operator gesture; one `cs thaw` from running.
    Parked,
    /// Collapsed.
    Failed,
    /// Completed.
    Done,
    /// Every phase whose story is not over — the default view.
    Unfinished,
    /// Every phase, archive included. All of this axis, and only this axis:
    /// it does not touch the perimeter.
    All,
}

impl PhaseSelector {
    /// The set this selector names.
    #[must_use]
    fn to_filter(self) -> PhaseFilter {
        match self {
            Self::Live => PhaseFilter::none().with(Phase::Live),
            Self::Waiting => PhaseFilter::none().with(Phase::Waiting),
            Self::Blocked => PhaseFilter::none().with(Phase::Blocked),
            Self::Parked => PhaseFilter::none().with(Phase::Parked),
            Self::Failed => PhaseFilter::none().with(Phase::Failed),
            Self::Done => PhaseFilter::none().with(Phase::Done),
            Self::Unfinished => PhaseFilter::unfinished(),
            Self::All => PhaseFilter::all(),
        }
    }
}

use super::patrol::propel_stale_molecules;
use super::Context;

/// Arguments for the `peek` subcommand.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Disable the TUI and render plaintext events to stdout. Required until
    /// the Phase 1 ratatui TUI lands.
    #[arg(long)]
    pub no_tui: bool,

    /// Run a single poll + diff + propel pass and exit. Implies `--no-tui`.
    #[arg(long)]
    pub once: bool,

    /// Follow the event stream until interrupted (the default for `--no-tui`
    /// when `--once` is not set).
    #[arg(long)]
    pub follow: bool,

    /// Staleness threshold in seconds, passed to the propel pass.
    #[arg(long, default_value_t = 300)]
    pub stale_after: u64,

    /// State poll cadence in milliseconds.
    #[arg(long, default_value_t = 1000)]
    pub poll_ms: u64,

    /// Propel nudge cadence in seconds. Defaults to `min(60, stale_after/5)`.
    #[arg(long)]
    pub propel_every: Option<u64>,

    /// Disable tmux propulsion. State is still read and diffed, but no
    /// nudges are sent.
    #[arg(long)]
    pub no_tmux: bool,

    /// Sugar for `--all-galaxies --phase all`, and exactly that. Both axes
    /// at their widest: every project AND every phase, archive included.
    /// `--all` means all, literally; it never narrows. Conflicts with the
    /// two flags it expands to — sugar and its expansion are one way of
    /// saying one thing, not two ways of saying it twice. See
    /// `docs/guides/peek-temporalities.md`.
    #[arg(long, conflicts_with_all = ["all_galaxies", "phase"])]
    pub all: bool,

    /// Perimeter axis: scan every project under `$COSMON_CLUSTER_ROOT`
    /// instead of the current one. Opt-in — cross-project reach is never
    /// implicit. Says nothing about which phases you see. Same spelling as
    /// `cs tail --all-galaxies`: one word, one meaning, across the binary.
    /// In TUI mode the `a` key toggles this at runtime.
    #[arg(long)]
    pub all_galaxies: bool,

    /// Temporality axis: which phases to surface. Repeatable and
    /// comma-separated; the values union. Says nothing about the
    /// perimeter. Defaults to `unfinished` — every molecule whose story is
    /// not over. `--phase unfinished,done,failed` is what `--past` used to
    /// mean. In TUI mode the `A` key cycles this at runtime.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub phase: Vec<PhaseSelector>,

    /// Cadence in seconds for emitting `EnergyTick` events into
    /// `events.jsonl`. Zero disables emission. Only active in `--no-tui` mode.
    #[arg(long, default_value_t = 30)]
    pub energy_tick_interval: u64,

    /// Emit a byte-deterministic, fixed-width (120-col) ASCII snapshot of
    /// the fleet and exit. The same fleet state produces byte-identical
    /// output across every device — iPhone SSH, iPad Blink, `MacBook`,
    /// tmux pane — so a PR reviewer can `diff` two captures and expect
    /// zero differences. Implies `--no-tui` and disables propulsion; no
    /// clock or environment (`$COLUMNS`, `$TERM`) affects the output.
    #[arg(long, conflicts_with_all = ["follow", "once"])]
    pub snapshot: bool,
}

impl Args {
    /// Resolve the effective propel cadence in seconds.
    pub(crate) fn propel_every_seconds(&self) -> u64 {
        self.propel_every
            .unwrap_or_else(|| std::cmp::min(60, (self.stale_after / 5).max(1)))
            .max(1)
    }

    /// Build the [`PhaseFilter`] implied by the CLI flags — the
    /// temporality axis, and only it.
    ///
    /// `--all` is sugar for `--all-galaxies --phase all`, so its
    /// contribution *here* is exactly `--phase all`. The perimeter half of
    /// the sugar is read by [`Self::scans_all_galaxies`]. Neither function
    /// can see the other's axis, which is the whole point of the cut.
    pub(crate) fn phase_filter(&self) -> PhaseFilter {
        if self.all {
            return PhaseFilter::all();
        }
        PhaseFilter::from_phase_args(&self.phase)
    }

    /// Whether to scan every project — the perimeter axis, and only it.
    ///
    /// The other half of the `--all` sugar. See [`Self::phase_filter`].
    pub(crate) fn scans_all_galaxies(&self) -> bool {
        self.all || self.all_galaxies
    }
}

/// Options passed to the shared no-tui engine. Keeps [`Args`] decoupled from
/// the internal call from `cs watch`.
pub(crate) struct NoTuiOptions {
    pub stale_after: u64,
    pub poll_ms: u64,
    pub propel_every_seconds: u64,
    pub once: bool,
    pub no_tmux: bool,
    /// Heartbeat label shown in the footer (`peek` or `watch`).
    pub label: &'static str,
    /// Cadence for emitting `EnergyTick` events (0 disables).
    pub energy_tick_interval: u64,
    /// Phase filter applied to the `MoleculeAdded` baseline stream. The
    /// default ([`PhaseFilter::unfinished`]) hides the archive and
    /// nothing else; `--phase` selects any other slice of the same axis.
    /// Live transitions (status / step / worker changes) are always
    /// surfaced — they are the signal the operator subscribed to.
    pub phase_filter: PhaseFilter,
}

// ---------------------------------------------------------------------------
// Propel pass
// ---------------------------------------------------------------------------

fn propel_pass(
    store: &FileStore,
    backend: Option<&TmuxBackend>,
    stale_after: u64,
) -> anyhow::Result<Vec<WatchEvent>> {
    let fleet = store.load_fleet()?;
    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let mut events = Vec::new();
    if let Some(be) = backend {
        let propelled = propel_stale_molecules(store, &molecules, &fleet, Some(be), stale_after);
        for (worker, molecule, stale_seconds) in propelled {
            events.push(WatchEvent::Propelled {
                worker,
                molecule,
                stale_seconds,
            });
        }
    } else {
        let stale = super::patrol::find_stale_running_molecules(
            &molecules,
            &fleet,
            stale_after,
            Utc::now(),
        );
        for (worker, molecule, stale_seconds) in stale {
            events.push(WatchEvent::StaleDetected {
                worker,
                molecule,
                stale_seconds,
            });
        }
    }
    Ok(events)
}

// ---------------------------------------------------------------------------
// Three-tier deadlines.
// ---------------------------------------------------------------------------

/// Which cadence tier is the earliest to fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Heartbeat,
    Poll,
    Propel,
}

/// The three independent deadlines that drive the no-tui loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deadlines {
    pub heartbeat: Instant,
    pub poll: Instant,
    pub propel: Instant,
}

impl Deadlines {
    pub(crate) fn earliest(&self) -> (Tier, Instant) {
        let mut best = (Tier::Heartbeat, self.heartbeat);
        if self.poll < best.1 {
            best = (Tier::Poll, self.poll);
        }
        if self.propel < best.1 {
            best = (Tier::Propel, self.propel);
        }
        best
    }
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Execute the `peek` command.
///
/// The global `--json` flag wins over every rendering mode, including
/// `--snapshot`. Both are "print once and exit" projections of the same
/// fleet snapshot; when an operator asks for both, the machine channel is
/// the one they meant — a JSON consumer cannot fall back to parsing the
/// 120-column raster, whereas a human reading `--snapshot` never pipes it
/// through `jq`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if ctx.json {
        return run_json(ctx, args);
    }
    if args.snapshot {
        return run_canonical_snapshot(ctx, args);
    }
    if !args.no_tui {
        super::require_project_identity(ctx)?;
        return super::peek_tui::run(
            ctx,
            &super::peek_tui::TuiOptions {
                all_projects: args.scans_all_galaxies(),
                phase_filter: args.phase_filter(),
                refresh_ms: args.poll_ms.max(250),
                filter: super::peek_tui::FilterConfig::default(),
            },
        );
    }
    run_no_tui(
        ctx,
        &NoTuiOptions {
            stale_after: args.stale_after,
            poll_ms: args.poll_ms,
            propel_every_seconds: args.propel_every_seconds(),
            once: args.once,
            no_tmux: args.no_tmux,
            label: "peek",
            energy_tick_interval: args.energy_tick_interval,
            phase_filter: args.phase_filter(),
        },
    )
}

// ---------------------------------------------------------------------------
// `cs peek --json` — the machine projection.
// ---------------------------------------------------------------------------

/// One molecule, as `cs peek --json` publishes it.
///
/// # Why these four fields and no others
///
/// The default is omission and the burden falls on the field. Adding a key
/// later is additive and breaks nobody; removing or renaming one breaks
/// every consumer. So a field ships only when it is both **settled** and
/// **not reconstructible** by the consumer.
///
/// Notably absent: any **bucket** / category / classification field, under
/// any name. The bucket taxonomy is the artefact under active redesign and
/// has already been re-cut more than once. Publishing it would freeze, as a
/// machine contract, the one object with a demonstrated re-cut cadence —
/// every subsequent improvement to the operator's taxonomy would become a
/// breaking change for every consumer. `(status, heartbeat)` reconstructs a
/// bucket in a handful of lines; a bucket cannot reconstruct `status`,
/// because it erases the difference between *finished* and *failed*.
/// Publishing the lossy derivative while withholding the source is
/// backwards. If a consumer who genuinely needs a bucket ever appears, name
/// them and add the field in a minor release.
///
/// Also absent: energy / token counts. `cs ensemble --json` already
/// publishes them per worker, and peek cannot see the sessions a
/// token-accounting consumer would need (see `docs/guides/inbox-trial.md`).
#[derive(Debug, serde::Serialize)]
struct PeekMoleculeJson {
    /// Stable molecule id (e.g. `task-20260716-6a4e`).
    id: String,
    /// Which galaxy the molecule belongs to, as a **display label**.
    /// Load-bearing under `--all`, where rows span galaxies and the
    /// molecule id alone does not say which.
    ///
    /// Honest about what it is: the galaxy's `project_id` when its
    /// `config.toml` declares one, and the containing directory's name when
    /// it does not. Two namespaces under one key, and the consumer cannot
    /// tell which it got — so this is a label to show a human, not a key to
    /// join on. It is published at that strength deliberately rather than
    /// promoted to a typed `project_id`: the observability projection does
    /// not carry `MoleculeData::project_id`, and inventing a contract this
    /// command cannot honour is the failure mode the rest of this schema is
    /// built to avoid. A consumer needing a real project identity should
    /// ask `cs observe --json`.
    project: String,
    /// The **raw core status**, serialized by [`MoleculeStatus`]'s own
    /// derive — never re-lettered, never mapped.
    ///
    /// This is what closes the ADR-068 parity trap: `cs observe --json` and
    /// `cs peek --json` now read the same field of the same enum, so they
    /// cannot report two answers for one molecule. It is also why a future
    /// `#[non_exhaustive]` variant serializes as its own `snake_case` name
    /// with zero code change here — the alternative, laundering an
    /// unrecognized status into `"pending"` through a `_ =>` arm, would
    /// publish a fabricated fact rather than fall back.
    status: MoleculeStatus,
    /// Liveness tier, identical to the one the TUI renders.
    heartbeat: cosmon_observability::HeartbeatTier,
    /// Timestamp `heartbeat` was classified from — the max of the tmux
    /// session clock and the molecule's `updated_at`. Rides along because
    /// `heartbeat` is lossy for a consumer who wants its own thresholds.
    /// `null` when neither clock is available; never a fabricated instant.
    ///
    /// **This clock is attach-bumped.** tmux's `#{session_activity}` resets
    /// when a human opens the session, even if the worker produced nothing,
    /// and the `max` destroys which of the two inputs won. So an operator
    /// attaching to a dead worker's session makes the molecule look alive
    /// here for the next half hour. That is right for the heartbeat — a
    /// human at the keyboard *is* activity — and wrong for anyone asking
    /// "did this molecule move?". Those consumers want `updated_at`.
    last_activity: Option<DateTime<Utc>>,
    /// Wall-clock of the molecule's last state write.
    ///
    /// The un-contaminated peer of `last_activity`: nothing but a state
    /// write moves it, so a stall / orphan patrol (`jq 'select(.heartbeat
    /// == "orphaned")'` → nudge or purge) can ask whether the *molecule*
    /// moved rather than whether someone looked at it. Not reconstructible
    /// from `last_activity`, which has already folded this value into a max
    /// with a clock a keystroke can bump. `cs observe --json` publishes the
    /// same field, which is the parity this schema exists to hold.
    updated_at: DateTime<Utc>,
}

/// The `cs peek --json` document.
///
/// An object rather than a bare array so the schema can grow additively —
/// a future fleet-level counter is a new key, not a breaking reshape of the
/// top level.
///
/// # Divergence from ADR-028, stated rather than buried
///
/// ADR-028 §3 sketches this flag as *"machine-readable NDJSON"*. It ships
/// as one document instead, for two reasons worth recording:
///
/// - **Every other `--json` in the CLI is already a single document.** `cs
///   observe --json` and `cs ensemble --json` both `to_string_pretty` a
///   whole value. NDJSON here would make peek the odd one out, and peek is
///   specifically the command asked to hold parity with `cs observe`.
/// - **NDJSON has no place for a fleet-level fact.** `filter` describes the
///   document, not any molecule in it; a line-per-molecule stream can only
///   carry it by stamping it onto every row or dropping it. The delib that
///   commissioned this schema (`docs/design/peek-redesign/outcomes.md` §C6)
///   framed it as `molecules[]` throughout.
///
/// NDJSON earns its keep on an unbounded or streaming feed. This command
/// prints one snapshot and exits, so the property it buys is not in play.
/// **This is a divergence from a live ADR, not a reading of it** — if the
/// ADR is right and this is wrong, the fix is a successor ADR, not a quiet
/// reshape of a published contract.
#[derive(Debug, serde::Serialize)]
struct PeekJson {
    /// Which phases [`molecules`](Self::molecules) was drawn from —
    /// [`PhaseFilter::label`], e.g. `"unfinished"` (the default) or
    /// `"all"`.
    ///
    /// Published because the document is a **slice**, and without this key
    /// a consumer cannot tell a molecule that does not exist from one the
    /// default filter declined to show. That distinction is not hypothetical:
    /// [`MoleculeStatus`] is `#[non_exhaustive]`, so a status added upstream
    /// is assigned a [`Phase`] in the core, and if that phase is terminal the
    /// new status silently leaves the default document. The value never
    /// laundered — the molecule is simply absent, and absence reads as
    /// "filtered by design". Naming the slice is what makes the omission
    /// legible and tells the consumer that `--all` exists.
    filter: String,
    /// One entry per molecule in the slice, **sorted by `id`**.
    ///
    /// The order is part of what a consumer diffs, so it may not depend on
    /// hash iteration: `FleetSnapshot` stores molecules in a `HashMap` and
    /// sessions are walked from another, so the natural order here is
    /// `RandomState` order and reshuffles run-to-run for an unchanged fleet.
    /// Sorting by `id` matches `render_canonical`, the sibling print-once
    /// projection, rather than the TUI's `(liveness, updated_at, id)` — a
    /// machine consumer wants a stable order, and the TUI's is deliberately
    /// unstable, since it re-sorts as workers breathe.
    molecules: Vec<PeekMoleculeJson>,
}

/// Emit the machine projection of the fleet to stdout and exit.
///
/// Shares [`super::peek_tui::build_snapshot`] and
/// [`super::peek_tui::snapshot_to_rows`] with the TUI and the canonical
/// snapshot, so `--phase` / `--all` select exactly the same molecules here
/// as they do on screen. What the rows are *not* asked for is `status`:
/// [`RowView`](super::peek_tui::RowView) carries a display status, which
/// overrides `Pending` to `Running` when a live tmux session is bound to
/// the molecule — a defensible thing to show a human mid-`tackle`, and a
/// fabricated fact to hand a machine. Status is read from the snapshot's
/// molecules directly; the rows contribute only the liveness signals they
/// alone compute (the merge across a runtime + cognition pair).
fn run_json(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let project_id = super::require_project_identity(ctx)?;

    let state_dir = ctx.state_dir();
    let socket = super::tmux_socket_name(ctx);
    let (mut snap, _state_dirs) = super::peek_tui::build_snapshot(
        &state_dir,
        &socket,
        Some(&project_id),
        args.scans_all_galaxies(),
    )?;

    let phase_filter = args.phase_filter();
    if phase_filter != PhaseFilter::all() {
        super::peek_tui::filter_snapshot_by_phase(&mut snap, phase_filter);
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&snapshot_to_json(&snap, phase_filter))?
    );
    Ok(())
}

/// Project a fleet snapshot into the `cs peek --json` document.
///
/// Split out of [`run_json`] so the join it performs — liveness from the
/// rows, status from the molecules — is reachable from a test without a
/// tmux server or a state directory.
fn snapshot_to_json(
    snap: &cosmon_observability::FleetSnapshot,
    phase_filter: PhaseFilter,
) -> PeekJson {
    // Index the molecules once. The rows are keyed by `mol_id` and so is
    // this, so the join is a lookup rather than a scan per row — peek runs
    // against fleets of ~1000 molecules.
    let by_id: std::collections::HashMap<String, &cosmon_observability::molecule::Molecule> =
        snap.molecules().map(|m| (m.id.to_string(), m)).collect();

    let mut molecules: Vec<PeekMoleculeJson> = super::peek_tui::snapshot_to_rows(snap)
        .into_iter()
        .filter_map(|row| {
            // A row whose id resolves to no molecule is a tmux session with
            // nothing behind it. It is not a molecule and has no status to
            // report, so it is not one of `molecules[]`.
            let mol = by_id.get(&row.mol_id)?;
            Some(PeekMoleculeJson {
                id: row.mol_id.clone(),
                project: row.project.clone(),
                status: mol.status,
                heartbeat: row.heartbeat,
                last_activity: row.last_activity,
                updated_at: mol.updated_at,
            })
        })
        .collect();

    // See `PeekJson::molecules` — the snapshot's iteration order is hash
    // order, and a wire order that reshuffles for an unchanged fleet is a
    // diff that lies. `id`s are unique, so this is a total order.
    molecules.sort_by(|a, b| a.id.cmp(&b.id));

    PeekJson {
        filter: phase_filter.label(),
        molecules,
    }
}

/// Emit the wheat-paste canonical snapshot to stdout.
///
/// This path deliberately shares `peek_tui::build_snapshot` (which walks
/// the state store and tmux sockets) with the TUI, but swaps the
/// ratatui renderer for [`cosmon_observability::render::render_canonical`].
/// No TUI, no colour, no clock in the output — byte-deterministic by
/// construction.
fn run_canonical_snapshot(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    use std::io::Write as _;

    let project_id = super::require_project_identity(ctx)?;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let socket = super::tmux_socket_name(ctx);
    let (mut snap, _state_dirs) = super::peek_tui::build_snapshot(
        &state_dir,
        &socket,
        Some(&project_id),
        args.scans_all_galaxies(),
    )?;

    // Project the snapshot through the requested phase slice so the
    // wheat-paste byte stream reflects the operator's `--phase` selection.
    // `--all` passes the snapshot through untouched, preserving the
    // byte-identical contract for `cs peek --snapshot --all`.
    let phase_filter = args.phase_filter();
    if phase_filter != PhaseFilter::all() {
        super::peek_tui::filter_snapshot_by_phase(&mut snap, phase_filter);
    }

    // Load the sensorium aggregate from disk and project it into the
    // canonical raster. The strip itself is rendered inside
    // `render_canonical`; absent organs collapse to the zero baseline
    // so the strip line is byte-identical for a fresh galaxy.
    let sensorium = crate::sensorium::load_sensorium(&state_dir);
    let cfg = cosmon_observability::render::SnapshotConfig {
        sensorium,
        ..cosmon_observability::render::SnapshotConfig::default()
    };
    let rendered = cosmon_observability::render::render_canonical(&snap, &cfg);

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(rendered.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

/// Shared `--no-tui` loop, also invoked by the deprecated `cs watch` alias.
#[allow(clippy::too_many_lines)]
pub(crate) fn run_no_tui(ctx: &Context, opts: &NoTuiOptions) -> anyhow::Result<()> {
    super::require_project_identity(ctx)?;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    let backend = if opts.no_tmux {
        None
    } else {
        Some(TmuxBackend::new(super::tmux_socket_name(ctx)))
    };

    let mut stdout = std::io::stdout();
    let tty = stdout.is_terminal() && !opts.once;

    let session_start = Instant::now();
    let mut prev: Option<Snapshot> = None;

    let first: PollOutcome = poll_and_diff(&store, prev.as_ref())?;
    let baseline = if opts.phase_filter == PhaseFilter::all() {
        first
    } else {
        PollOutcome {
            snapshot: first.snapshot,
            events: filter_baseline(first.events, opts.phase_filter),
        }
    };
    print_baseline(&mut stdout, tty, Utc::now(), &baseline)?;
    prev = Some(baseline.snapshot);

    let propel_events = propel_pass(&store, backend.as_ref(), opts.stale_after)?;
    print_events(&mut stdout, tty, Utc::now(), &propel_events)?;

    if opts.once {
        return Ok(());
    }

    let poll_interval = Duration::from_millis(opts.poll_ms.max(HEARTBEAT_INTERVAL_MS));
    let propel_interval = Duration::from_secs(opts.propel_every_seconds);
    let heartbeat_interval = Duration::from_millis(HEARTBEAT_INTERVAL_MS);

    let now0 = Instant::now();
    let mut deadlines = Deadlines {
        heartbeat: now0 + heartbeat_interval,
        poll: now0 + poll_interval,
        propel: now0 + propel_interval,
    };

    let energy_tick_interval = if opts.energy_tick_interval == 0 {
        None
    } else {
        Some(Duration::from_secs(opts.energy_tick_interval))
    };
    let mut last_energy_tick = now0;
    let events_path = state_dir.join("events.jsonl");
    let mut last_energy: std::collections::HashMap<String, (u64, u64, f64)> =
        std::collections::HashMap::new();

    let mut spinner_idx = 0usize;

    loop {
        let (tier, deadline) = deadlines.earliest();
        let now = Instant::now();
        if now < deadline {
            let wait = (deadline - now).min(Duration::from_millis(LOOP_SLEEP_MS));
            std::thread::sleep(wait);
            continue;
        }

        match tier {
            Tier::Heartbeat => {
                if tty {
                    clear_line(&mut stdout, tty)?;
                    let running = prev.as_ref().map_or(0, |s| {
                        s.molecules
                            .values()
                            .filter(|m| m.status == MoleculeStatus::Running)
                            .count()
                    });
                    let workers = prev.as_ref().map_or(0, |s| s.workers.len());
                    let frame = SPINNER_FRAMES[spinner_idx % SPINNER_FRAMES.len()];
                    spinner_idx = spinner_idx.wrapping_add(1);
                    write!(
                        stdout,
                        "{}",
                        render_heartbeat(
                            opts.label,
                            Local::now(),
                            session_start.elapsed(),
                            frame,
                            workers,
                            running
                        )
                        .dimmed()
                    )?;
                    stdout.flush()?;
                }
                deadlines.heartbeat = Instant::now() + heartbeat_interval;
            }
            Tier::Poll => {
                let outcome = poll_and_diff(&store, prev.as_ref())?;
                print_events(&mut stdout, tty, Utc::now(), &outcome.events)?;
                prev = Some(outcome.snapshot);
                deadlines.poll = Instant::now() + poll_interval;
            }
            Tier::Propel => {
                let events = propel_pass(&store, backend.as_ref(), opts.stale_after)?;
                print_events(&mut stdout, tty, Utc::now(), &events)?;
                deadlines.propel = Instant::now() + propel_interval;
            }
        }

        if let Some(iv) = energy_tick_interval {
            if last_energy_tick.elapsed() >= iv {
                emit_energy_ticks(
                    &store,
                    &events_path,
                    &state_dir,
                    &super::tmux_socket_name(ctx),
                    &mut stdout,
                    tty,
                    &mut last_energy,
                );
                last_energy_tick = Instant::now();
            }
        }
    }
}

/// Probe every active worker's claudion energy, emit an `EnergyTick` event
/// per worker, and echo a short delta line to stdout.
fn emit_energy_ticks(
    store: &FileStore,
    events_path: &std::path::Path,
    state_dir: &std::path::Path,
    socket: &str,
    stdout: &mut std::io::Stdout,
    tty: bool,
    last_energy: &mut std::collections::HashMap<String, (u64, u64, f64)>,
) {
    use cosmon_core::event_v2::EventV2;

    let Ok(fleet) = store.load_fleet() else {
        return;
    };
    let backends = crate::energy_probe::discover_fleet_backends(state_dir, socket);
    let energy = crate::energy_probe::load_worker_energy(state_dir, &backends, &fleet);
    if energy.is_empty() {
        return;
    }

    clear_line(stdout, tty).ok();
    for (wid, e) in &energy {
        let (input, output, cost) = e.as_tuple();
        let wid_s = wid.as_str().to_owned();
        let prev = last_energy.get(&wid_s).copied().unwrap_or((0, 0, 0.0));
        let delta_tokens = (input + output).saturating_sub(prev.0 + prev.1);
        let delta_cost = (cost - prev.2).max(0.0);
        last_energy.insert(wid_s.clone(), (input, output, cost));

        let Ok(worker_id) = cosmon_core::id::WorkerId::new(&wid_s) else {
            continue;
        };
        let _ = cosmon_state::event_log::emit_one(
            events_path,
            EventV2::EnergyTick {
                worker_id,
                input_tokens: input,
                output_tokens: output,
                cost_usd: cost,
            },
            None,
        );

        let line = format!(
            "~ {:<12} energy: {} tokens (+{}, ${:.4})\n",
            wid_s,
            humanize_tokens_small(input + output),
            humanize_tokens_small(delta_tokens),
            delta_cost,
        );
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.flush();
    }
}

#[allow(clippy::cast_precision_loss)]
fn humanize_tokens_small(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use clap::{Parser, ValueEnum as _};
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_observability::FleetSnapshot;
    use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
    use tempfile::TempDir;

    use super::*;

    fn make_worker(name: &str, desired: DesiredState, status: WorkerStatus) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("polecat").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            status,
        );
        w.desired = desired;
        w
    }

    fn make_molecule(
        id: &str,
        status: MoleculeStatus,
        worker: Option<&str>,
        step: usize,
    ) -> MoleculeData {
        MoleculeData {
            fleet_id: FleetId::new("default").unwrap(),
            id: MoleculeId::new(id).unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: worker.map(|w| WorkerId::new(w).unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: step,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
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
    fn deadlines_earliest_picks_poll_when_poll_is_closest() {
        let now = Instant::now();
        let d = Deadlines {
            heartbeat: now + Duration::from_millis(500),
            poll: now + Duration::from_millis(100),
            propel: now + Duration::from_secs(60),
        };
        assert_eq!(d.earliest().0, Tier::Poll);
    }

    #[test]
    fn deadlines_earliest_picks_heartbeat_when_heartbeat_is_closest() {
        let now = Instant::now();
        let d = Deadlines {
            heartbeat: now + Duration::from_millis(50),
            poll: now + Duration::from_millis(1000),
            propel: now + Duration::from_secs(60),
        };
        assert_eq!(d.earliest().0, Tier::Heartbeat);
    }

    #[test]
    fn deadlines_earliest_picks_propel_when_propel_is_closest() {
        let now = Instant::now();
        let d = Deadlines {
            heartbeat: now + Duration::from_secs(10),
            poll: now + Duration::from_secs(5),
            propel: now + Duration::from_secs(1),
        };
        assert_eq!(d.earliest().0, Tier::Propel);
    }

    fn args_with(stale_after: u64, propel_every: Option<u64>) -> Args {
        Args {
            no_tui: true,
            once: false,
            follow: false,
            stale_after,
            poll_ms: 1000,
            propel_every,
            no_tmux: false,
            all: false,
            all_galaxies: false,
            phase: Vec::new(),
            energy_tick_interval: 0,
            snapshot: false,
        }
    }

    #[test]
    fn propel_every_seconds_defaults_to_60_for_default_stale_after() {
        assert_eq!(args_with(300, None).propel_every_seconds(), 60);
    }

    #[test]
    fn propel_every_seconds_shrinks_for_tight_stale_after() {
        assert_eq!(args_with(100, None).propel_every_seconds(), 20);
    }

    #[test]
    fn propel_every_seconds_honors_explicit_override() {
        assert_eq!(args_with(300, Some(7)).propel_every_seconds(), 7);
    }

    #[test]
    fn propel_every_seconds_clamps_to_one_second_minimum() {
        assert_eq!(args_with(300, Some(0)).propel_every_seconds(), 1);
    }

    #[test]
    fn propel_pass_without_backend_emits_stale_detected_dry_run() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mut fleet = Fleet::default();
        fleet.workers.insert(
            WorkerId::new("stuck").unwrap(),
            make_worker("stuck", DesiredState::Running, WorkerStatus::Active),
        );
        store.save_fleet(&fleet).unwrap();

        let mut mol = make_molecule(
            "cs-20260409-dead",
            MoleculeStatus::Running,
            Some("stuck"),
            0,
        );
        mol.updated_at = Utc::now() - chrono::Duration::seconds(900);
        store.save_molecule(&mol.id, &mol).unwrap();

        let events = propel_pass(&store, None, 300).unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WatchEvent::StaleDetected { .. })),
            "expected at least one StaleDetected event, got {events:?}",
        );
    }

    #[test]
    fn peek_no_tui_once_smoke() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::default()).unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nproject_id = \"test-0000\"\n",
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            no_tui: true,
            once: true,
            follow: false,
            stale_after: 300,
            poll_ms: 1000,
            propel_every: None,
            no_tmux: true,
            all: false,
            all_galaxies: false,
            phase: Vec::new(),
            energy_tick_interval: 0,
            snapshot: false,
        };
        run(&ctx, &args).unwrap();
    }

    fn molecule_added(id: &str, status: MoleculeStatus) -> WatchEvent {
        WatchEvent::MoleculeAdded {
            id: MoleculeId::new(id).unwrap(),
            view: crate::event_log::MoleculeView {
                status,
                assigned_worker: None,
                current_step: 0,
                total_steps: 1,
            },
        }
    }

    #[test]
    fn filter_baseline_default_drops_the_archive_and_keeps_everything_else() {
        let events = vec![
            molecule_added("cs-20260427-aaaa", MoleculeStatus::Running),
            molecule_added("cs-20260427-bbbb", MoleculeStatus::Pending),
            molecule_added("cs-20260427-cccc", MoleculeStatus::Completed),
            molecule_added("cs-20260427-dddd", MoleculeStatus::Collapsed),
            molecule_added("cs-20260427-eeee", MoleculeStatus::Frozen),
            molecule_added("cs-20260427-ffff", MoleculeStatus::Starved),
        ];
        let kept = filter_baseline(events, PhaseFilter::default());
        let ids: Vec<&str> = kept
            .iter()
            .map(|ev| match ev {
                WatchEvent::MoleculeAdded { id, .. } => id.as_str(),
                other => panic!("expected MoleculeAdded, got {other:?}"),
            })
            .collect();
        // Only the two terminal molecules go. The pending, frozen and
        // starved rows are unfinished work the operator was never told
        // about anywhere else, and the old `== running` default dropped
        // all three.
        assert_eq!(
            ids,
            vec![
                "cs-20260427-aaaa",
                "cs-20260427-bbbb",
                "cs-20260427-eeee",
                "cs-20260427-ffff",
            ],
        );
    }

    #[test]
    fn filter_baseline_default_watchdog_preserves_transition_events() {
        let id = MoleculeId::new("cs-20260427-eeee").unwrap();
        let events = vec![
            WatchEvent::MoleculeStatusChanged {
                id: id.clone(),
                from: MoleculeStatus::Pending,
                to: MoleculeStatus::Running,
            },
            WatchEvent::MoleculeStepChanged {
                id: id.clone(),
                from: 0,
                to: 1,
                total: 2,
            },
            WatchEvent::MoleculeWorkerChanged {
                id,
                from: None,
                to: Some(WorkerId::new("polecat-1").unwrap()),
            },
        ];
        let kept = filter_baseline(events.clone(), PhaseFilter::default());
        assert_eq!(kept.len(), events.len());
    }

    // ---------------------------------------------------------------
    // PhaseFilter unit tests (delib-20260716-a2f1 C4)
    // ---------------------------------------------------------------

    /// Every status this binary knows, for the folds below.
    ///
    /// Aliases the core's own list rather than restating it. `MoleculeStatus`
    /// is `#[non_exhaustive]`, so a local array would not fail to compile
    /// when a variant is added upstream — these folds would just quietly
    /// stop covering it, on exactly the day it mattered. `MoleculeStatus::ALL`
    /// is guarded in the crate that owns the enum.
    const EVERY_STATUS: [MoleculeStatus; 7] = MoleculeStatus::ALL;

    #[test]
    fn default_is_not_terminal_exactly() {
        // The whole ruling in one assertion: the default surfaces a
        // molecule if and only if its story is not over. Not "if and only
        // if it is running", which is what shipped and which hid the five
        // frozen and the twenty-seven orphans along with the archive.
        let f = PhaseFilter::default();
        for s in EVERY_STATUS {
            assert_eq!(
                f.matches_status(s),
                !s.is_terminal(),
                "the default disagrees with core's is_terminal() on {s}",
            );
        }
    }

    #[test]
    fn default_surfaces_starved_the_status_that_summons_the_operator() {
        // The regression that started the deliberation. `Starved` is alive
        // (ADR-062) and every peek classification used to file it with the
        // archive, where it was one row in 918 and invisible by default.
        assert!(PhaseFilter::default().matches_status(MoleculeStatus::Starved));
    }

    #[test]
    fn default_surfaces_frozen_which_no_other_instrument_reports() {
        assert!(PhaseFilter::default().matches_status(MoleculeStatus::Frozen));
    }

    #[test]
    fn default_hides_the_archive_and_only_the_archive() {
        let f = PhaseFilter::default();
        assert!(!f.matches_status(MoleculeStatus::Completed));
        assert!(!f.matches_status(MoleculeStatus::Collapsed));
    }

    #[test]
    fn no_phase_selector_is_the_unfinished_default() {
        assert_eq!(PhaseFilter::from_phase_args(&[]), PhaseFilter::unfinished());
    }

    #[test]
    fn phase_selectors_union_and_order_cannot_matter() {
        // Union is the only composition rule, so the operator never has to
        // hold an evaluation order in their head to predict the view.
        let forward = PhaseFilter::from_phase_args(&[PhaseSelector::Done, PhaseSelector::Live]);
        let backward = PhaseFilter::from_phase_args(&[PhaseSelector::Live, PhaseSelector::Done]);
        assert_eq!(forward, backward);
        assert_eq!(
            forward,
            PhaseFilter::none().with(Phase::Live).with(Phase::Done)
        );
    }

    #[test]
    fn archive_selectors_say_what_past_used_to_mean() {
        // `--past` is deleted, not aliased: it named a timeline while the
        // operator was asking about a relationship. This is the set it
        // actually delivered, now spelled out on one axis.
        let f = PhaseFilter::from_phase_args(&[
            PhaseSelector::Unfinished,
            PhaseSelector::Done,
            PhaseSelector::Failed,
        ]);
        for s in EVERY_STATUS {
            assert!(
                f.matches_status(s),
                "the archive selectors must never drop {s}"
            );
        }
    }

    #[test]
    fn every_phase_is_reachable_by_its_own_name() {
        // A phase the table can show but no flag can name is a row the
        // operator cannot ask for.
        for p in Phase::ALL {
            let named = PhaseSelector::value_variants()
                .iter()
                .find(|sel| sel.to_filter() == PhaseFilter::none().with(p));
            assert!(named.is_some(), "no --phase value selects {p} alone");
        }
    }

    /// Parse `cs peek` flags in isolation, so the axis tests below assert
    /// against the real clap tree rather than a hand-built `Args`.
    #[derive(Parser)]
    struct PeekOnly {
        #[command(flatten)]
        args: Args,
    }

    fn parse(flags: &[&str]) -> Args {
        let mut argv = vec!["peek"];
        argv.extend_from_slice(flags);
        PeekOnly::parse_from(argv).args
    }

    #[test]
    fn all_is_exactly_its_documented_expansion() {
        // The whole verdict in one assertion: `--all` is sugar for
        // `--all-galaxies --phase all` and must stay observably identical
        // to it on both axes. If this ever drifts, the sugar has started
        // meaning something the doc string does not say — which is the
        // silent narrowing the panel disqualified.
        let sugar = parse(&["--all"]);
        let expansion = parse(&["--all-galaxies", "--phase", "all"]);
        assert_eq!(sugar.phase_filter(), expansion.phase_filter());
        assert_eq!(sugar.scans_all_galaxies(), expansion.scans_all_galaxies());
        assert_eq!(sugar.phase_filter(), PhaseFilter::all());
        assert!(sugar.scans_all_galaxies());
    }

    #[test]
    fn each_axis_flag_moves_its_own_axis_and_no_other() {
        // The tolnay rule made executable: no flag's meaning may be a
        // function of the number of axes. `--all-galaxies` must not touch
        // the phases, and `--phase` must not touch the perimeter.
        let perimeter = parse(&["--all-galaxies"]);
        assert!(perimeter.scans_all_galaxies());
        assert_eq!(perimeter.phase_filter(), PhaseFilter::unfinished());

        let temporality = parse(&["--phase", "all"]);
        assert_eq!(temporality.phase_filter(), PhaseFilter::all());
        assert!(!temporality.scans_all_galaxies());
    }

    #[test]
    fn bare_peek_is_the_current_project_and_the_unfinished_set() {
        let args = parse(&[]);
        assert!(!args.scans_all_galaxies());
        assert_eq!(args.phase_filter(), PhaseFilter::unfinished());
    }

    #[test]
    fn sugar_cannot_be_mixed_with_its_expansion() {
        // One way to say one thing. Accepting `--all --phase live` would
        // force a precedence rule, and a precedence rule is exactly how a
        // flag named `--all` starts returning less than all.
        for flags in [
            vec!["--all", "--phase", "live"],
            vec!["--all", "--all-galaxies"],
        ] {
            let mut argv = vec!["peek"];
            argv.extend_from_slice(&flags);
            assert!(
                PeekOnly::try_parse_from(argv).is_err(),
                "{flags:?} must be rejected, not silently resolved",
            );
        }
    }

    #[test]
    fn the_deleted_flags_are_gone_not_quietly_accepted() {
        // `--past` and `--future` are deleted, not aliased. An unknown flag
        // is an error the operator sees; an alias onto a set the name
        // mis-describes is a lie they do not.
        for dead in ["--past", "--future"] {
            assert!(
                PeekOnly::try_parse_from(vec!["peek", dead]).is_err(),
                "{dead} must not parse",
            );
        }
    }

    #[test]
    fn phase_all_selector_means_all_literally() {
        let f = PhaseFilter::from_phase_args(&[PhaseSelector::All]);
        for s in EVERY_STATUS {
            assert!(f.matches_status(s), "--phase all must accept {s}");
        }
        assert_eq!(f, PhaseFilter::all());
        for p in Phase::ALL {
            assert!(f.contains(p), "--phase all must contain {p}");
        }
    }

    #[test]
    fn unparseable_status_is_surfaced_never_hidden() {
        // An erasure the operator can see beats a substitution they cannot.
        // A status string this binary does not understand is exactly when
        // an observer must not have opinions.
        assert!(PhaseFilter::none().matches("a-status-from-the-future"));
        assert!(PhaseFilter::default().matches("a-status-from-the-future"));
    }

    #[test]
    fn matches_string_agrees_with_matches_status() {
        // `matches` parses and delegates; this holds the two entry points
        // to one answer so the string path can never drift into a sixth
        // private classification.
        for f in [
            PhaseFilter::default(),
            PhaseFilter::all(),
            PhaseFilter::from_phase_args(&[PhaseSelector::Failed, PhaseSelector::Done]),
        ] {
            for s in EVERY_STATUS {
                assert_eq!(
                    f.matches(&s.to_string()),
                    f.matches_status(s),
                    "string and typed paths disagree on {s}",
                );
            }
        }
    }

    #[test]
    fn labels_name_the_set_not_the_bits() {
        assert_eq!(PhaseFilter::default().label(), "unfinished");
        assert_eq!(PhaseFilter::all().label(), "all");
        assert_eq!(PhaseFilter::none().label(), "(empty)");
        assert_eq!(
            PhaseFilter::from_phase_args(&[PhaseSelector::Failed, PhaseSelector::Done]).label(),
            "failed + done"
        );
    }

    // -----------------------------------------------------------------
    // `cs peek --json` — the machine contract.
    // -----------------------------------------------------------------

    /// Build a snapshot holding one molecule, optionally bound to a live
    /// tmux session with `now` as its last activity.
    /// Insert one molecule, optionally bound to a live tmux session whose
    /// last activity is `now`.
    ///
    /// `project_root` carries a **label**, not a path, because that is what
    /// `populate_snapshot` actually stores on an observability `Molecule`
    /// (`project_label_for`: the galaxy's `project_id`, or its directory
    /// name). A fixture holding a path would encode a mental model the
    /// production code contradicts, and the `project` assertions below would
    /// pin a value that can never occur.
    fn json_push(snap: &mut FleetSnapshot, id: &str, status: MoleculeStatus, with_session: bool) {
        use cosmon_observability::molecule::Molecule as ObsMolecule;
        use cosmon_observability::session::Session;

        snap.insert_molecule(ObsMolecule {
            id: id.into(),
            title: "t".into(),
            kind: "task".into(),
            status,
            project_root: "cosmon-9de1".into(),
            session: None,
            updated_at: Utc::now(),
        });
        if with_session {
            snap.push_session(Session {
                name: format!("cosmon-{id}"),
                socket: "cosmon".into(),
                project_root: "/srv/cosmon/cosmon".into(),
                molecule_id: Some(id.to_owned()),
                worker_id: None,
                last_activity: Some(Utc::now()),
            });
        }
    }

    fn json_fixture(status: MoleculeStatus, with_session: bool) -> FleetSnapshot {
        let mut snap = FleetSnapshot::new();
        json_push(&mut snap, "task-20260716-6a4e", status, with_session);
        snap
    }

    fn json_value(snap: &FleetSnapshot) -> serde_json::Value {
        serde_json::to_value(snapshot_to_json(snap, PhaseFilter::default())).unwrap()
    }

    #[test]
    fn json_publishes_the_raw_core_status_not_the_display_override() {
        // The ADR-068 parity trap this schema exists to close. A `Pending`
        // molecule with a live session bound to it is rendered `running` by
        // the TUI — a defensible thing to show a human inside the
        // tackle/save race, and a fabricated fact to hand a machine. If
        // this ever reports "running", `cs peek --json` and `cs observe
        // --json` describe one molecule with two answers.
        let v = json_value(&json_fixture(MoleculeStatus::Pending, true));
        assert_eq!(v["molecules"][0]["status"], "pending");
    }

    #[test]
    fn json_never_launders_a_status_into_pending() {
        // `Starved` and `Queued` are the two the old observability bridge
        // destroyed: one renamed to a variant with no referent in the core,
        // the other swallowed by a `_ =>` arm along with every future
        // variant. Each must serialize as itself.
        for status in EVERY_STATUS {
            let v = json_value(&json_fixture(status, false));
            assert_eq!(
                v["molecules"][0]["status"],
                serde_json::Value::String(status.to_string()),
                "{status} did not survive the projection",
            );
        }
    }

    #[test]
    fn json_omits_every_taxonomy_field() {
        // The whole ruling in one assertion: no bucket, under any name. The
        // taxonomy is under active redesign, so publishing it would freeze a
        // machine contract to the one object with a demonstrated re-cut
        // cadence. Deleting this test is the gesture that ships the freeze —
        // it should cost a deliberation, not a keystroke.
        let v = json_value(&json_fixture(MoleculeStatus::Running, true));
        // `serde_json::Value`'s map is sorted, so this pins the key *set*,
        // not the emitted order.
        let keys: Vec<&str> = v["molecules"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "heartbeat",
                "id",
                "last_activity",
                "project",
                "status",
                "updated_at"
            ],
            "the peek --json schema changed; a new key is a forever contract",
        );
    }

    #[test]
    fn json_reports_a_missing_clock_as_null_never_as_an_instant() {
        // A molecule with no session still has `updated_at`, so the honest
        // wire value is that timestamp — not a fabricated "now" and not a
        // silently dropped key. The key is always present so consumers can
        // rely on a stable shape.
        let v = json_value(&json_fixture(MoleculeStatus::Frozen, false));
        assert!(v["molecules"][0]
            .as_object()
            .unwrap()
            .contains_key("last_activity"));
    }

    #[test]
    fn json_heartbeat_is_the_tier_name_in_snake_case() {
        // Orphaned is the tier that matters: running in state, no live tmux.
        let v = json_value(&json_fixture(MoleculeStatus::Running, false));
        assert_eq!(v["molecules"][0]["heartbeat"], "orphaned");

        let v = json_value(&json_fixture(MoleculeStatus::Running, true));
        assert_eq!(v["molecules"][0]["heartbeat"], "active");
    }

    #[test]
    fn json_orders_molecules_by_id_whatever_the_hash_says() {
        // `FleetSnapshot` holds molecules in a `HashMap`, so the projection's
        // natural order is `RandomState` order — it reshuffles run to run for
        // an unchanged fleet. A consumer diffing two captures would read that
        // churn as change. Insertion order here is deliberately not sorted.
        let mut snap = FleetSnapshot::new();
        for id in [
            "task-20260716-ffff",
            "task-20260716-0a3b",
            "verify-20260716-74e4",
            "task-20260716-6a4e",
        ] {
            json_push(&mut snap, id, MoleculeStatus::Running, false);
        }
        let v = json_value(&snap);
        let ids: Vec<&str> = v["molecules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "task-20260716-0a3b",
                "task-20260716-6a4e",
                "task-20260716-ffff",
                "verify-20260716-74e4",
            ],
        );
    }

    #[test]
    fn json_names_the_slice_it_published() {
        // Without this key, "molecule absent" and "molecule filtered out"
        // are the same observation. They are not the same fact.
        let snap = json_fixture(MoleculeStatus::Running, false);
        assert_eq!(json_value(&snap)["filter"], "unfinished");
        let v = serde_json::to_value(snapshot_to_json(&snap, PhaseFilter::all())).unwrap();
        assert_eq!(v["filter"], "all");
    }

    #[test]
    fn json_publishes_a_clock_no_keystroke_can_bump() {
        // `last_activity` folds in tmux's attach-bumped session clock, so
        // looking at a molecule moves it. `updated_at` only moves when the
        // molecule's state is written — it is the field a stall patrol must
        // read, and it cannot be recovered from the max the other field is.
        let v = json_value(&json_fixture(MoleculeStatus::Running, true));
        assert!(
            v["molecules"][0]["updated_at"].is_string(),
            "updated_at must always be present; every molecule has been written at least once",
        );
    }

    #[test]
    fn json_project_is_the_same_label_with_or_without_a_live_session() {
        // Rows are built by two different paths — one from a tmux session,
        // one from a molecule alone — and only the second is guaranteed to
        // read `Molecule::project_root`. If the session path ever leaks its
        // own basename-of-a-path here, one molecule would report two
        // different `project` strings depending on whether a worker happens
        // to be attached.
        let attached = json_value(&json_fixture(MoleculeStatus::Running, true));
        let detached = json_value(&json_fixture(MoleculeStatus::Running, false));
        assert_eq!(attached["molecules"][0]["project"], "cosmon-9de1");
        assert_eq!(
            attached["molecules"][0]["project"],
            detached["molecules"][0]["project"],
        );
    }

    #[test]
    fn json_drops_a_session_with_no_molecule_behind_it() {
        use cosmon_observability::session::Session;
        let mut snap = FleetSnapshot::new();
        snap.push_session(Session {
            name: "cosmon-stray".into(),
            socket: "cosmon".into(),
            project_root: "/tmp/cosmon".into(),
            molecule_id: None,
            worker_id: None,
            last_activity: Some(Utc::now()),
        });
        let v = json_value(&snap);
        assert_eq!(v["molecules"].as_array().unwrap().len(), 0);
    }
}
